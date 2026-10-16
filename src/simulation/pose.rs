//! Per-instance pose evaluation: sampling, interaction drive, interpolation.

use glam::{Quat, Vec3};

use crate::model::{
    CompiledModel, CompiledSkeleton3d, CompiledSprite2d, JointTransform, ModelData,
    try_sample_joints, try_sample_parameters,
};

use super::components::{InteractionState, PokeReaction};
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
#[derive(Clone, Copy, Debug)]
pub(crate) struct DriveInput {
    /// Direction to the neighbour, in world space, if one is within the
    /// interaction radius.
    pub direction: Vec3,
    /// Eased reaction weight in `0.0..=1.0`.
    pub weight: f32,
    /// Mirror sign of the instance root (`+1.0` or `-1.0`). A mirrored root
    /// flips the world direction of every local rotation around Y and of
    /// every horizontal gaze sign, so the drive must multiply through it.
    pub facing: f32,
}

impl Default for DriveInput {
    fn default() -> Self {
        Self {
            direction: Vec3::ZERO,
            weight: 0.0,
            facing: 1.0,
        }
    }
}

impl DriveInput {
    /// No reaction: used for spawn and as the neutral state.
    pub fn none() -> Self {
        Self::default()
    }

    /// Reads the reaction from an instance's interaction state under its
    /// current mirror facing.
    pub fn from_state(state: &InteractionState, facing: f32) -> Self {
        match state.neighbor {
            Some(_) if state.weight > 0.0 => Self {
                direction: state.offset,
                weight: state.weight.clamp(0.0, 1.0),
                facing,
            },
            _ => Self {
                facing,
                ..Self::none()
            },
        }
    }

    /// Reads the turn from a poke reaction under the instance's current
    /// mirror facing. The weight follows the poke envelope, so the turn
    /// fades in and out on its own; a dead-zone'd direction (a poke on the
    /// centre) yields no turn at all.
    pub fn from_reaction(reaction: &PokeReaction, facing: f32) -> Self {
        Self {
            direction: reaction.direction,
            weight: super::systems::poke_envelope(reaction.elapsed).clamp(0.0, 1.0),
            facing,
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
    // Under a mirrored root the same world direction needs the opposite
    // parameter sign, so the facing multiplies through.
    let sign = direction.x.clamp(-1.0, 1.0) * drive.facing;
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
    // The instance root is a translation times a horizontal mirror, so a
    // mirrored root flips the world direction of every yaw: multiplying by
    // the facing keeps the same local rotation pointing at the neighbour.
    let direction = drive.direction.normalize_or_zero();
    let yaw = direction
        .x
        .atan2(direction.z)
        .clamp(-HEAD_YAW_LIMIT, HEAD_YAW_LIMIT);
    let yaw = yaw * drive.weight * drive.facing;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LoadLimits, load_model_bytes};

    /// A minimal skeleton with a steerable head joint, matching the bundled
    /// robot's `interaction.head_joint = "head"` binding shape.
    fn skeleton_model() -> std::sync::Arc<CompiledModel> {
        let document = serde_json::json!({
            "format": "delicium.model", "format_version": "0.1", "name": "drive probe",
            "kind": "skeleton3d",
            "interaction": {"head_joint": "head"},
            "skeleton3d": {
                "vertices": [
                    {"position": [-0.5, 1.0, 0.0], "normal": [0.0, 0.0, 1.0], "color": [1.0, 1.0, 1.0, 1.0], "joints": [0, 0, 0, 0], "weights": [1.0, 0.0, 0.0, 0.0]},
                    {"position": [0.5, 1.0, 0.0], "normal": [0.0, 0.0, 1.0], "color": [1.0, 1.0, 1.0, 1.0], "joints": [0, 0, 0, 0], "weights": [1.0, 0.0, 0.0, 0.0]},
                    {"position": [0.0, 2.0, 0.0], "normal": [0.0, 0.0, 1.0], "color": [1.0, 1.0, 1.0, 1.0], "joints": [0, 0, 0, 0], "weights": [1.0, 0.0, 0.0, 0.0]}
                ],
                "indices": [0, 1, 2],
                "joints": [
                    {"name": "head", "parent": null, "translation": [0.0, 1.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0], "scale": [1.0, 1.0, 1.0]},
                    {"name": "root", "parent": null, "translation": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0], "scale": [1.0, 1.0, 1.0]}
                ],
                "animations": []
            }
        });
        load_model_bytes(
            &serde_json::to_vec(&document).unwrap(),
            &LoadLimits::default(),
        )
        .unwrap()
    }

    /// A minimal sprite with a `gaze_x` parameter and the matching binding.
    fn sprite_model() -> std::sync::Arc<CompiledModel> {
        let document = serde_json::json!({
            "format": "delicium.model", "format_version": "0.1", "name": "gaze probe",
            "kind": "sprite2d",
            "interaction": {"gaze_parameter": "gaze_x"},
            "sprite2d": {
                "parameters": [{"name": "gaze_x", "min": -1.0, "max": 1.0, "default": 0.0}],
                "layers": [{
                    "name": "body", "color": [1.0, 1.0, 1.0, 1.0],
                    "vertices": [[-0.5, 0.0], [0.5, 0.0], [0.0, 1.0]],
                    "indices": [0, 1, 2]
                }],
                "animations": []
            }
        });
        load_model_bytes(
            &serde_json::to_vec(&document).unwrap(),
            &LoadLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn skeleton_drive_respects_the_instance_facing() {
        let model = skeleton_model();
        let ModelData::Skeleton3d(skeleton) = &model.data else {
            panic!("expected a skeleton model");
        };
        let locals = vec![JointTransform::default(); skeleton.joints.len()];
        let drive_right = DriveInput {
            direction: Vec3::new(1.0, 0.0, 0.0),
            weight: 1.0,
            facing: 1.0,
        };
        let mut positive = locals.clone();
        apply_skeleton_drive(&model, skeleton, &mut positive, &drive_right, 1).unwrap();
        let mut mirrored = locals.clone();
        let drive_mirrored = DriveInput {
            facing: -1.0,
            ..drive_right
        };
        apply_skeleton_drive(&model, skeleton, &mut mirrored, &drive_mirrored, 1).unwrap();
        // The head joint index inside the probe model.
        let head = skeleton
            .joints
            .iter()
            .position(|joint| joint.name == "head")
            .unwrap();
        // Unmirrored: yaw = +HEAD_YAW_LIMIT; mirrored: yaw = -HEAD_YAW_LIMIT.
        // The relation mirrored = R_y(-2*limit) composed on the unmirrored
        // rotation must hold; quaternion equality (up to sign) is |dot| = 1.
        let expected = Quat::from_rotation_y(-2.0 * HEAD_YAW_LIMIT) * positive[head].rotation;
        let dot = mirrored[head].rotation.dot(expected).abs();
        assert!(
            dot > 1.0 - 1e-6,
            "mirrored head rotation is not the -2*yaw composition: {:?} vs {:?}",
            mirrored[head].rotation,
            expected
        );
    }

    #[test]
    fn reaction_drive_follows_the_envelope_and_dead_zone() {
        let mut reaction = PokeReaction {
            direction: Vec3::new(1.0, 0.0, 0.0),
            elapsed: 0.0,
        };
        // A fresh poke has an all-but-zero weight, the peak is 1 and the end
        // of the reaction is exactly zero again.
        assert!(DriveInput::from_reaction(&reaction, 1.0).weight < 1e-6);
        reaction.elapsed = super::super::POKE_REACTION_SECONDS * 0.15;
        let active = DriveInput::from_reaction(&reaction, 1.0);
        assert!((active.weight - 1.0).abs() < 1e-6);
        assert_eq!(active.direction, Vec3::new(1.0, 0.0, 0.0));
        reaction.elapsed = super::super::POKE_REACTION_SECONDS;
        assert_eq!(DriveInput::from_reaction(&reaction, 1.0).weight, 0.0);
        // A dead-zone'd direction never carries a turn, even at full weight:
        // the pose gate refuses it.
        reaction.elapsed = super::super::POKE_REACTION_SECONDS * 0.15;
        reaction.direction = Vec3::ZERO;
        let centred = DriveInput::from_reaction(&reaction, 1.0);
        assert!(centred.weight > 0.0);
        assert!(!active_direction(&centred));
    }

    #[test]
    fn sprite_drive_sign_follows_the_facing() {
        let model = sprite_model();
        let ModelData::Sprite2d(sprite) = &model.data else {
            panic!("expected a sprite model");
        };
        let drive = |x: f32, facing: f32| DriveInput {
            direction: Vec3::new(x, 0.0, 0.0),
            weight: 1.0,
            facing,
        };
        let probe = |drive: &DriveInput| {
            let mut parameters = vec![0.0f32; sprite.parameters.len()];
            apply_sprite_drive(&model, sprite, &mut parameters, drive, 1).unwrap();
            parameters
        };
        // A neighbour to the right while unmirrored and a neighbour to the
        // right while mirrored must drive the parameter to opposite ends.
        let unmirrored = probe(&drive(1.0, 1.0));
        let mirrored = probe(&drive(1.0, -1.0));
        assert_eq!(unmirrored[0], 1.0);
        assert_eq!(mirrored[0], -1.0);
        // Mirroring the root and flipping the world direction reproduces the
        // unmirrored result: the same neighbour seen through a mirror.
        assert_eq!(probe(&drive(-1.0, -1.0)), unmirrored);
    }
}
