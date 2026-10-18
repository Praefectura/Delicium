//! Deterministic, opt-in attention and root presentation.
//!
//! Nothing in this module has an effect until the application enables it
//! through [`Simulation::set_attention_enabled`](super::Simulation::set_attention_enabled):
//! every instance carries an [`AttentionState`] from spawn (the component is
//! registered up front), but [`step`] returns before touching the world and
//! every field stays neutral. Headless and benchmark behavior therefore stays
//! bit-exact while attention is disabled, which is the default.
//!
//! One step resolves at most one presentation target per instance (pointer,
//! screen/work edge, window edge or idle), eases the matching weights and
//! produces one bounded root presentation pair (horizontal shear plus vertical
//! scale) that the render pass folds into the already existing
//! [`RenderFrame`](crate::renderer::RenderFrame) transform. No pose evaluation,
//! renderer resource or picking path is changed here.

use bevy_ecs::{component::Component, entity::Entity, resource::Resource, world::World};
use glam::{Mat4, Vec3, Vec4};

use super::components::{InstanceTag, PokeReaction, Position, WanderState};
use super::pose::{DriveInput, lerp};
use super::social::SocialState;
use super::{FIXED_DT_SECONDS, InstanceId, POKE_REACTION_SECONDS, SimError, WorldRect};
use crate::model::{CompiledModel, ModelData};

/// Fraction of the remaining weight a presentation target gains per second.
const ATTENTION_EASE_PER_SECOND: f32 = 6.0;

/// Weight below which an eased source snaps to its target, so a decayed
/// presentation cannot linger with a tiny non-zero weight forever.
const ATTENTION_WEIGHT_SNAP: f32 = 1e-4;

/// Exit radius of the pointer and edge hysteresis, as a multiple of the enter
/// radius.
const ATTENTION_EXIT_FACTOR: f32 = 1.25;

/// The pointer anchor sits this fraction of the model height above the rest
/// base.
const ATTENTION_ANCHOR_FRACTION: f32 = 0.6;

/// Pointer enter radius, before clamping, as a fraction of the model height.
const POINTER_RADIUS_FRACTION: f32 = 0.6;

/// Bounds of the pointer enter radius in world units.
const POINTER_RADIUS_MIN: f32 = 0.35;
const POINTER_RADIUS_MAX: f32 = 1.2;

/// Bounds of the edge enter radius in world units.
const EDGE_RADIUS_MIN: f32 = 0.5;
const EDGE_RADIUS_MAX: f32 = 1.2;

/// Maximum head/gaze yaw an attention drive asks for (18 degrees). Deliberately
/// below the pose pass's [`HEAD_YAW_LIMIT`](super::HEAD_YAW_LIMIT), which stays
/// the final bound.
const ATTENTION_YAW_LIMIT: f32 = std::f32::consts::PI / 10.0;

/// Positive Z bias of the pointer direction, as a fraction of the model
/// height. Without it `atan2(dx, 0)` saturates at 90 degrees and the head would
/// only ever snap to the pose limit.
const POINTER_Z_BIAS_FRACTION: f32 = 0.6;

/// Smallest Z bias of the pointer direction, in world units.
const POINTER_Z_BIAS_MIN: f32 = 0.3;

/// Bounds of one idle variant duration in seconds.
const IDLE_MIN_SECONDS: f32 = 3.0;
const IDLE_MAX_SECONDS: f32 = 7.0;

/// Cross-fade between two idle variants.
const IDLE_TRANSITION_SECONDS: f32 = 0.35;

/// Number of bounded idle variants.
const IDLE_VARIANT_COUNT: u64 = 4;

/// Distinct seed step and salt for the idle stream. Never shared with the
/// wander stream in [`super::systems`].
const ATTENTION_SEED_STEP: u64 = 0xD1B5_4A32_D192_ED03;
const ATTENTION_SEED_SALT: u64 = 0xA076_1D64_78BD_642F;

/// Largest root shear the presentation may apply.
const ATTENTION_SHEAR_LIMIT: f32 = 0.08;

/// Bounds of the presentation Y scale; `1.012` is the largest idle breathing
/// amplitude.
const ATTENTION_SCALE_MIN: f32 = 0.96;
const ATTENTION_SCALE_MAX: f32 = 1.012;

/// Poke streak window and cap. The first click keeps the original hop.
const STREAK_WINDOW_SECONDS: f32 = 1.2;
const STREAK_CAP: u8 = 3;
const STREAK_SECOND_SHEAR: f32 = 0.04;
const STREAK_THIRD_SHEAR: f32 = 0.06;
const STREAK_OSCILLATION_HZ: f32 = 2.5;

/// Bounds of the horizontal drag velocity in world units per second.
const DRAG_VELOCITY_LIMIT: f32 = 4.0;

/// Velocity smoothing rate of the drag feedback.
const DRAG_SMOOTHING_PER_SECOND: f32 = 12.0;

/// Shear produced per unit of horizontal drag velocity.
const DRAG_SHEAR_GAIN: f32 = 0.04;

/// Settle animation after a drag release.
const DRAG_SETTLE_SECONDS: f32 = 0.20;
const SETTLE_SCALE_START: f32 = 0.96;

/// Fixed steps between two edge scans (10 Hz at the 60 Hz step).
const EDGE_SCAN_TICKS: u8 = 6;

/// Rect caps of one environment snapshot.
const MAX_SCREEN_RECTS: usize = 64;
const MAX_WORK_RECTS: usize = 64;
const MAX_WINDOW_RECTS: usize = 32;

/// Upper bound of the drainable attention-event buffer. Transitions beyond it
/// are dropped; draining is purely observational and cannot change state.
const ATTENTION_EVENT_CAP: usize = 4096;

/// Geometry fallback of a degenerate or empty model: a unit-height body whose
/// rest base sits on the instance origin.
const FALLBACK_BASE_Y: f32 = 0.0;
const FALLBACK_HEIGHT: f32 = 1.0;

/// Bounded idle variant targets: head/gaze yaw in radians, root shear and
/// root Y scale. All stay inside the documented presentation bounds.
const IDLE_VARIANTS: [(f32, f32, f32); 4] = [
    (std::f32::consts::PI / 30.0, 0.008, 1.004),
    (-std::f32::consts::PI / 30.0, -0.008, 0.996),
    (std::f32::consts::PI * 13.0 / 180.0, 0.015, 1.009),
    (-std::f32::consts::PI * 13.0 / 180.0, -0.015, 0.997),
];

/// Which presentation target currently owns an instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttentionSource {
    /// No attention target; the pose falls back to the existing drive.
    None,
    /// The pointer is inside its hysteresis radius.
    Pointer,
    /// The nearest screen edge is inside its hysteresis radius.
    ScreenEdge,
    /// The nearest work-area edge is inside its hysteresis radius.
    WorkAreaEdge,
    /// The nearest window edge is inside its hysteresis radius.
    WindowEdge,
    /// The deterministic idle variant.
    Idle,
    /// The user is dragging the instance.
    Drag,
}

impl AttentionSource {
    /// Stable log label shared by the application and tests.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pointer => "pointer",
            Self::ScreenEdge => "screen_edge",
            Self::WorkAreaEdge => "work_area_edge",
            Self::WindowEdge => "window_edge",
            Self::Idle => "idle",
            Self::Drag => "drag",
        }
    }
}

/// Read-only snapshot of one instance's attention and root presentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttentionSnapshot {
    /// Presentation target owning the instance after the last step.
    pub source: AttentionSource,
    /// Eased pointer weight in `0.0..=1.0`; decays after the pointer leaves.
    pub pointer_weight: f32,
    /// Eased edge weight in `0.0..=1.0`; decays after the edge leaves.
    pub edge_weight: f32,
    /// Current idle variant, always in `0..=3`.
    pub idle_variant: u8,
    /// Whether a drag is currently feeding the presentation.
    pub dragging: bool,
    /// Remaining drag settle time in seconds; `0.0` outside a settle.
    pub settle_remaining: f32,
    /// Consecutive poke streak in `0..=3`.
    pub streak: u8,
    /// Current committed root shear.
    pub shear_x: f32,
    /// Current committed root Y scale.
    pub scale_y: f32,
}

/// One presentation-source transition, drained by the application layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttentionEvent {
    /// Instance whose presentation source changed.
    pub instance: InstanceId,
    /// Source after the transition.
    pub source: AttentionSource,
}

/// Per-instance attention state, spawned with every instance.
///
/// Geometry extents are cached once at spawn and after a successful
/// [`Simulation::replace_model`](super::Simulation::replace_model), so a tick
/// never walks a mesh or scans model layers again.
#[derive(Component)]
pub(super) struct AttentionState {
    /// Local Y of the model's rest bottom, used as the presentation pivot.
    pub base_y: f32,
    /// Cached model height (`max_y - min_y`) in local units.
    pub height: f32,
    pointer_active: bool,
    pointer_direction: Vec3,
    pointer_weight: f32,
    edge_active: bool,
    edge_kind: AttentionSource,
    edge_direction: Vec3,
    edge_weight: f32,
    idle_active: bool,
    idle_weight: f32,
    idle_direction: Vec3,
    gaze_direction: Vec3,
    gaze_weight: f32,
    idle_variant: u8,
    idle_remaining: f32,
    idle_transition: f32,
    idle_from_yaw: f32,
    idle_to_yaw: f32,
    idle_from_shear: f32,
    idle_to_shear: f32,
    idle_from_scale: f32,
    idle_to_scale: f32,
    idle_present_shear: f32,
    idle_present_scale: f32,
    dragging: bool,
    drag_last_x: f32,
    drag_velocity: f32,
    settle_remaining: f32,
    settle_from_shear: f32,
    streak: u8,
    streak_window_remaining: f32,
    rng: u64,
    prev_shear: f32,
    prev_scale_y: f32,
    curr_shear: f32,
    curr_scale_y: f32,
    source: AttentionSource,
}

/// Environment and cadence inputs of one attention tick.
struct StepInput {
    pointer: Option<Vec3>,
    scan_edges: bool,
    screens: Vec<WorldRect>,
    work: Vec<WorldRect>,
    windows: Vec<WorldRect>,
}

/// Simulation-wide attention inputs and observation buffer.
#[derive(Resource, Default)]
pub(super) struct AttentionRes {
    pub enabled: bool,
    pub pointer: Option<Vec3>,
    pub screens: Vec<WorldRect>,
    pub work: Vec<WorldRect>,
    pub windows: Vec<WorldRect>,
    pub(super) event_buffer: Vec<AttentionEvent>,
    edge_cooldown: u8,
}

/// Initial per-instance state: cached geometry, a private idle stream and a
/// fully neutral presentation.
pub(super) fn initial_state(id: InstanceId, model: &CompiledModel) -> AttentionState {
    let (base_y, height) = model_extents(model);
    AttentionState {
        base_y,
        height,
        pointer_active: false,
        pointer_direction: Vec3::Z,
        pointer_weight: 0.0,
        edge_active: false,
        edge_kind: AttentionSource::None,
        edge_direction: Vec3::Z,
        edge_weight: 0.0,
        idle_active: false,
        idle_weight: 0.0,
        idle_direction: Vec3::Z,
        gaze_direction: Vec3::Z,
        gaze_weight: 0.0,
        idle_variant: 0,
        idle_remaining: 0.0,
        idle_transition: IDLE_TRANSITION_SECONDS,
        idle_from_yaw: 0.0,
        idle_to_yaw: 0.0,
        idle_from_shear: 0.0,
        idle_to_shear: 0.0,
        idle_from_scale: 1.0,
        idle_to_scale: 1.0,
        idle_present_shear: 0.0,
        idle_present_scale: 1.0,
        dragging: false,
        drag_last_x: 0.0,
        drag_velocity: 0.0,
        settle_remaining: 0.0,
        settle_from_shear: 0.0,
        streak: 0,
        streak_window_remaining: 0.0,
        rng: id.wrapping_mul(ATTENTION_SEED_STEP) ^ ATTENTION_SEED_SALT,
        prev_shear: 0.0,
        prev_scale_y: 1.0,
        curr_shear: 0.0,
        curr_scale_y: 1.0,
        source: AttentionSource::None,
    }
}

/// Validates one environment group without mutating anything.
fn validate_rects(group: &str, rects: &[WorldRect], limit: usize) -> Result<(), SimError> {
    if rects.len() > limit {
        return Err(SimError::InvalidAttentionEdges {
            detail: format!("{group} has {} rectangles, more than {limit}", rects.len()),
        });
    }
    for (index, rect) in rects.iter().enumerate() {
        let values = [rect.left, rect.right, rect.bottom, rect.top];
        let bounded = values
            .iter()
            .all(|value| value.is_finite() && value.abs() <= super::MAX_COORDINATE);
        if !bounded || rect.left >= rect.right || rect.bottom >= rect.top {
            return Err(SimError::InvalidAttentionEdges {
                detail: format!(
                    "{group} rectangle {index} is not finite, bounded and strictly ordered"
                ),
            });
        }
    }
    Ok(())
}

/// Validates a whole environment snapshot atomically.
pub(super) fn validate_edges(
    screens: &[WorldRect],
    work: &[WorldRect],
    windows: &[WorldRect],
) -> Result<(), SimError> {
    validate_rects("screen", screens, MAX_SCREEN_RECTS)?;
    validate_rects("work", work, MAX_WORK_RECTS)?;
    validate_rects("window", windows, MAX_WINDOW_RECTS)
}

/// Replaces the stored environment snapshot after [`validate_edges`] passed.
pub(super) fn store_edges(
    world: &mut World,
    screens: &[WorldRect],
    work: &[WorldRect],
    windows: &[WorldRect],
) {
    if let Some(mut res) = world.get_resource_mut::<AttentionRes>() {
        res.screens = screens.to_vec();
        res.work = work.to_vec();
        res.windows = windows.to_vec();
    }
}

/// Clears every attention input, observation and presentation in the world.
pub(super) fn disable(world: &mut World) {
    if let Some(mut res) = world.get_resource_mut::<AttentionRes>() {
        res.pointer = None;
        res.screens.clear();
        res.work.clear();
        res.windows.clear();
        res.event_buffer.clear();
        res.edge_cooldown = 0;
    }
    let mut query = world.query::<&mut AttentionState>();
    for mut state in query.iter_mut(world) {
        state.reset_presentation();
    }
}

/// Caches the new model's rest extents and neutralizes every model-shaped
/// presentation value.
///
/// Called only after a successful replacement commit, so a failed swap leaves
/// the old presentation untouched. The private idle stream position survives,
/// but the drag, settle, edge/pointer/idle presentation and the poke streak are
/// cleared because the model that produced them is gone.
pub(super) fn reset_for_model(world: &mut World, entity: Entity, model: &CompiledModel) {
    let Some(mut state) = world.get_mut::<AttentionState>(entity) else {
        return;
    };
    let (base_y, height) = model_extents(model);
    let rng = state.rng;
    state.base_y = base_y;
    state.height = height;
    state.reset_shape();
    state.rng = rng;
    state.streak = 0;
    state.streak_window_remaining = 0.0;
}

/// Records one accepted poke for the streak, capped at three.
pub(super) fn record_poke(world: &mut World, entity: Entity) {
    let Some(mut state) = world.get_mut::<AttentionState>(entity) else {
        return;
    };
    state.streak = if state.streak_window_remaining > 0.0 && state.streak > 0 {
        state.streak.saturating_add(1).min(STREAK_CAP)
    } else {
        1
    };
    state.streak_window_remaining = STREAK_WINDOW_SECONDS;
}

/// Drops any queued event for `instance`, so a despawned id is never reported
/// again.
pub(super) fn forget_instance(world: &mut World, id: InstanceId) {
    if let Some(mut res) = world.get_resource_mut::<AttentionRes>() {
        res.event_buffer.retain(|event| event.instance != id);
    }
}

/// Advances attention and root presentation by one fixed step.
///
/// An exclusive system on purpose: the per-instance pass is sorted by instance
/// id and reads only its own components plus the environment snapshot, so the
/// result cannot depend on archetype or thread order. A disabled resource
/// returns before writing any component.
pub(super) fn step(world: &mut World) {
    // A disabled resource is checked without a mutable borrow, so the default
    // headless path cannot even bump a change tick.
    if !world
        .get_resource::<AttentionRes>()
        .is_some_and(|res| res.enabled)
    {
        return;
    }
    let input = {
        let Some(mut res) = world.get_resource_mut::<AttentionRes>() else {
            return;
        };
        let scan_edges = res.edge_cooldown == 0;
        if scan_edges {
            // Period of exactly EDGE_SCAN_TICKS fixed steps (10 Hz).
            res.edge_cooldown = EDGE_SCAN_TICKS - 1;
        } else {
            res.edge_cooldown -= 1;
        }
        StepInput {
            pointer: res.pointer,
            scan_edges,
            screens: if scan_edges {
                res.screens.clone()
            } else {
                Vec::new()
            },
            work: if scan_edges {
                res.work.clone()
            } else {
                Vec::new()
            },
            windows: if scan_edges {
                res.windows.clone()
            } else {
                Vec::new()
            },
        }
    };

    let mut rows: Vec<(InstanceId, Entity)> = {
        let mut query = world.query::<(Entity, &InstanceTag)>();
        query
            .iter(world)
            .map(|(entity, tag)| (tag.0, entity))
            .collect()
    };
    rows.sort_by_key(|(id, _)| *id);

    let mut events = Vec::new();
    for (id, entity) in rows {
        let Some(position) = world.get::<Position>(entity).map(|position| position.0) else {
            continue;
        };
        let moved_this_tick = world
            .get::<WanderState>(entity)
            .is_some_and(|wander| wander.moved_this_tick);
        let reaction_elapsed = world
            .get::<PokeReaction>(entity)
            .map_or(POKE_REACTION_SECONDS, |reaction| reaction.elapsed);
        let social = world
            .get::<SocialState>(entity)
            .is_some_and(|state| state.active.is_some());
        let Some(mut state) = world.get_mut::<AttentionState>(entity) else {
            continue;
        };
        let previous = state.source;
        state.advance(
            FIXED_DT_SECONDS as f32,
            position,
            moved_this_tick,
            reaction_elapsed,
            social,
            &input,
        );
        if state.source != previous {
            events.push(AttentionEvent {
                instance: id,
                source: state.source,
            });
        }
    }

    if events.is_empty() {
        return;
    }
    if let Some(mut res) = world.get_resource_mut::<AttentionRes>() {
        for event in events {
            if res.event_buffer.len() < ATTENTION_EVENT_CAP {
                res.event_buffer.push(event);
            }
        }
    }
}

impl AttentionState {
    /// Clears every presentation-derived value but keeps geometry and the idle
    /// stream position. The streak is cleared separately by the callers that
    /// need to drop it.
    fn reset_shape(&mut self) {
        self.pointer_active = false;
        self.pointer_direction = Vec3::Z;
        self.pointer_weight = 0.0;
        self.edge_active = false;
        self.edge_kind = AttentionSource::None;
        self.edge_direction = Vec3::Z;
        self.edge_weight = 0.0;
        self.idle_active = false;
        self.idle_weight = 0.0;
        self.idle_direction = Vec3::Z;
        self.gaze_direction = Vec3::Z;
        self.gaze_weight = 0.0;
        self.idle_variant = 0;
        self.idle_remaining = 0.0;
        self.idle_transition = IDLE_TRANSITION_SECONDS;
        self.idle_from_yaw = 0.0;
        self.idle_to_yaw = 0.0;
        self.idle_from_shear = 0.0;
        self.idle_to_shear = 0.0;
        self.idle_from_scale = 1.0;
        self.idle_to_scale = 1.0;
        self.idle_present_shear = 0.0;
        self.idle_present_scale = 1.0;
        self.dragging = false;
        self.drag_last_x = 0.0;
        self.drag_velocity = 0.0;
        self.settle_remaining = 0.0;
        self.settle_from_shear = 0.0;
        self.prev_shear = 0.0;
        self.prev_scale_y = 1.0;
        self.curr_shear = 0.0;
        self.curr_scale_y = 1.0;
        self.source = AttentionSource::None;
    }

    /// Full clear used when attention is disabled: no presentation, no input
    /// state and no streak survives.
    fn reset_presentation(&mut self) {
        self.reset_shape();
        self.streak = 0;
        self.streak_window_remaining = 0.0;
    }

    /// Starts a drag presentation at the instance's current position.
    pub(super) fn begin_drag(&mut self, position: Vec3) {
        self.dragging = true;
        self.drag_last_x = position.x;
        self.drag_velocity = 0.0;
        self.settle_remaining = 0.0;
        self.settle_from_shear = 0.0;
        self.streak = 0;
        self.streak_window_remaining = 0.0;
    }

    /// Ends a drag presentation. A settle runs only when a drag was actually
    /// active; every other path cancels to neutral immediately.
    pub(super) fn end_drag(&mut self, settle: bool) {
        let was_active = self.dragging;
        self.dragging = false;
        self.drag_velocity = 0.0;
        if settle && was_active {
            self.settle_remaining = DRAG_SETTLE_SECONDS;
            self.settle_from_shear = self.curr_shear;
            self.prev_scale_y = SETTLE_SCALE_START;
            self.curr_scale_y = SETTLE_SCALE_START;
        } else {
            self.settle_remaining = 0.0;
            self.settle_from_shear = 0.0;
            self.prev_shear = 0.0;
            self.curr_shear = 0.0;
            self.prev_scale_y = 1.0;
            self.curr_scale_y = 1.0;
        }
    }

    /// Current presentation snapshot.
    pub(super) fn snapshot(&self) -> AttentionSnapshot {
        AttentionSnapshot {
            source: self.source,
            pointer_weight: self.pointer_weight,
            edge_weight: self.edge_weight,
            idle_variant: self.idle_variant,
            dragging: self.dragging,
            settle_remaining: self.settle_remaining,
            streak: self.streak,
            shear_x: self.curr_shear,
            scale_y: self.curr_scale_y,
        }
    }

    /// Attention drive selected for the pose pass, if any.
    ///
    /// Pointer, edge and idle targets share one eased gaze envelope. Switching
    /// targets therefore retains the outgoing direction and strength instead
    /// of selecting an independently charged weight. Drag and pose locks clear
    /// the envelope; walking also cancels an idle gaze immediately. The root
    /// idle presentation is neutral immediately on suppression too.
    pub(super) fn drive(&self, facing: f32, turn: f32) -> Option<DriveInput> {
        if self.dragging || self.gaze_weight <= ATTENTION_WEIGHT_SNAP {
            return None;
        }
        Some(DriveInput {
            direction: self.gaze_direction,
            weight: self.gaze_weight,
            facing,
            turn,
        })
    }

    /// Root presentation interpolated between the previous and current tick.
    ///
    /// A fully neutral pair returns exactly `(0.0, 1.0)` without arithmetic,
    /// so the disabled/neutral render path keeps the original transform
    /// bit-exact.
    pub(super) fn interpolated_presentation(&self, alpha: f32) -> (f32, f32) {
        let shear = if self.prev_shear == 0.0 && self.curr_shear == 0.0 {
            0.0
        } else {
            lerp(self.prev_shear, self.curr_shear, alpha.clamp(0.0, 1.0))
        };
        let scale_y = if self.prev_scale_y == 1.0 && self.curr_scale_y == 1.0 {
            1.0
        } else {
            lerp(self.prev_scale_y, self.curr_scale_y, alpha.clamp(0.0, 1.0))
        };
        (shear, scale_y)
    }

    /// Runs the per-instance attention update for one fixed tick.
    fn advance(
        &mut self,
        dt: f32,
        position: Vec3,
        moved_this_tick: bool,
        reaction_elapsed: f32,
        social: bool,
        input: &StepInput,
    ) {
        let ease = (ATTENTION_EASE_PER_SECOND * dt).clamp(0.0, 1.0);
        let anchor = (
            position.x,
            position.y + self.base_y + ATTENTION_ANCHOR_FRACTION * self.height,
        );
        let feet = (position.x, position.y + self.base_y);
        let head = (position.x, position.y + self.base_y + self.height);

        // The streak window decays only while attention is enabled, which is
        // the only way this function runs.
        if self.streak_window_remaining > 0.0 {
            self.streak_window_remaining = (self.streak_window_remaining - dt).max(0.0);
            if self.streak_window_remaining <= 0.0 {
                self.streak = 0;
            }
        }

        // Drag velocity is derived from the externally moved position; the
        // simulation never writes the position for the presentation.
        if self.dragging {
            let raw = ((position.x - self.drag_last_x) / dt.max(f32::EPSILON))
                .clamp(-DRAG_VELOCITY_LIMIT, DRAG_VELOCITY_LIMIT);
            let smoothing = (DRAG_SMOOTHING_PER_SECOND * dt).clamp(0.0, 1.0);
            self.drag_velocity += (raw - self.drag_velocity) * smoothing;
            self.drag_velocity = self
                .drag_velocity
                .clamp(-DRAG_VELOCITY_LIMIT, DRAG_VELOCITY_LIMIT);
            self.drag_last_x = position.x;
        } else {
            self.drag_velocity = 0.0;
        }
        let drag_shear = (-self.drag_velocity * DRAG_SHEAR_GAIN)
            .clamp(-ATTENTION_SHEAR_LIMIT, ATTENTION_SHEAR_LIMIT);

        if !self.dragging && self.settle_remaining > 0.0 {
            self.settle_remaining = (self.settle_remaining - dt).max(0.0);
        }
        // A settle only exists after `end_drag(settle = true)` on an actually
        // active drag, so a positive remainder is the whole condition.
        let settling = self.settle_remaining > 0.0 && !self.dragging;
        let settle_progress = if settling {
            1.0 - self.settle_remaining / DRAG_SETTLE_SECONDS
        } else {
            1.0
        };
        let settle_shear = if settling {
            self.settle_from_shear * (1.0 - settle_progress) * (1.0 - settle_progress)
        } else {
            0.0
        };
        let settle_scale = if settling {
            SETTLE_SCALE_START + (1.0 - SETTLE_SCALE_START) * settle_progress
        } else {
            1.0
        };

        // Pointer: enter at R, leave at 1.25 R. The direction always carries a
        // positive Z bias (the model faces +Z), so `atan2(dx, dz)` is a real
        // angle instead of a saturated 90 degrees; the yaw itself is clamped
        // to the attention limit well below the pose pass bound.
        let pointer_radius =
            (POINTER_RADIUS_FRACTION * self.height).clamp(POINTER_RADIUS_MIN, POINTER_RADIUS_MAX);
        let pointer_distance = input.pointer.map(|pointer| {
            let dx = pointer.x - anchor.0;
            let dy = pointer.y - anchor.1;
            (dx * dx + dy * dy).sqrt()
        });
        self.pointer_active = match pointer_distance {
            Some(distance) if self.pointer_active => {
                distance <= pointer_radius * ATTENTION_EXIT_FACTOR
            }
            Some(distance) => distance <= pointer_radius,
            None => false,
        };
        if self.pointer_active
            && let Some(pointer) = input.pointer
        {
            self.pointer_direction = yaw_direction(pointer.x - position.x, self.height);
        }

        // Edges are scanned at 10 Hz; hysteresis and easing continue on every
        // tick from the last scan. A choice is re-evaluated with the exit
        // radius only while the same source (screen/work or window) stays
        // near, so switching groups does not inherit the wider threshold.
        if input.scan_edges {
            let radius =
                (POINTER_RADIUS_FRACTION * self.height).clamp(EDGE_RADIUS_MIN, EDGE_RADIUS_MAX);
            let held_window = matches!(self.edge_kind, AttentionSource::WindowEdge);
            let screen_threshold = if self.edge_active && !held_window {
                radius * ATTENTION_EXIT_FACTOR
            } else {
                radius
            };
            let window_threshold = if self.edge_active && held_window {
                radius * ATTENTION_EXIT_FACTOR
            } else {
                radius
            };
            let screen_work =
                nearest_screen_or_work(&input.screens, &input.work, feet, head, anchor);
            let window =
                nearest_rect_edge(&input.windows, feet, head, anchor).map(|edge| EdgeCandidate {
                    kind: AttentionSource::WindowEdge,
                    distance: edge.distance,
                    point_x: edge.point_x,
                });
            let chosen = screen_work
                .filter(|candidate| candidate.distance <= screen_threshold)
                .or_else(|| window.filter(|candidate| candidate.distance <= window_threshold));
            self.edge_active = chosen.is_some();
            match chosen {
                Some(candidate) => {
                    self.edge_kind = candidate.kind;
                    self.edge_direction =
                        yaw_direction(candidate.point_x - position.x, self.height);
                }
                None => self.edge_kind = AttentionSource::None,
            }
        }

        // Idle is suppressed by drags, drag settles, walking, social locks
        // and reactions, and only starts after the pointer and edge weights
        // have fully decayed, so its drive can never pre-empt a higher
        // priority source that is still easing out.
        let idle_allowed = !self.dragging
            && !settling
            && !social
            && !moved_this_tick
            && reaction_elapsed >= POKE_REACTION_SECONDS
            && self.pointer_weight <= ATTENTION_WEIGHT_SNAP
            && self.edge_weight <= ATTENTION_WEIGHT_SNAP;
        self.idle_active = idle_allowed && !self.pointer_active && !self.edge_active;

        if self.idle_active {
            if self.idle_remaining <= 0.0 {
                self.begin_idle_variant();
            }
            self.idle_remaining = (self.idle_remaining - dt).max(0.0);
            self.idle_transition = (self.idle_transition + dt).min(IDLE_TRANSITION_SECONDS);
        }
        self.refresh_idle_presentation();

        // A poke reaction or an active social pair owns the pose outright:
        // everything the attention pass could present is suppressed meanwhile,
        // so its source, weights and root pair stay neutral instead of
        // competing with the higher-priority drive.
        let pose_locked = social || reaction_elapsed < POKE_REACTION_SECONDS;
        let pointer_target = if self.pointer_active && !self.dragging && !pose_locked {
            1.0
        } else {
            0.0
        };
        // Per-source weights describe acquisition/decay and gate the return
        // to idle. The final gaze below has one shared envelope, so changing
        // the selected source never swaps independently charged pose drives.
        let edge_target =
            if self.edge_active && !self.pointer_active && !self.dragging && !pose_locked {
                1.0
            } else {
                0.0
            };
        let idle_target = if self.idle_active { 1.0 } else { 0.0 };
        self.pointer_weight = eased_weight(self.pointer_weight, pointer_target, ease);
        self.edge_weight = eased_weight(self.edge_weight, edge_target, ease);
        self.idle_weight = eased_weight(self.idle_weight, idle_target, ease);

        let previous_source = self.source;
        self.source = if self.dragging {
            AttentionSource::Drag
        } else if pose_locked {
            AttentionSource::None
        } else if self.pointer_active {
            AttentionSource::Pointer
        } else if self.edge_active {
            match self.edge_kind {
                AttentionSource::ScreenEdge
                | AttentionSource::WorkAreaEdge
                | AttentionSource::WindowEdge => self.edge_kind,
                _ => AttentionSource::None,
            }
        } else if self.idle_active {
            AttentionSource::Idle
        } else {
            AttentionSource::None
        };

        let gaze_target = match self.source {
            AttentionSource::Pointer => Some(self.pointer_direction),
            AttentionSource::ScreenEdge
            | AttentionSource::WorkAreaEdge
            | AttentionSource::WindowEdge => Some(self.edge_direction),
            AttentionSource::Idle => Some(self.idle_direction),
            AttentionSource::None | AttentionSource::Drag => None,
        };
        if self.dragging
            || pose_locked
            || (moved_this_tick
                && previous_source == AttentionSource::Idle
                && gaze_target.is_none())
        {
            self.gaze_direction = Vec3::Z;
            self.gaze_weight = 0.0;
        } else if let Some(target) = gaze_target {
            let yaw = if self.gaze_weight <= ATTENTION_WEIGHT_SNAP {
                target.x.atan2(target.z)
            } else {
                lerp(
                    self.gaze_direction.x.atan2(self.gaze_direction.z),
                    target.x.atan2(target.z),
                    ease,
                )
            }
            .clamp(-ATTENTION_YAW_LIMIT, ATTENTION_YAW_LIMIT);
            self.gaze_direction = Vec3::new(yaw.sin(), 0.0, yaw.cos());
            self.gaze_weight = eased_weight(self.gaze_weight, 1.0, ease);
        } else {
            self.gaze_weight = eased_weight(self.gaze_weight, 0.0, ease);
        }

        // One bounded presentation pair. The idle contribution exists only
        // while idle actually owns the instance: a walk, a social lock, a
        // reaction, a drag or a drag settle suppresses it in the same step and
        // leaves the root exactly neutral, as documented. Drag and settle
        // still contribute their own exclusive terms.
        let idle_gain = if self.idle_active {
            self.idle_weight
        } else {
            0.0
        };
        let idle_shear = self.idle_present_shear * idle_gain;
        let idle_scale = 1.0 + (self.idle_present_scale - 1.0) * idle_gain;
        let streak_shear = self.streak_shear(reaction_elapsed);
        let shear = (drag_shear + settle_shear + idle_shear + streak_shear)
            .clamp(-ATTENTION_SHEAR_LIMIT, ATTENTION_SHEAR_LIMIT);
        let base_scale = if settling { settle_scale } else { idle_scale };
        let scale_y = base_scale.clamp(ATTENTION_SCALE_MIN, ATTENTION_SCALE_MAX);

        self.prev_shear = self.curr_shear;
        self.prev_scale_y = self.curr_scale_y;
        self.curr_shear = shear;
        self.curr_scale_y = scale_y;
    }

    /// Damped shear oscillation of the second and third poke in a streak,
    /// played inside the existing one-second reaction. The first poke always
    /// keeps the original hop untouched.
    fn streak_shear(&self, reaction_elapsed: f32) -> f32 {
        if self.streak < 2 || reaction_elapsed < 0.0 || reaction_elapsed >= POKE_REACTION_SECONDS {
            return 0.0;
        }
        let peak = if self.streak >= 3 {
            STREAK_THIRD_SHEAR
        } else {
            STREAK_SECOND_SHEAR
        };
        let progress = reaction_elapsed / POKE_REACTION_SECONDS;
        let wave = (std::f32::consts::TAU * STREAK_OSCILLATION_HZ * progress).sin();
        peak * wave * (1.0 - progress)
    }

    fn idle_blend(&self) -> f32 {
        let t = (self.idle_transition / IDLE_TRANSITION_SECONDS).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    fn begin_idle_variant(&mut self) {
        let blend = self.idle_blend();
        let yaw = lerp(self.idle_from_yaw, self.idle_to_yaw, blend);
        let shear = lerp(self.idle_from_shear, self.idle_to_shear, blend);
        let scale = lerp(self.idle_from_scale, self.idle_to_scale, blend);
        // Every draw advances the private stream exactly twice, and skipping
        // the current variant guarantees consecutive variants differ visibly.
        let step = (next_random(&mut self.rng) % (IDLE_VARIANT_COUNT - 1)) as u8 + 1;
        let variant = (self.idle_variant + step) % IDLE_VARIANT_COUNT as u8;
        let duration =
            IDLE_MIN_SECONDS + random_unit(&mut self.rng) * (IDLE_MAX_SECONDS - IDLE_MIN_SECONDS);
        let (target_yaw, target_shear, target_scale) = IDLE_VARIANTS[variant as usize];
        self.idle_from_yaw = yaw;
        self.idle_from_shear = shear;
        self.idle_from_scale = scale;
        self.idle_to_yaw = target_yaw;
        self.idle_to_shear = target_shear;
        self.idle_to_scale = target_scale;
        self.idle_variant = variant;
        self.idle_transition = 0.0;
        self.idle_remaining = duration;
    }

    fn refresh_idle_presentation(&mut self) {
        let blend = self.idle_blend();
        let yaw = lerp(self.idle_from_yaw, self.idle_to_yaw, blend);
        self.idle_direction = Vec3::new(yaw.sin(), 0.0, yaw.cos());
        self.idle_present_shear = lerp(self.idle_from_shear, self.idle_to_shear, blend);
        self.idle_present_scale = lerp(self.idle_from_scale, self.idle_to_scale, blend);
    }
}

/// Eases one presentation weight toward its target and snaps once close.
fn eased_weight(current: f32, target: f32, ease: f32) -> f32 {
    if (target - current).abs() <= ATTENTION_WEIGHT_SNAP {
        return target;
    }
    (current + (target - current) * ease).clamp(0.0, 1.0)
}

/// Unit horizontal direction with a positive Z bias and a yaw bounded by the
/// attention limit.
fn yaw_direction(dx: f32, height: f32) -> Vec3 {
    let z_bias = (POINTER_Z_BIAS_FRACTION * height).max(POINTER_Z_BIAS_MIN);
    let yaw = dx
        .atan2(z_bias)
        .clamp(-ATTENTION_YAW_LIMIT, ATTENTION_YAW_LIMIT);
    Vec3::new(yaw.sin(), 0.0, yaw.cos())
}

/// splitmix64, identical arithmetic to the wander stream but a fully separate
/// per-instance sequence.
fn next_random(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn random_unit(state: &mut u64) -> f32 {
    (next_random(state) >> 40) as f32 / (1u32 << 24) as f32
}

/// One candidate edge, with the X of the nearest point on the boundary.
struct EdgeCandidate {
    kind: AttentionSource,
    distance: f32,
    point_x: f32,
}

/// Nearest point on one rectangle boundary, using the feet for bottom
/// awareness, the head for top awareness and the 60% anchor for the sides.
///
/// Every boundary is a nearest finite segment: the tested point is clamped
/// onto the segment before the distance is taken, so a foot below and beside a
/// rectangle resolves diagonally to its corner.
fn rect_edge(
    rect: &WorldRect,
    feet: (f32, f32),
    head: (f32, f32),
    anchor: (f32, f32),
) -> (f32, f32) {
    let mut best = (f32::INFINITY, feet.0);
    let mut consider = |distance: f32, point_x: f32| {
        if distance < best.0 {
            best = (distance, point_x);
        }
    };
    let bottom = nearest_on_segment(feet, rect.left, rect.bottom, rect.right, rect.bottom);
    consider(bottom.0, bottom.1);
    let top = nearest_on_segment(head, rect.left, rect.top, rect.right, rect.top);
    consider(top.0, top.1);
    let left = nearest_on_segment(anchor, rect.left, rect.bottom, rect.left, rect.top);
    consider(left.0, left.1);
    let right = nearest_on_segment(anchor, rect.right, rect.bottom, rect.right, rect.top);
    consider(right.0, right.1);
    best
}

/// Distance from `point` to the nearest point of an axis-aligned segment,
/// together with that point's X.
fn nearest_on_segment(point: (f32, f32), x0: f32, y0: f32, x1: f32, y1: f32) -> (f32, f32) {
    let x = point.0.clamp(x0.min(x1), x0.max(x1));
    let y = point.1.clamp(y0.min(y1), y0.max(y1));
    let dx = point.0 - x;
    let dy = point.1 - y;
    ((dx * dx + dy * dy).sqrt(), x)
}

/// Nearest edge among all rectangles of one group.
fn nearest_rect_edge(
    rects: &[WorldRect],
    feet: (f32, f32),
    head: (f32, f32),
    anchor: (f32, f32),
) -> Option<EdgeCandidate> {
    let mut best: Option<EdgeCandidate> = None;
    for rect in rects {
        let (distance, point_x) = rect_edge(rect, feet, head, anchor);
        if best
            .as_ref()
            .is_none_or(|candidate| distance < candidate.distance)
        {
            best = Some(EdgeCandidate {
                kind: AttentionSource::None,
                distance,
                point_x,
            });
        }
    }
    best
}

/// Nearest screen/work edge. Exact distance ties prefer the work area, then
/// the screen. Both groups outrank window edges regardless of distance.
fn nearest_screen_or_work(
    screens: &[WorldRect],
    work: &[WorldRect],
    feet: (f32, f32),
    head: (f32, f32),
    anchor: (f32, f32),
) -> Option<EdgeCandidate> {
    let screen = nearest_rect_edge(screens, feet, head, anchor).map(|mut candidate| {
        candidate.kind = AttentionSource::ScreenEdge;
        candidate
    });
    let work = nearest_rect_edge(work, feet, head, anchor).map(|mut candidate| {
        candidate.kind = AttentionSource::WorkAreaEdge;
        candidate
    });
    match (screen, work) {
        (Some(screen), Some(work)) => Some(if work.distance <= screen.distance {
            work
        } else {
            screen
        }),
        (Some(screen), None) => Some(screen),
        (None, work) => work,
    }
}

/// Cached rest extents of one model, read from the compiled layers or mesh.
/// Degenerate handmade data falls back to a neutral unit-height body.
fn model_extents(model: &CompiledModel) -> (f32, f32) {
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut any = false;
    let mut visit = |y: f32| {
        if y.is_finite() && y.abs() <= super::MAX_COORDINATE {
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            any = true;
        }
    };
    match &model.data {
        ModelData::Sprite2d(sprite) => {
            for layer in &sprite.layers {
                for vertex in &layer.vertices {
                    visit(vertex[1]);
                }
            }
        }
        ModelData::Skeleton3d(skeleton) => {
            for vertex in &skeleton.vertices {
                visit(vertex.position[1]);
            }
        }
    }
    if any && min_y.is_finite() && max_y.is_finite() && max_y > min_y {
        (min_y, max_y - min_y)
    } else {
        (FALLBACK_BASE_Y, FALLBACK_HEIGHT)
    }
}

/// Bounded root presentation matrix around the model's rest bottom.
///
/// Shear is applied relative to the rest floor and the Y scale pivots on it
/// too, so the feet never sink below the instance origin.
pub(super) fn presentation_matrix(base_y: f32, shear: f32, scale_y: f32) -> Mat4 {
    let pivot = Mat4::from_translation(Vec3::new(0.0, base_y, 0.0));
    let body = Mat4::from_cols(
        Vec4::new(1.0, 0.0, 0.0, 0.0),
        Vec4::new(shear, scale_y, 0.0, 0.0),
        Vec4::new(0.0, 0.0, 1.0, 0.0),
        Vec4::new(0.0, 0.0, 0.0, 1.0),
    );
    pivot * body * Mat4::from_translation(Vec3::new(0.0, -base_y, 0.0))
}
