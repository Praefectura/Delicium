//! Fixed-step simulation: asset pool, instances, interactions and pose
//! evaluation.
//!
//! The simulation owns a private [`bevy_ecs`] world and a single 60 Hz clock.
//! Nothing in this module depends on a window, a GPU device or the file
//! system, so every public entry point is callable from a headless test.
//!
//! # Ticks
//!
//! [`Simulation::advance`] consumes whole [`FIXED_DT`] steps, at most
//! [`MAX_SUBSTEPS`] per call. One tick runs six systems in a fixed order:
//!
//! 1. the social step: deterministically pair opted-in neighbours and pause
//!    their autonomous walking without touching user drag suspension;
//! 2. the wander step: every instance advances its idle/walk state machine
//!    inside [`Simulation::wander_bounds`] and may write its own position;
//! 3. an interaction snapshot: every instance finds its nearest neighbour
//!    inside [`INTERACTION_RADIUS`] and eases its reaction weight;
//! 4. the poke reaction step: every active poke reaction ages by one step
//!    and turns its instance toward the poke point;
//! 5. the opt-in attention step: a deterministic presentation pass that
//!    resolves the pointer, desktop edges or the idle variant; it returns
//!    before touching the world while attention is disabled (the default);
//! 6. the pose evaluation: animation sampling (2D parameters or 3D joint
//!    locals) plus the reaction, social or attention drive.
//!
//! The walk precedes the snapshot so it and the same frame's render agree on
//! where every instance is.
//!
//! Backlog beyond the step budget is dropped and counted in
//! [`TickReport::dropped_steps`]; the sub-step residual stays available
//! through [`Simulation::alpha`] so the renderer can interpolate.
//!
//! # Parallelism
//!
//! Pose evaluation runs through `Query::par_iter_mut` on a bounded
//! `bevy_tasks::ComputeTaskPool` initialised once with at most
//! `MAX_SIM_THREADS` worker threads. Each instance only writes its own
//! components, and the nearest-neighbour pass works on a snapshot sorted by
//! instance id, so tick results do not depend on thread scheduling.
//!
//! # Poses
//!
//! [`Simulation::render_frame`] interpolates the previous and current *local*
//! poses (translation/scale lerp, rotation slerp with the shortest arc) and
//! only then applies the model's forward kinematics and deformation. Skin
//! matrices are never linearly blended, so they stay rigid.

mod actions;
mod assets;
mod attention;
mod components;
mod pose;
mod social;
mod systems;
pub use attention::{AttentionEvent, AttentionSnapshot, AttentionSource};
pub use social::{SocialEvent, SocialInfo, SocialPhase};
use social::{SocialEvents, SocialState};

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use bevy_ecs::{entity::Entity, schedule::Schedule, world::World};
use glam::{Mat4, Vec3};
use thiserror::Error;

use crate::{
    model::{CompiledModel, ModelData, ModelError, ModelKind, try_deform_layer, try_skin_matrices},
    renderer::{InstanceDraw, RenderFrame},
};

use actions::{ActionEvents, ActionState};
use attention::{AttentionRes, AttentionState};
use components::{
    InstanceTag, InteractionState, InteractionsRes, ModelHandle, PokeReaction, PoseState, Position,
    TickStatus, TimeState, WanderBoundsRes, WanderEventsRes, WanderRegionsRes, WanderState,
};
use pose::{DriveInput, Pose, clamp, interpolated_locals, interpolated_parameters};

pub use assets::render_asset;

/// Identifier of a compiled model inside the simulation's asset pool.
pub type AssetId = u64;

/// Identifier of a live instance.
pub type InstanceId = u64;

/// Fixed simulation step: the clock never advances faster than this.
pub const FIXED_DT: Duration = Duration::from_nanos(FIXED_DT_NANOS);

/// Maximum number of fixed steps a single [`Simulation::advance`] call runs.
pub const MAX_SUBSTEPS: u32 = 4;

/// Largest number of instances the simulation keeps alive.
pub const MAX_INSTANCES: usize = 64;

/// Distance in world units inside which two instances interact.
pub const INTERACTION_RADIUS: f32 = 2.5;

/// The step length in nanoseconds, matching [`FIXED_DT`] exactly.
const FIXED_DT_NANOS: u64 = 1_000_000_000 / 60;

/// The same step as seconds, for animation sampling.
const FIXED_DT_SECONDS: f64 = FIXED_DT_NANOS as f64 / 1_000_000_000.0;

/// Fraction of the remaining weight a reaction gains per second.
const INTERACTION_EASE_PER_SECOND: f32 = 6.0;

/// Maximum head yaw the 3D interaction drive applies (60 degrees).
const HEAD_YAW_LIMIT: f32 = std::f32::consts::FRAC_PI_3;

/// Deterministic animation phase offset per instance id, so clones of one
/// asset never start out in exactly the same pose.
const CLONE_PHASE_STEP_SECONDS: f64 = 0.23;

/// Walking speed of an autonomous instance, in world units per second.
pub const WANDER_SPEED: f32 = 0.25;

/// Shortest idle pause between two walks, in seconds.
pub const WANDER_IDLE_MIN_SECONDS: f32 = 1.5;

/// Longest idle pause between two walks, in seconds.
pub const WANDER_IDLE_MAX_SECONDS: f32 = 4.5;

/// Targets closer than this to the current position are replaced by the far
/// end of the walkable range, so a walk never degenerates into a nudge.
pub const WANDER_MIN_TARGET_DISTANCE: f32 = 0.8;

/// Inset from the visible world bounds the wandering centre must keep.
/// Wide enough (wider than the widest bundled asset half-width, 0.98) that no
/// character leaves the screen while walking.
pub const WANDER_EDGE_MARGIN: f32 = 1.2;

/// Distance below which a walk snaps onto its target. Must exceed the per
/// step travel (`WANDER_SPEED * FIXED_DT` = 0.004167) for convergence.
pub const WANDER_ARRIVE_EPSILON: f32 = 0.01;

/// Golden-ratio increment of the per-instance splitmix64 stream.
pub const WANDER_SEED_STEP: u64 = 0x9E37_79B9_7F4A_7C15;

/// Upper bound of the drainable wander-event buffer. Events beyond it are
/// dropped; they are pure observations the simulation never reads back, so
/// dropping cannot change any state.
pub const WANDER_EVENT_CAP: usize = 4096;

/// How long a poke reaction (turn plus hop) lasts, in seconds. While a
/// reaction is active the instance stands still and keeps looking at the
/// poke point.
pub const POKE_REACTION_SECONDS: f32 = 1.0;

/// Peak height of the poke hop, in world units. Small enough to stay well
/// inside the 0.1 geometric hit-region tolerance, so the hop never changes
/// which pixels hit the instance.
pub const POKE_HOP_AMPLITUDE: f32 = 0.07;

/// Fraction of the reaction spent ramping the turn up from zero.
const POKE_RISE_FRACTION: f32 = 0.15;

/// Duration of the hop parabola, in seconds.
const POKE_HOP_SECONDS: f32 = 0.45;

/// Horizontal distance below which a poke counts as on-centre: no turn is
/// applied (the direction is undecidable), only the hop.
const POKE_DIRECTION_DEAD_ZONE: f32 = 0.05;

/// Largest accepted coordinate of any instance position, on any axis.
const MAX_COORDINATE: f32 = 10_000.0;

/// Worker threads the simulation's task pool may use at most.
const MAX_SIM_THREADS: usize = 4;

/// Model loads the app may keep in flight while assets are added.
const MAX_PENDING_LOADS: usize = 4;

/// Default example assets the app pins with `keep`.
const MAX_DEFAULT_ASSETS: usize = 2;

/// Asset pool budget: every live instance, the pending loads and the pinned
/// defaults must fit at the same time.
const MAX_ASSETS: usize = MAX_INSTANCES + MAX_PENDING_LOADS + MAX_DEFAULT_ASSETS;

/// Semantic action selected by an explicitly bound model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Idle,
    Walk,
    Reaction,
    Greet,
    Play,
}

impl ActionKind {
    /// Stable log label shared by the application and GUI assertions.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Walk => "walk",
            Self::Reaction => "reaction",
            Self::Greet => "greet",
            Self::Play => "play",
        }
    }
}

/// A committed action change or repeated reaction, never a per-frame sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionEvent {
    pub instance: InstanceId,
    pub action: ActionKind,
    pub restarted: bool,
}

/// Read-only snapshot of one live instance.
#[derive(Clone, Debug)]
pub struct InstanceInfo {
    /// Stable instance identifier.
    pub id: InstanceId,
    /// Asset this instance currently draws.
    pub asset: AssetId,
    /// Name of the current model, as declared in the file.
    pub name: String,
    /// Kind of the current model.
    pub kind: ModelKind,
    /// World-space position; the root transform is a pure translation.
    pub position: Vec3,
    /// Nearest neighbour inside [`INTERACTION_RADIUS`], if any.
    pub neighbor: Option<InstanceId>,
    /// Current animation time in seconds.
    pub animation_time: f64,
    /// Selected semantic action, or `None` for legacy all-animation playback.
    pub action: Option<ActionKind>,
    /// Reciprocal social activity; independent of user drag suspension.
    pub social: Option<SocialInfo>,
}

/// Horizontal world range autonomous walking is confined to.
///
/// Callers pass the raw visible range (`x = ±span/2 * aspect`); the
/// [`WANDER_EDGE_MARGIN`] inset is applied inside the simulation so the
/// policy lives in exactly one place. Invariant: both values are finite and
/// `left <= right`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldXBounds {
    pub left: f32,
    pub right: f32,
}

/// A visible world rectangle. Its top is inclusive and bottom exclusive,
/// matching physical desktop rows. Both dimensions must be strictly positive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldRect {
    pub left: f32,
    pub right: f32,
    pub bottom: f32,
    pub top: f32,
}

/// One observed wander transition, drained by the application layer and
/// logged there; the simulation itself never reads events back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WanderEvent {
    /// An idle instance picked a new target and started walking.
    Start {
        instance: InstanceId,
        x: f32,
        y: f32,
        target_x: f32,
    },
    /// A walking instance reached its target and stopped.
    Arrive {
        instance: InstanceId,
        x: f32,
        y: f32,
    },
}

/// A directed nearest-neighbour link resolved during the last tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InteractionEdge {
    /// Instance that reacts.
    pub from: InstanceId,
    /// Instance it reacts to.
    pub to: InstanceId,
    /// Distance in world units at snapshot time.
    pub distance: f32,
}

/// What one [`Simulation::advance`] call did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Fixed steps that were actually simulated.
    pub steps: u32,
    /// Whole steps that were discarded because the backlog exceeded
    /// [`MAX_SUBSTEPS`].
    pub dropped_steps: u64,
}

/// Every failure the simulation reports.
///
/// API calls never panic: unknown ids, exhausted budgets, unusable positions
/// and broken model data all surface here.
#[derive(Debug, Error)]
pub enum SimError {
    /// No asset is registered under this id.
    #[error("asset {0} does not exist")]
    UnknownAsset(AssetId),

    /// No instance is registered under this id.
    #[error("instance {0} does not exist")]
    UnknownInstance(InstanceId),

    /// The 64-instance budget is exhausted.
    #[error("instance limit of {limit} has been reached")]
    InstanceLimit { limit: usize },

    /// The asset pool budget is exhausted.
    #[error("asset pool limit of {limit} has been reached")]
    AssetLimit { limit: usize },

    /// A position is not finite or lies outside the accepted world range.
    #[error("position {position:?} is not a finite, bounded world position")]
    InvalidPosition { position: Vec3 },

    /// Wander bounds are not finite or not ordered (`left <= right`).
    /// The previous bounds stay in effect.
    #[error("wander bounds ({left}, {right}) are not finite and ordered")]
    InvalidWanderBounds { left: f32, right: f32 },

    /// A desktop visibility rectangle is non-finite, empty or reversed.
    #[error("wander region {index} is not finite and strictly ordered")]
    InvalidWanderRegion { index: usize },

    /// An attention environment snapshot is invalid. The whole previous
    /// snapshot (screens, work areas and windows) stays in effect.
    #[error("attention environment is invalid: {detail}")]
    InvalidAttentionEdges { detail: String },

    /// Instance state does not match its model (only reachable with
    /// hand-built, unvalidated compiled data).
    #[error("instance {instance} is inconsistent with its model: {detail}")]
    Inconsistent {
        instance: InstanceId,
        detail: String,
    },

    /// Model evaluation failed.
    #[error("model evaluation failed: {0}")]
    Model(#[from] ModelError),

    /// A poke cannot be accepted without reusing a consumed generation.
    #[error("poke generation exhausted for instance {instance}")]
    PokeRevisionExhausted { instance: InstanceId },

    /// The monotonic identifier space is exhausted.
    #[error("identifier space exhausted")]
    IdExhausted,
}

/// The whole simulation: one asset pool, one ECS world, one 60 Hz clock.
///
/// The ECS world is deliberately private; all external access goes through
/// the methods below, which keep instances and assets in sync.
pub struct Simulation {
    world: World,
    schedule: Schedule,
    assets: assets::AssetPool,
    entities: std::collections::BTreeMap<InstanceId, Entity>,
    next_instance_id: InstanceId,
    accumulator_nanos: u64,
    tick_count: u64,
}

impl Default for Simulation {
    fn default() -> Self {
        Self::new()
    }
}

impl Simulation {
    /// Creates an empty simulation with no assets and no instances.
    pub fn new() -> Self {
        systems::initialize_task_pool();
        let mut world = World::new();
        world.insert_resource(InteractionsRes::default());
        world.insert_resource(TickStatus::default());
        world.insert_resource(WanderBoundsRes::default());
        world.insert_resource(WanderRegionsRes::default());
        world.insert_resource(WanderEventsRes::default());
        world.insert_resource(ActionEvents::default());
        world.insert_resource(SocialEvents::default());
        world.insert_resource(AttentionRes::default());
        world.register_component::<SocialState>();
        // Register the instance components up front, so the schedule can be
        // initialised even when it runs before the first spawn.
        world.register_component::<ModelHandle>();
        world.register_component::<InstanceTag>();
        world.register_component::<Position>();
        world.register_component::<TimeState>();
        world.register_component::<PoseState>();
        world.register_component::<InteractionState>();
        world.register_component::<WanderState>();
        world.register_component::<PokeReaction>();
        world.register_component::<ActionState>();
        world.register_component::<AttentionState>();
        Self {
            world,
            schedule: systems::build_schedule(),
            assets: assets::AssetPool::new(),
            entities: std::collections::BTreeMap::new(),
            next_instance_id: 1,
            accumulator_nanos: 0,
            tick_count: 0,
        }
    }

    // -- assets -------------------------------------------------------------

    /// Registers a compiled model and returns its asset id.
    ///
    /// Ids increase monotonically and never depend on the file name, so
    /// loading two files with the same name can never overwrite an entry.
    pub fn insert_asset(&mut self, model: Arc<CompiledModel>) -> Result<AssetId, SimError> {
        self.assets.insert(model)
    }

    /// Looks up a registered asset.
    pub fn asset(&self, id: AssetId) -> Option<&Arc<CompiledModel>> {
        self.assets.get(id)
    }

    /// Removes an asset, but only while no instance references it.
    ///
    /// Returns `false` for unknown ids and for assets still in use.
    pub fn discard_asset(&mut self, id: AssetId) -> bool {
        if self.assets.get(id).is_none() || self.asset_in_use(id) {
            return false;
        }
        self.assets.remove(id)
    }

    /// Removes every asset that is neither used by an instance nor listed in
    /// `keep` and returns the removed ids in ascending order.
    ///
    /// The caller uses the returned ids to drop the matching renderer
    /// resources.
    pub fn collect_unused_assets(&mut self, keep: &[AssetId]) -> Vec<AssetId> {
        let used: BTreeSet<AssetId> = self
            .entities
            .values()
            .filter_map(|entity| {
                self.world
                    .get::<ModelHandle>(*entity)
                    .map(|handle| handle.asset)
            })
            .collect();
        self.assets.collect_unused(&used, keep)
    }

    /// Number of registered assets.
    pub fn asset_count(&self) -> usize {
        self.assets.len()
    }

    fn asset_in_use(&self, id: AssetId) -> bool {
        self.entities.values().any(|entity| {
            self.world
                .get::<ModelHandle>(*entity)
                .is_some_and(|handle| handle.asset == id)
        })
    }

    // -- instances ----------------------------------------------------------

    /// Spawns an instance of `asset` at `position`.
    ///
    /// Every instance shares the model `Arc` but keeps its own animation
    /// time, pose and interaction state. The initial animation phase is
    /// derived deterministically from the instance id, so clones of one asset
    /// start visibly out of step.
    pub fn spawn(&mut self, asset: AssetId, position: Vec3) -> Result<InstanceId, SimError> {
        let model = self
            .assets
            .get(asset)
            .cloned()
            .ok_or(SimError::UnknownAsset(asset))?;
        validate_position(position)?;
        if self.entities.len() >= MAX_INSTANCES {
            return Err(SimError::InstanceLimit {
                limit: MAX_INSTANCES,
            });
        }
        let id = self.next_instance_id;
        let time = id as f64 * CLONE_PHASE_STEP_SECONDS;
        // Sample before committing: a model whose data cannot produce a pose
        // must not create a half-initialised instance.
        let reaction = PokeReaction::default();
        let (action, pose) =
            actions::initialize(&model, time, time, &reaction, &DriveInput::none(), id)?;
        let attention = attention::initial_state(id, &model);
        let initial_action = action.0.as_ref().map(|playback| playback.kind);
        self.next_instance_id = id.checked_add(1).ok_or(SimError::IdExhausted)?;
        let entity = self
            .world
            .spawn((
                ModelHandle { asset, model },
                InstanceTag(id),
                Position(position),
                TimeState { time },
                PoseState {
                    prev: pose.clone(),
                    curr: pose,
                },
                InteractionState::default(),
                systems::initial_wander_state(id),
                reaction,
                action,
                SocialState::default(),
                attention,
            ))
            .id();
        self.entities.insert(id, entity);
        if let Some(action) = initial_action {
            self.world.resource_mut::<ActionEvents>().push(ActionEvent {
                instance: id,
                action,
                restarted: false,
            });
        }
        Ok(id)
    }

    /// Removes an instance. Returns `false` for unknown ids.
    ///
    /// Interaction links that pointed at the instance are cleared right away,
    /// without waiting for the next tick.
    pub fn despawn(&mut self, id: InstanceId) -> bool {
        let Some(entity) = self.entities.remove(&id) else {
            return false;
        };
        social::cancel(&mut self.world, id);
        self.world.despawn(entity);
        attention::forget_instance(&mut self.world, id);
        if let Some(mut interactions) = self.world.get_resource_mut::<InteractionsRes>() {
            interactions
                .0
                .retain(|edge| edge.from != id && edge.to != id);
        }
        {
            let mut states = self.world.query::<&mut InteractionState>();
            for mut state in states.iter_mut(&mut self.world) {
                if state.neighbor == Some(id) {
                    state.neighbor = None;
                    state.offset = Vec3::ZERO;
                    state.weight = 0.0;
                }
            }
        }
        true
    }

    /// Moves an instance. Non-finite or extreme positions are rejected and
    /// leave the instance untouched.
    ///
    /// An externally placed instance abandons any ongoing walk and starts a
    /// fresh idle countdown (its facing is kept); a drag or control-window
    /// edit therefore always wins over autonomous movement. The suspension
    /// flag is orthogonal and untouched, so a drag that only moves the
    /// pointer a little cannot accidentally resume a suspended instance.
    pub fn set_position(&mut self, id: InstanceId, position: Vec3) -> Result<(), SimError> {
        validate_position(position)?;
        let entity = *self
            .entities
            .get(&id)
            .ok_or(SimError::UnknownInstance(id))?;
        let mut slot = self
            .world
            .get_mut::<Position>(entity)
            .ok_or(SimError::UnknownInstance(id))?;
        slot.0 = position;
        social::cancel(&mut self.world, id);
        if let Some(mut wander) = self.world.get_mut::<WanderState>(entity) {
            systems::cancel_walk(&mut wander);
        }
        Ok(())
    }

    // -- wander -------------------------------------------------------------

    /// Sets (or clears) the horizontal bounds autonomous walking is confined
    /// to. `None` disables walking entirely, which is the default.
    ///
    /// Bounds must be finite and ordered; anything else is rejected and the
    /// previous bounds stay in effect.
    pub fn set_wander_bounds(&mut self, bounds: Option<WorldXBounds>) -> Result<(), SimError> {
        if let Some(bounds) = bounds
            && (!bounds.left.is_finite() || !bounds.right.is_finite() || bounds.left > bounds.right)
        {
            return Err(SimError::InvalidWanderBounds {
                left: bounds.left,
                right: bounds.right,
            });
        }
        if let Some(mut slot) = self.world.get_resource_mut::<WanderBoundsRes>() {
            slot.0 = bounds;
        }
        Ok(())
    }

    /// Restricts walking to the visible segment containing each instance at
    /// its current height. `None` preserves the original bounds-only behavior;
    /// `Some(vec![])` pauses all walking. Invalid input preserves the old value.
    pub fn set_wander_regions(&mut self, regions: Option<Vec<WorldRect>>) -> Result<(), SimError> {
        if let Some(regions) = &regions {
            for (index, rect) in regions.iter().enumerate() {
                if ![rect.left, rect.right, rect.bottom, rect.top]
                    .iter()
                    .all(|v| v.is_finite())
                    || rect.left >= rect.right
                    || rect.bottom >= rect.top
                {
                    return Err(SimError::InvalidWanderRegion { index });
                }
            }
        }
        self.world.resource_mut::<WanderRegionsRes>().0 = regions;
        Ok(())
    }

    /// Reads the current wander bounds, if walking is enabled.
    pub fn wander_bounds(&self) -> Option<WorldXBounds> {
        self.world
            .get_resource::<WanderBoundsRes>()
            .and_then(|bounds| bounds.0)
    }

    /// Parks an instance: any ongoing walk is cancelled and the instance
    /// stays put until [`resume_wander`](Self::resume_wander) is called. The
    /// idle countdown is frozen too, so holding a character still for longer
    /// than one idle period cannot move it away from the pointer.
    pub fn suspend_wander(&mut self, id: InstanceId) -> Result<(), SimError> {
        let entity = *self
            .entities
            .get(&id)
            .ok_or(SimError::UnknownInstance(id))?;
        let mut wander = self
            .world
            .get_mut::<WanderState>(entity)
            .ok_or(SimError::UnknownInstance(id))?;
        wander.suspended = true;
        systems::cancel_walk(&mut wander);
        social::cancel(&mut self.world, id);
        Ok(())
    }

    /// Releases a parked instance and draws a fresh idle duration, so the
    /// character pauses before setting off. Idempotent.
    pub fn resume_wander(&mut self, id: InstanceId) -> Result<(), SimError> {
        let entity = *self
            .entities
            .get(&id)
            .ok_or(SimError::UnknownInstance(id))?;
        let mut wander = self
            .world
            .get_mut::<WanderState>(entity)
            .ok_or(SimError::UnknownInstance(id))?;
        wander.suspended = false;
        systems::reset_to_idle(&mut wander);
        Ok(())
    }

    /// Starts (or restarts) a poke reaction on an instance: it stops
    /// walking, turns toward `point` for [`POKE_REACTION_SECONDS`] and hops
    /// once. A repeat poke resets the reaction.
    ///
    /// The point is validated like any other world position; a point inside
    /// the horizontal dead zone around the instance turns the reaction into a
    /// hop without a turn. The position itself never moves.
    pub fn poke_reaction(&mut self, id: InstanceId, point: Vec3) -> Result<(), SimError> {
        validate_position(point)?;
        let entity = *self
            .entities
            .get(&id)
            .ok_or(SimError::UnknownInstance(id))?;
        let position = self
            .world
            .get::<Position>(entity)
            .ok_or(SimError::UnknownInstance(id))?
            .0;
        let delta = point - position;
        let horizontal = Vec3::new(delta.x, delta.y, 0.0);
        let direction = if horizontal.length() < POKE_DIRECTION_DEAD_ZONE {
            Vec3::ZERO
        } else {
            horizontal.normalize()
        };
        let mut reaction = self
            .world
            .get_mut::<PokeReaction>(entity)
            .ok_or(SimError::UnknownInstance(id))?;
        let revision = reaction
            .revision
            .checked_add(1)
            .ok_or(SimError::PokeRevisionExhausted { instance: id })?;
        reaction.direction = direction;
        reaction.elapsed = 0.0;
        reaction.revision = revision;
        social::cancel(&mut self.world, id);
        if self.attention_enabled() {
            attention::record_poke(&mut self.world, entity);
        }
        Ok(())
    }

    // -- attention ----------------------------------------------------------

    /// Enables or disables the opt-in attention presentation.
    ///
    /// Disabled is the default and leaves every headless and benchmark run
    /// bit-exact. Disabling immediately clears the presentation, the pointer,
    /// the environment snapshot, the event buffer and every streak; no pose
    /// drive or root presentation survives.
    pub fn set_attention_enabled(&mut self, enabled: bool) {
        if let Some(mut res) = self.world.get_resource_mut::<AttentionRes>() {
            res.enabled = enabled;
        }
        if !enabled {
            attention::disable(&mut self.world);
        }
    }

    /// Sets (or clears) the world-space pointer target. `None` removes the
    /// target and lets the pointer weight decay.
    ///
    /// The point is validated like any other world position; an invalid point
    /// is rejected and the previous target stays untouched.
    pub fn set_pointer_world(&mut self, point: Option<Vec3>) -> Result<(), SimError> {
        if let Some(point) = point {
            validate_position(point)?;
        }
        if let Some(mut res) = self.world.get_resource_mut::<AttentionRes>() {
            res.pointer = point;
        }
        Ok(())
    }

    /// Replaces the desktop environment used for edge attention.
    ///
    /// Every rectangle must be finite, bounded by the simulation coordinate
    /// limit (10000 world units) and strictly ordered; the groups are capped
    /// at 64 screen, 64 work and 32 window rectangles. Any invalid input
    /// rejects the whole call and the previous snapshot stays in effect.
    /// Empty slices clear that group.
    pub fn set_attention_edges(
        &mut self,
        screens: &[WorldRect],
        work: &[WorldRect],
        windows: &[WorldRect],
    ) -> Result<(), SimError> {
        attention::validate_edges(screens, work, windows)?;
        attention::store_edges(&mut self.world, screens, work, windows);
        Ok(())
    }

    /// Starts drag presentation feedback for an instance.
    ///
    /// Presentation only: the caller already suspends wandering, so this never
    /// touches position, wander, reactions or actions. It is meant to be
    /// called after the drag threshold is crossed.
    pub fn begin_drag_feedback(&mut self, id: InstanceId) -> Result<(), SimError> {
        let entity = *self
            .entities
            .get(&id)
            .ok_or(SimError::UnknownInstance(id))?;
        if !self.attention_enabled() {
            // No effects while attention is disabled.
            return Ok(());
        }
        let position = self
            .world
            .get::<Position>(entity)
            .ok_or(SimError::UnknownInstance(id))?
            .0;
        let mut state = self
            .world
            .get_mut::<AttentionState>(entity)
            .ok_or(SimError::UnknownInstance(id))?;
        state.begin_drag(position);
        Ok(())
    }

    /// Ends drag presentation feedback.
    ///
    /// With `settle` and an actually active drag, the shear and squash damp
    /// out over 0.20 s; every other case returns to neutral immediately.
    pub fn end_drag_feedback(&mut self, id: InstanceId, settle: bool) -> Result<(), SimError> {
        let entity = *self
            .entities
            .get(&id)
            .ok_or(SimError::UnknownInstance(id))?;
        if !self.attention_enabled() {
            return Ok(());
        }
        let mut state = self
            .world
            .get_mut::<AttentionState>(entity)
            .ok_or(SimError::UnknownInstance(id))?;
        state.end_drag(settle);
        Ok(())
    }

    /// Snapshot of one instance's attention and root presentation.
    pub fn attention_snapshot(&self, id: InstanceId) -> Option<AttentionSnapshot> {
        let entity = *self.entities.get(&id)?;
        self.world
            .get::<AttentionState>(entity)
            .map(AttentionState::snapshot)
    }

    /// Takes the presentation-source transitions since the last drain.
    /// Draining is observational and can never change simulation state.
    pub fn drain_attention_events(&mut self) -> Vec<AttentionEvent> {
        self.world
            .get_resource_mut::<AttentionRes>()
            .map(|mut res| std::mem::take(&mut res.event_buffer))
            .unwrap_or_default()
    }

    fn attention_enabled(&self) -> bool {
        self.world
            .get_resource::<AttentionRes>()
            .is_some_and(|res| res.enabled)
    }

    /// Takes pair transitions; draining is observational and never drives playback.
    pub fn drain_social_events(&mut self) -> Vec<SocialEvent> {
        std::mem::take(&mut self.world.resource_mut::<SocialEvents>().0)
    }

    /// Takes committed action changes since the last drain, including initial
    /// bound states at spawn and replacement. Draining never affects playback.
    pub fn drain_action_events(&mut self) -> Vec<ActionEvent> {
        self.world
            .get_resource_mut::<ActionEvents>()
            .map(|mut events| std::mem::take(&mut events.0))
            .unwrap_or_default()
    }

    /// Takes every wander transition recorded since the last drain. The
    /// buffer is cleared, so callers must drain once per frame right after
    /// [`advance`](Self::advance) to avoid replaying events on zero-step
    /// frames.
    pub fn drain_wander_events(&mut self) -> Vec<WanderEvent> {
        self.world
            .get_resource_mut::<WanderEventsRes>()
            .map(|mut events| std::mem::take(&mut events.0))
            .unwrap_or_default()
    }

    /// Swaps the model of an existing instance.
    ///
    /// The new asset must already exist and its initial pose (sampled at the
    /// instance's current animation time, including the current interaction
    /// reaction) must be valid; otherwise the instance keeps its old asset,
    /// pose and time. For an unbound target, scalar parameters that exist in
    /// both models *by name* keep their current value (clamped to the new
    /// range). A bound target starts fresh action playback, or joins the
    /// current poke. Joint poses, clip indices and transition snapshots are
    /// never copied; the kind may change between 2D and 3D.
    ///
    /// The caller prepares the renderer-side resources for `asset` first and
    /// calls this to commit the swap.
    pub fn replace_model(&mut self, id: InstanceId, asset: AssetId) -> Result<(), SimError> {
        let model = self
            .assets
            .get(asset)
            .cloned()
            .ok_or(SimError::UnknownAsset(asset))?;
        let entity = *self
            .entities
            .get(&id)
            .ok_or(SimError::UnknownInstance(id))?;
        let old = self
            .world
            .get::<ModelHandle>(entity)
            .ok_or(SimError::UnknownInstance(id))?
            .model
            .clone();
        let time = self
            .world
            .get::<TimeState>(entity)
            .ok_or(SimError::UnknownInstance(id))?
            .time;
        let current = {
            let pose = self
                .world
                .get::<PoseState>(entity)
                .ok_or(SimError::UnknownInstance(id))?;
            pose.curr.clone()
        };
        let drive = {
            let interaction = self
                .world
                .get::<InteractionState>(entity)
                .ok_or(SimError::UnknownInstance(id))?;
            let facing = if model.kind() == ModelKind::Sprite2d {
                self.world
                    .get::<WanderState>(entity)
                    .map_or(1.0, |wander| wander.facing)
            } else {
                1.0
            };
            let turn = self
                .world
                .get::<WanderState>(entity)
                .map_or(1.0, |wander| wander.turn);
            let reaction = self
                .world
                .get::<PokeReaction>(entity)
                .ok_or(SimError::UnknownInstance(id))?;
            if model.actions.is_some() && reaction.elapsed < POKE_REACTION_SECONDS {
                DriveInput::from_reaction(reaction, facing, turn)
            } else {
                DriveInput::from_state(interaction, facing, turn)
            }
        };

        // Everything is computed before anything is written, including the
        // model-shaped action state. No old transition survives a swap.
        let reaction = self
            .world
            .get::<PokeReaction>(entity)
            .ok_or(SimError::UnknownInstance(id))?;
        let (action, mut next) = actions::initialize(&model, 0.0, time, reaction, &drive, id)?;
        let initial_action = action.0.as_ref().map(|playback| playback.kind);
        if model.actions.is_none()
            && let (
                ModelData::Sprite2d(old_sprite),
                ModelData::Sprite2d(new_sprite),
                Pose::Sprite2d(new_parameters),
            ) = (&old.data, &model.data, &mut next)
            && let Pose::Sprite2d(current_parameters) = &current
        {
            for (index, parameter) in new_sprite.parameters.iter().enumerate() {
                let Some(old_index) = old_sprite.parameter_index(&parameter.name) else {
                    continue;
                };
                let Some(value) = current_parameters.get(old_index) else {
                    continue;
                };
                if let Some(slot) = new_parameters.get_mut(index) {
                    *slot = clamp(*value, parameter.min, parameter.max);
                }
            }
        }

        social::cancel(&mut self.world, id);
        let attention_model = Arc::clone(&model);
        self.world.entity_mut(entity).insert((
            ModelHandle { asset, model },
            PoseState {
                prev: next.clone(),
                curr: next,
            },
            action,
        ));
        // Only after the successful commit: the new model's rest extents
        // replace the cached ones, and every model-shaped presentation value
        // (drag, settle, pointer/edge/idle and the poke streak) starts over.
        // A failed swap above returns before this and keeps the old state.
        attention::reset_for_model(&mut self.world, entity, &attention_model);
        if let Some(action) = initial_action {
            self.world.resource_mut::<ActionEvents>().push(ActionEvent {
                instance: id,
                action,
                restarted: false,
            });
        }
        Ok(())
    }

    /// Snapshot of every live instance, in ascending id order.
    pub fn instances(&self) -> Vec<InstanceInfo> {
        self.entities
            .keys()
            .filter_map(|id| self.instance(*id))
            .collect()
    }

    /// Snapshot of one instance.
    pub fn instance(&self, id: InstanceId) -> Option<InstanceInfo> {
        let entity = *self.entities.get(&id)?;
        let handle = self.world.get::<ModelHandle>(entity)?;
        let position = self.world.get::<Position>(entity)?.0;
        let time = self.world.get::<TimeState>(entity)?.time;
        let interaction = self.world.get::<InteractionState>(entity)?;
        Some(InstanceInfo {
            id,
            asset: handle.asset,
            name: handle.model.name.clone(),
            kind: handle.model.kind(),
            position,
            neighbor: interaction.neighbor,
            animation_time: time,
            action: self
                .world
                .get::<ActionState>(entity)?
                .0
                .as_ref()
                .map(|playback| playback.kind),
            social: self.world.get::<SocialState>(entity)?.active,
        })
    }

    /// Interaction links of the last snapshot, sorted by `(from, to)`.
    pub fn interactions(&self) -> &[InteractionEdge] {
        let empty: &[InteractionEdge] = &[];
        self.world
            .get_resource::<InteractionsRes>()
            .map_or(empty, |interactions| interactions.0.as_slice())
    }

    // -- clock --------------------------------------------------------------

    /// Advances the fixed clock, simulating at most [`MAX_SUBSTEPS`] whole
    /// steps and dropping (and counting) the rest of the backlog.
    pub fn advance(&mut self, elapsed: Duration) -> Result<TickReport, SimError> {
        let requested = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        self.accumulator_nanos = self.accumulator_nanos.saturating_add(requested);

        let mut steps = 0;
        while self.accumulator_nanos >= FIXED_DT_NANOS && steps < MAX_SUBSTEPS {
            self.accumulator_nanos -= FIXED_DT_NANOS;
            self.schedule.run(&mut self.world);
            steps += 1;
            self.tick_count = self.tick_count.saturating_add(1);
            let error = self
                .world
                .get_resource_mut::<TickStatus>()
                .and_then(|mut status| status.error.take());
            if let Some(error) = error {
                return Err(error);
            }
        }

        let mut dropped_steps = 0;
        if self.accumulator_nanos >= FIXED_DT_NANOS {
            dropped_steps = self.accumulator_nanos / FIXED_DT_NANOS;
            self.accumulator_nanos -= dropped_steps * FIXED_DT_NANOS;
        }
        Ok(TickReport {
            steps,
            dropped_steps,
        })
    }

    /// Number of fixed steps simulated so far.
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// Residual of the clock inside the current step, in `0.0..1.0`.
    pub fn alpha(&self) -> f32 {
        (self.accumulator_nanos as f64 / FIXED_DT_NANOS as f64) as f32
    }

    // -- rendering ----------------------------------------------------------

    /// Builds the CPU render data for this frame.
    ///
    /// Parameter vectors and 3D local poses are interpolated between the
    /// previous and current tick with [`Simulation::alpha`]; the model's
    /// fallible evaluation then produces the deformed layer vertices or the
    /// skin matrices. No JSON is parsed and no GPU resource is touched here.
    pub fn render_frame(&self) -> Result<RenderFrame, SimError> {
        let alpha = self.alpha();
        let attention_enabled = self.attention_enabled();
        let mut instances = Vec::with_capacity(self.entities.len());
        for (&id, &entity) in &self.entities {
            let handle = self
                .world
                .get::<ModelHandle>(entity)
                .ok_or(SimError::UnknownInstance(id))?;
            let position = self
                .world
                .get::<Position>(entity)
                .ok_or(SimError::UnknownInstance(id))?
                .0;
            let pose = self
                .world
                .get::<PoseState>(entity)
                .ok_or(SimError::UnknownInstance(id))?;
            // Translation times a horizontal mirror around the instance's
            // own centre line; the facing is exactly ±1.0, so the matrix is
            // exact and needs no epsilon anywhere.
            //
            // Only 2D sprites mirror. A 3D skeleton keeps the authored root
            // orientation so asymmetric details (the scarf print, for
            // example) never appear reversed; the walk direction is conveyed
            // by the walk clip and the pose drive. Enforcing it here as well
            // keeps a stale or externally built state from reintroducing the
            // mirror.
            let facing = if handle.model.kind() == ModelKind::Sprite2d {
                self.world
                    .get::<WanderState>(entity)
                    .map_or(1.0, |wander| wander.facing)
            } else {
                1.0
            };
            // The poke hop is a presentation-only vertical offset; the
            // instance position itself never moves.
            let hop = self
                .world
                .get::<PokeReaction>(entity)
                .map_or(0.0, |reaction| systems::poke_hop(reaction.elapsed));
            let mut transform = Mat4::from_translation(position + Vec3::new(0.0, hop, 0.0))
                * Mat4::from_scale(Vec3::new(facing, 1.0, 1.0));
            // Opt-in root presentation: one bounded shear/scale pair around
            // the model's rest bottom. A neutral presentation returns exactly
            // `(0.0, 1.0)`, so the original matrix expression stays untouched
            // (no identity multiply, no changed signed zeros).
            if attention_enabled && let Some(state) = self.world.get::<AttentionState>(entity) {
                let (shear, scale_y) = state.interpolated_presentation(alpha);
                if shear != 0.0 || scale_y != 1.0 {
                    // The presentation is local to the model, which the
                    // mirror above may flip; compensating with the exact
                    // `facing` sign keeps the world-space lean direction.
                    transform *=
                        attention::presentation_matrix(state.base_y, shear * facing, scale_y);
                }
            }
            match &handle.model.data {
                ModelData::Sprite2d(sprite) => {
                    let parameters =
                        interpolated_parameters(sprite, &pose.prev, &pose.curr, alpha, id)?;
                    let mut layers = Vec::with_capacity(sprite.layers.len());
                    for index in 0..sprite.layers.len() {
                        layers.push(try_deform_layer(sprite, index, &parameters)?);
                    }
                    instances.push(InstanceDraw::Sprite2d {
                        asset: handle.asset,
                        instance: id,
                        transform,
                        layers,
                    });
                }
                ModelData::Skeleton3d(skeleton) => {
                    let locals = interpolated_locals(skeleton, &pose.prev, &pose.curr, alpha, id)?;
                    let skin_matrices = try_skin_matrices(skeleton, &locals)?;
                    instances.push(InstanceDraw::Skeleton3d {
                        asset: handle.asset,
                        instance: id,
                        transform,
                        skin_matrices,
                    });
                }
            }
        }
        Ok(RenderFrame { instances })
    }
}

/// Accepts only finite, reasonably bounded world positions.
fn validate_position(position: Vec3) -> Result<(), SimError> {
    let bounded = position.abs().max_element() <= MAX_COORDINATE;
    if position.is_finite() && bounded {
        Ok(())
    } else {
        Err(SimError::InvalidPosition { position })
    }
}
