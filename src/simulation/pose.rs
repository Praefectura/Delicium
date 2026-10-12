//! Per-instance pose evaluation: sampling, interaction drive, interpolation.

use glam::{Quat, Vec3};

use crate::model::{
    CompiledModel, CompiledSkeleton3d, CompiledSprite2d, JointTransform, ModelData,
    try_sample_joints, try_sample_parameters,
};

use super::components::InteractionState;
use super::{HEAD_YAW_LIMIT, InstanceId, SimError};

/// Kind-independent evaluation state of one instance.
#[derive(Clone, Debug)]
pub(crate) enum Pose {
    /// 2D: sampled parameter vector, indexed like
    /// [`CompiledSprite2d::parameters`].
    Sprite2d(Vec<f32>),
    /// 3D: sampled local joint transforms, indexed like
    /// [`CompiledSkeleton3d::joints`].
    Skeleton3d(Vec<JointTransform>),
}

/// Interaction reaction carried into pose evaluation.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DriveInput {
    /// Direction to the neighbour, in world space, if one is within the
    /// interaction radius.
    pub direction: Vec3,
    /// Eased reaction weight in `0.0..=1.0`.
    pub weight: f32,
}

impl DriveInput {
    /// No reaction: used for spawn and as the neutral state.
    pub fn none() -> Self {
        Self::default()
    }

    /// Reads the reaction from an instance's interaction state.
    pub fn from_state(state: &InteractionState) -> Self {
        match state.neighbor {
            Some(_) if state.weight > 0.0 => Self {
                direction: state.offset,
                weight: state.weight.clamp(0.0, 1.0),
            },
            _ => Self::none(),
        }
    }
}

/// Samples the full pose of one instance: animation first, then the
/// interaction drive on top.
///
/// An inability to sample parameters or joints (or a stale binding index in a
/// hand-built model) surfaces as a visible [`SimError`] rather than a silent
/// fallback; the caller keeps its previous state in that case.
pub(crate) fn sample_pose(
    model: &CompiledModel,
    time: f64,
    drive: &DriveInput,
    id: InstanceId,
) -> Result<Pose, SimError> {
    match &model.data {
        ModelData::Sprite2d(sprite) => {
            let mut parameters = try_sample_parameters(sprite, time)?;
            apply_sprite_drive(model, sprite, &mut parameters, drive, id)?;
            Ok(Pose::Sprite2d(parameters))
        }
        ModelData::Skeleton3d(skeleton) => {
            let mut locals = try_sample_joints(skeleton, time)?;
            apply_skeleton_drive(model, skeleton, &mut locals, drive, id)?;
            Ok(Pose::Skeleton3d(locals))
        }
    }
}

/// 2D drive: pull the gaze parameter toward the horizontal neighbour
/// direction, clamped to its declared range.
///
/// The model must declare `interaction.gaze_parameter`; without a binding the
/// reaction is skipped rather than guessing a parameter index.
fn apply_sprite_drive(
    model: &CompiledModel,
    sprite: &CompiledSprite2d,
    parameters: &mut [f32],
    drive: &DriveInput,
    id: InstanceId,
) -> Result<(), SimError> {
    if !active_direction(drive) {
        return Ok(());
    }
    let Some(index) = model.interaction.gaze_parameter else {
        return Ok(());
    };
    if index >= sprite.parameters.len() || index >= parameters.len() {
        return Err(SimError::Inconsistent {
            instance: id,
            detail: format!(
                "gaze_parameter {index} is out of range for {} parameters",
                sprite.parameters.len()
            ),
        });
    }
    let parameter = &sprite.parameters[index];
    let direction = drive.direction.normalize_or_zero();
    // Horizontal component decides the gaze sign: positive X (a neighbour to
    // the right of this instance) drives the parameter toward its maximum.
    let sign = direction.x.clamp(-1.0, 1.0);
    let middle = (parameter.min + parameter.max) * 0.5;
    let half_range = (parameter.max - parameter.min) * 0.5;
    let target = clamp(middle + sign * half_range, parameter.min, parameter.max);
    let value = &mut parameters[index];
    *value = clamp(
        lerp(*value, target, drive.weight),
        parameter.min,
        parameter.max,
    );
    Ok(())
}

/// 3D drive: yaw the head joint toward the neighbour, at most
/// [`HEAD_YAW_LIMIT`] around local Y.
///
/// The model must declare `interaction.head_joint`; a stale index (possible
/// with hand-built compiled data) is reported instead of being ignored.
fn apply_skeleton_drive(
    model: &CompiledModel,
    skeleton: &CompiledSkeleton3d,
    locals: &mut [JointTransform],
    drive: &DriveInput,
    id: InstanceId,
) -> Result<(), SimError> {
    if !active_direction(drive) {
        return Ok(());
    }
    let Some(joint_index) = model.interaction.head_joint else {
        return Ok(());
    };
    if joint_index >= skeleton.joints.len() || joint_index >= locals.len() {
        return Err(SimError::Inconsistent {
            instance: id,
            detail: format!(
                "head_joint {joint_index} is out of range for {} joints",
                skeleton.joints.len()
            ),
        });
    }
    // The model faces +Z; a positive rotation around Y turns +Z toward +X
    // (right-hand rule), so `atan2(dx, dz)` is the yaw that faces the
    // neighbour, clamped to the authored limit and scaled by the weight.
    // The world direction is used directly: the instance root is a pure
    // translation, so world and local orientations coincide.
    let direction = drive.direction.normalize_or_zero();
    let yaw = direction
        .x
        .atan2(direction.z)
        .clamp(-HEAD_YAW_LIMIT, HEAD_YAW_LIMIT);
    let yaw = yaw * drive.weight;
    let local = &mut locals[joint_index];
    local.rotation = (Quat::from_rotation_y(yaw) * local.rotation).normalize();
    Ok(())
}

/// Whether a reaction should be applied at all.
fn active_direction(drive: &DriveInput) -> bool {
    drive.weight > 0.0 && drive.direction.is_finite() && drive.direction.length_squared() > 1e-12
}

/// Interpolates the previous and current 2D parameters with `alpha`.
///
/// Both vectors must match the model; a mismatch is reported as
/// [`SimError::Inconsistent`].
pub(crate) fn interpolated_parameters(
    sprite: &CompiledSprite2d,
    previous: &Pose,
    current: &Pose,
    alpha: f32,
    id: InstanceId,
) -> Result<Vec<f32>, SimError> {
    let (Pose::Sprite2d(previous), Pose::Sprite2d(current)) = (previous, current) else {
        return Err(SimError::Inconsistent {
            instance: id,
            detail: "2D model with a 3D pose".to_string(),
        });
    };
    if previous.len() != current.len() || current.len() != sprite.parameters.len() {
        return Err(SimError::Inconsistent {
            instance: id,
            detail: format!(
                "parameter vectors have lengths {} and {} for {} parameters",
                previous.len(),
                current.len(),
                sprite.parameters.len()
            ),
        });
    }
    let alpha = alpha.clamp(0.0, 1.0);
    let mut out = Vec::with_capacity(current.len());
    for (index, parameter) in sprite.parameters.iter().enumerate() {
        out.push(clamp(
            lerp(previous[index], current[index], alpha),
            parameter.min,
            parameter.max,
        ));
    }
    Ok(out)
}

/// Interpolates the previous and current 3D local poses with `alpha`.
///
/// Translation and scale are lerped, rotation is slerped along the shortest
/// arc; the result stays a valid local pose, so the caller's forward
/// kinematics and skinning keep matrices rigid. A length mismatch or a
/// non-finite value is reported as [`SimError::Inconsistent`].
pub(crate) fn interpolated_locals(
    skeleton: &CompiledSkeleton3d,
    previous: &Pose,
    current: &Pose,
    alpha: f32,
    id: InstanceId,
) -> Result<Vec<JointTransform>, SimError> {
    let (Pose::Skeleton3d(previous), Pose::Skeleton3d(current)) = (previous, current) else {
        return Err(SimError::Inconsistent {
            instance: id,
            detail: "3D model with a 2D pose".to_string(),
        });
    };
    if previous.len() != skeleton.joints.len() || current.len() != skeleton.joints.len() {
        return Err(SimError::Inconsistent {
            instance: id,
            detail: format!(
                "pose vectors have lengths {} and {} for {} joints",
                previous.len(),
                current.len(),
                skeleton.joints.len()
            ),
        });
    }
    let alpha = alpha.clamp(0.0, 1.0);
    let mut out = Vec::with_capacity(current.len());
    for (previous, current) in previous.iter().zip(current.iter()) {
        let ok = |transform: &JointTransform| {
            transform.translation.is_finite()
                && transform.rotation.is_finite()
                && transform.rotation.length_squared() > 1e-8
                && transform.scale.is_finite()
        };
        if !ok(previous) || !ok(current) {
            return Err(SimError::Inconsistent {
                instance: id,
                detail: "joint pose is not a finite, rigid transform".to_string(),
            });
        }
        out.push(JointTransform {
            translation: previous.translation.lerp(current.translation, alpha),
            rotation: previous
                .rotation
                .normalize()
                .slerp(current.rotation.normalize(), alpha)
                .normalize(),
            scale: previous.scale.lerp(current.scale, alpha),
        });
    }
    Ok(out)
}

/// Linear blend of two scalars.
pub(crate) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a * (1.0 - t) + b * t
}

/// Clamps a value into `[min, max]` without panicking on degenerate input.
pub(crate) fn clamp(value: f32, min: f32, max: f32) -> f32 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}
