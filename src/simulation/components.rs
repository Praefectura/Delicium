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

/// First error produced while simulating a tick, if any.
#[derive(Resource, Default)]
pub(crate) struct TickStatus {
    /// Error reported by the last tick; taken by `Simulation::advance`.
    pub error: Option<SimError>,
}
