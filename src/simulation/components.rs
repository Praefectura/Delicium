//! Internal ECS components and resources owned by the simulation.

use std::sync::Arc;

use bevy_ecs::{component::Component, resource::Resource};
use glam::Vec3;

use crate::model::CompiledModel;

use super::pose::Pose;
use super::{AssetId, InstanceId, InteractionEdge, SimError};

/// Asset handle of one instance. Every instance of one asset shares the same
/// `Arc`, while all mutable state lives in the other components.
#[derive(Component)]
pub(crate) struct ModelHandle {
    /// Id of the pooled asset this instance draws.
    pub asset: AssetId,
    /// Shared compiled model.
    pub model: Arc<CompiledModel>,
}

/// Stable public identifier of an instance, mirrored into the ECS so query
/// systems can report meaningful errors.
#[derive(Component)]
pub(crate) struct InstanceTag(pub InstanceId);

/// World-space position; the root transform is a pure translation.
#[derive(Component)]
pub(crate) struct Position(pub Vec3);

/// Instance animation time in seconds, in `f64` so it accumulates without
/// losing resolution over long sessions.
#[derive(Component)]
pub(crate) struct TimeState {
    /// Current animation time in seconds.
    pub time: f64,
}

/// Previous and current tick poses, used for render interpolation.
#[derive(Component)]
pub(crate) struct PoseState {
    /// Pose of the previous tick.
    pub prev: Pose,
    /// Pose of the current tick.
    pub curr: Pose,
}

/// Interaction reaction of one instance, refreshed by every snapshot pass.
#[derive(Component, Default)]
pub(crate) struct InteractionState {
    /// Nearest neighbour inside the interaction radius, if any.
    pub neighbor: Option<InstanceId>,
    /// Unit direction from this instance to its neighbour.
    pub offset: Vec3,
    /// Eased reaction weight in `0.0..=1.0`.
    pub weight: f32,
}

/// Interaction links of the last snapshot, sorted by `(from, to)`.
#[derive(Resource, Default)]
pub(crate) struct InteractionsRes(pub Vec<InteractionEdge>);

/// Short-lived reaction to a pointer poke (a press and release on the
/// instance without a drag).
///
/// Owned by the instance; `elapsed` counts fixed steps from the poke and
/// `direction` is the unit horizontal vector from the instance to the poke
/// point (or exactly zero for a poke on the centre, which keeps the facing).
/// A pristine instance starts out with no reaction: `elapsed` sits at the
/// sentinel [`POKE_REACTION_SECONDS`](super::POKE_REACTION_SECONDS), so a
/// freshly spawned instance is never treated as mid-reaction.
#[derive(Component)]
pub(crate) struct PokeReaction {
    /// Unit direction to the poke point, or `Vec3::ZERO` when dead-zone'd.
    pub direction: Vec3,
    /// Seconds since the poke; the reaction is over at
    /// [`POKE_REACTION_SECONDS`](super::POKE_REACTION_SECONDS).
    pub elapsed: f32,
    /// Accepted poke generation; consumed only after successful pose sampling.
    pub revision: u64,
}

impl Default for PokeReaction {
    fn default() -> Self {
        Self {
            direction: Vec3::ZERO,
            elapsed: super::POKE_REACTION_SECONDS,
            revision: 0,
        }
    }
}

/// Autonomous walking state of one instance.
///
/// The position is only ever written by the wander system (and by external
/// `set_position` calls, which cancel an ongoing walk); this component owns
/// the phase machine, the deterministic random stream and the mirror facing.
#[derive(Component)]
pub(crate) struct WanderState {
    /// Current phase of the idle/walk cycle.
    pub phase: WanderPhase,
    /// Mirror sign, always exactly `+1.0` (as authored) or `-1.0` (mirrored
    /// horizontally around the instance's own vertical centre line). Only 2D
    /// sprites mirror; a 3D skeleton keeps `+1.0` so its authored orientation
    /// (and asymmetric details such as a scarf print) never flips, and turns
    /// through `WanderState::turn` instead.
    pub facing: f32,
    /// 3D presentation turn in `-1.0..=1.0`, eased per tick toward
    /// `WanderState::turn_target`. `+1.0` keeps the authored orientation;
    /// `-1.0` reflects the authored root yaw about the model's front axis.
    /// Reflecting the yaw as a rotation is what faces a skeleton along its
    /// walk direction without mirroring its geometry, so asymmetric prints
    /// stay readable. 2D sprites keep `+1.0` and use `facing`.
    pub turn: f32,
    /// Walk direction the turn eases toward: `+1.0` right, `-1.0` left. The
    /// last walked direction is kept while idling, exactly like `facing`.
    pub turn_target: f32,
    /// Per-instance splitmix64 stream, seeded from the instance id so the
    /// walk is deterministic and independent of tick order or thread count.
    pub rng: u64,
    /// Set while a user gesture owns the instance (drag press or control
    /// window position edit); a suspended instance never moves on its own.
    pub suspended: bool,
    /// Whether this tick actually changed X through autonomous walking.
    pub moved_this_tick: bool,
}

/// The idle/walk cycle of one wandering instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WanderPhase {
    /// Standing still for `remaining` seconds before choosing a target.
    Idle { remaining: f32 },
    /// Walking horizontally toward `target_x`.
    Walking { target_x: f32 },
}

/// Horizontal world bounds wandering is allowed in; `None` disables walking
/// entirely (the default, so headless tests and pinned workloads stay put).
#[derive(Resource, Default, Clone, Copy, Debug)]
pub(crate) struct WanderBoundsRes(pub Option<super::WorldXBounds>);

/// Optional physical-desktop visibility expressed in world coordinates.
#[derive(Resource, Default)]
pub(crate) struct WanderRegionsRes(pub Option<Vec<super::WorldRect>>);

/// Observable wander transitions produced by the last tick, drained by the
/// application layer for logging. The simulation never reads them back, so
/// dropping events cannot change any state.
#[derive(Resource, Default)]
pub(crate) struct WanderEventsRes(pub Vec<super::WanderEvent>);

/// First error produced while simulating a tick, if any.
#[derive(Resource, Default)]
pub(crate) struct TickStatus {
    /// Error reported by the last tick; taken by `Simulation::advance`.
    pub error: Option<SimError>,
}
