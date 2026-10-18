//! Runtime evaluation on compiled assets: parameter sampling, 2D layer
//! deformation, 3D joint sampling and skinning matrices.
//!
//! Two layers of API, deliberately:
//!
//! - the **infallible** functions named in the frozen contract
//!   ([`sample_parameters`], [`deform_layer`], [`sample_joints`],
//!   [`skin_matrices`]) never panic and never return `NaN`: an inconsistent
//!   call degrades to the neutral result (defaults, bind pose, rest vertices
//!   or an empty matrix list);
//! - the **fallible** `try_*` variants report the same situations as
//!   [`ModelError`], for callers that want to surface the problem.
//!
//! The infallible wrappers treat invalid times as `0.0`; the fallible forms
//! reject them. Every animation wraps modulo its own duration. All
//! indices coming from a compiled asset are bounds-checked here as well;
//! compiled data is trusted in shape but not assumed infallible.
//!
//! [`try_sample_parameters_clip`] and [`try_sample_joints_clip`] sample a
//! single selected animation (the action-role path): `Loop` wraps modulo the
//! clip duration like the classic path (in `f64`, before narrowing), while
//! `Clamp` holds the final keys and never wraps back to the first frame.
//! They are deliberately fallible only: no infallible wrapper, no silent
//! fallback.

use glam::{Mat4, Quat, Vec3};

use crate::model::error::ModelError;
use crate::model::types::{
    AnimationInterpolation, ClipPlayback, CompiledSkeleton3d, CompiledSprite2d, Deformer2d,
    JointTransform, Parameter, ParameterAnimation, RotationKey, ScalarKey, ScaleKey,
    SkeletalAnimation, TranslationKey,
};

/// Upper bound on a coordinate produced by deformation. Well outside any
/// authored sprite, but small enough that downstream interpolation stays
/// finite.
const MAX_DEFORMED_COORD: f32 = 1e7;

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// Maps any user-supplied time onto a safe non-negative, finite value.
fn sanitize_time(time: f64) -> f64 {
    if time.is_finite() && time > 0.0 {
        time
    } else {
        0.0
    }
}

fn checked_time(time: f64) -> Result<f64, ModelError> {
    if !time.is_finite() || time < 0.0 {
        return Err(ModelError::InvalidInput {
            detail: "animation time must be finite and non-negative".into(),
        });
    }
    Ok(time)
}

fn wrap_time(time: f64, duration: f32) -> f32 {
    // Reduce each clip's phase before narrowing long-running instance time.
    let wrapped = (time % f64::from(duration)) as f32;
    if wrapped.is_finite() { wrapped } else { 0.0 }
}

/// Maps `time` onto the clip's local sampling time according to `playback`.
///
/// `Loop` reuses [`wrap_time`] (f64 modulo first, then narrow); `Clamp`
/// narrows to `[0, duration]` in f64 and never wraps the endpoint back to the
/// first frame. `duration` has already passed [`check_duration`].
fn clip_local_time(time: f64, duration: f32, playback: ClipPlayback) -> f32 {
    match playback {
        ClipPlayback::Loop => wrap_time(time, duration),
        ClipPlayback::Clamp => {
            let local = time.clamp(0.0, f64::from(duration)) as f32;
            if local.is_finite() { local } else { 0.0 }
        }
    }
}

fn check_duration(duration: f32, animation: &str) -> Result<(), ModelError> {
    if !duration.is_finite() || duration <= 0.0 {
        return Err(ModelError::InvalidInput {
            detail: format!("animation `{animation}` has an invalid duration {duration}"),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 2D: parameters
// ---------------------------------------------------------------------------

/// Samples every parameter at `time`, starting from the declared defaults.
///
/// Returns exactly `model.parameters.len()` values, each inside its declared
/// range. Never panics: non-finite or negative `time` is treated as `0.0`, and
/// an internally inconsistent model yields the defaults.
pub fn sample_parameters(model: &CompiledSprite2d, time: f64) -> Vec<f32> {
    try_sample_parameters(model, sanitize_time(time))
        .unwrap_or_else(|_| model.parameters.iter().map(|p| p.default).collect())
}

/// Fallible form of [`sample_parameters`]: reports inconsistent compiled data
/// instead of degrading.
pub fn try_sample_parameters(model: &CompiledSprite2d, time: f64) -> Result<Vec<f32>, ModelError> {
    let time = checked_time(time)?;
    let mut values: Vec<f32> = model.parameters.iter().map(|p| p.default).collect();

    for animation in &model.animations {
        check_duration(animation.duration, &animation.name)?;
        let local_time = wrap_time(time, animation.duration);
        apply_parameter_animation(model, animation, local_time, &mut values)?;
    }

    if !values.iter().all(|v| v.is_finite()) {
        return Err(ModelError::InvalidInput {
            detail: "sampled parameter values are not finite".to_string(),
        });
    }
    Ok(values)
}

/// Samples the parameter animation at `clip_index` at `time`, starting from
/// the declared defaults; other animations are not applied.
///
/// `Loop` wraps modulo the clip duration exactly like
/// [`try_sample_parameters`]; `Clamp` holds `[0, duration]` and never wraps
/// the final frame back to the first. An out-of-range `clip_index` is an
/// explicit [`ModelError::OutOfRange`]; there is no silent fallback.
pub fn try_sample_parameters_clip(
    model: &CompiledSprite2d,
    clip_index: usize,
    time: f64,
    playback: ClipPlayback,
) -> Result<Vec<f32>, ModelError> {
    let time = checked_time(time)?;
    let animation = model
        .animations
        .get(clip_index)
        .ok_or_else(|| ModelError::OutOfRange {
            context: "clip animation".to_string(),
            index: clip_index,
            len: model.animations.len(),
        })?;
    check_duration(animation.duration, &animation.name)?;

    let local_time = clip_local_time(time, animation.duration, playback);
    let mut values: Vec<f32> = model.parameters.iter().map(|p| p.default).collect();
    apply_parameter_animation(model, animation, local_time, &mut values)?;

    if !values.iter().all(|v| v.is_finite()) {
        return Err(ModelError::InvalidInput {
            detail: "sampled parameter values are not finite".to_string(),
        });
    }
    Ok(values)
}

/// Applies one parameter animation's channels at `local_time` (already mapped
/// onto the clip) to `values`. Shared by the all-animation path and the
/// single-clip path.
fn apply_parameter_animation(
    model: &CompiledSprite2d,
    animation: &ParameterAnimation,
    local_time: f32,
    values: &mut [f32],
) -> Result<(), ModelError> {
    for channel in &animation.channels {
        let parameter = model
            .parameters
            .get(channel.parameter)
            .ok_or(ModelError::OutOfRange {
                context: format!("animation `{}` channel", animation.name),
                index: channel.parameter,
                len: model.parameters.len(),
            })?;
        let value = sample_scalar_keys(&channel.keys, local_time, animation.duration)?;
        values[channel.parameter] = clamp(value, parameter.min, parameter.max);
    }
    Ok(())
}

/// Linear sampling with clamping to the first/last key inside `[0, duration]`.
fn sample_scalar_keys(keys: &[ScalarKey], time: f32, duration: f32) -> Result<f32, ModelError> {
    let first = keys.first().ok_or_else(|| ModelError::InvalidInput {
        detail: "animation channel has no keys".to_string(),
    })?;
    let mut previous = f32::NEG_INFINITY;
    for key in keys {
        if !key.time.is_finite() || !key.value.is_finite() {
            return Err(ModelError::InvalidInput {
                detail: "animation keys contain non-finite values".to_string(),
            });
        }
        if key.time < 0.0 || key.time > duration || key.time <= previous {
            return Err(ModelError::InvalidInput {
                detail: "animation keys are not strictly increasing inside the duration"
                    .to_string(),
            });
        }
        previous = key.time;
    }

    if time <= first.time {
        return Ok(first.value);
    }
    let last = keys[keys.len() - 1];
    if time >= last.time {
        return Ok(last.value);
    }

    // `partition_point` gives the first key after `time`; both neighbours
    // exist because `first.time < time < last.time`.
    let next = keys.partition_point(|k| k.time <= time);
    let a = keys[next - 1];
    let b = keys[next];
    let span = b.time - a.time;
    let fraction = if span > 0.0 {
        (time - a.time) / span
    } else {
        1.0
    };
    let value = a.value + (b.value - a.value) * fraction;
    if !value.is_finite() {
        return Err(ModelError::InvalidInput {
            detail: "interpolated parameter value is not finite".to_string(),
        });
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// 2D: deformation
// ---------------------------------------------------------------------------

/// Deforms the vertices of layer `layer_index` with `parameters`.
///
/// Returns the layer's vertices unchanged when `layer_index` is out of range,
/// when `parameters` does not match the model's parameter count, or when the
/// model is internally inconsistent; this function never panics. Use
/// [`try_deform_layer`] to distinguish those cases.
pub fn deform_layer(
    model: &CompiledSprite2d,
    layer_index: usize,
    parameters: &[f32],
) -> Vec<[f32; 2]> {
    // The checked core distinguishes real input errors from a merely
    // inconsistent model, so this path cannot panic on either.
    match deform_layer_checked(model, layer_index, parameters) {
        Ok(points) => points,
        Err(ModelError::OutOfRange { .. }) | Err(ModelError::InvalidInput { .. }) => {
            // Out-of-range layer -> empty; mismatched parameters -> rest pose.
            model
                .layers
                .get(layer_index)
                .map(|layer| layer.vertices.clone())
                .unwrap_or_default()
        }
        Err(_) => Vec::new(),
    }
}

/// Fallible form of [`deform_layer`].
///
/// The deformer chain of the layer (`parent` links) is applied from the
/// outermost ancestor inwards, then the layer's own deformer: rotation is
/// `parameters[p] * max_angle_radians` around `pivot`, warp is the bilinear
/// displacement scaled by `parameters[p]`. Both scale with the sign of the
/// parameter value.
pub fn try_deform_layer(
    model: &CompiledSprite2d,
    layer_index: usize,
    parameters: &[f32],
) -> Result<Vec<[f32; 2]>, ModelError> {
    deform_layer_checked(model, layer_index, parameters)
}

/// Shared core: validates the call, then applies the deformer chain and the
/// output guards. Reached by both the contract function and its `try_*` twin,
/// so neither can panic on a mutated or inconsistent model.
fn deform_layer_checked(
    model: &CompiledSprite2d,
    layer_index: usize,
    parameters: &[f32],
) -> Result<Vec<[f32; 2]>, ModelError> {
    let layer = model
        .layers
        .get(layer_index)
        .ok_or_else(|| ModelError::OutOfRange {
            context: "layer".to_string(),
            index: layer_index,
            len: model.layers.len(),
        })?;
    if parameters.len() != model.parameters.len() {
        return Err(ModelError::InvalidInput {
            detail: format!(
                "expected {} parameter values, got {}",
                model.parameters.len(),
                parameters.len()
            ),
        });
    }
    for (i, value) in parameters.iter().enumerate() {
        if !value.is_finite() {
            return Err(ModelError::InvalidInput {
                detail: format!("parameter value at index {i} is not finite"),
            });
        }
    }

    let mut points = layer.vertices.clone();
    if let Some(root) = layer.deformer {
        // Walk up to the outermost ancestor, bounded by the deformer count so
        // a malformed parent chain cannot loop forever.
        let mut chain = Vec::new();
        let mut cursor = Some(root);
        while let Some(index) = cursor {
            let deformer = model
                .deformers
                .get(index)
                .ok_or_else(|| ModelError::OutOfRange {
                    context: "deformer".to_string(),
                    index,
                    len: model.deformers.len(),
                })?;
            chain.push(index);
            if chain.len() > model.deformers.len() {
                return Err(ModelError::InvalidInput {
                    detail: "deformer chain is longer than the deformer list".to_string(),
                });
            }
            cursor = deformer_parent(deformer);
        }
        chain.reverse();

        for index in chain {
            let deformer = &model.deformers[index];
            apply_deformer(deformer, &model.parameters, parameters, &mut points)?;
        }
    }

    for point in &mut points {
        if !point[0].is_finite() || !point[1].is_finite() {
            return Err(ModelError::InvalidInput {
                detail: "deformed vertex is not finite".to_string(),
            });
        }
        if point[0].abs() > MAX_DEFORMED_COORD || point[1].abs() > MAX_DEFORMED_COORD {
            return Err(ModelError::InvalidInput {
                detail: "deformed vertex exceeds the supported coordinate range".to_string(),
            });
        }
    }
    Ok(points)
}

fn deformer_parent(deformer: &Deformer2d) -> Option<usize> {
    match deformer {
        Deformer2d::Rotation { parent, .. } => *parent,
        Deformer2d::Warp { parent, .. } => *parent,
    }
}

fn apply_deformer(
    deformer: &Deformer2d,
    parameters: &[Parameter],
    values: &[f32],
    points: &mut [[f32; 2]],
) -> Result<(), ModelError> {
    match deformer {
        Deformer2d::Rotation {
            parameter,
            pivot,
            max_angle_radians,
            ..
        } => {
            let value = checked_parameter(parameters, values, *parameter)?;
            let angle = value * *max_angle_radians;
            if !angle.is_finite() {
                return Err(ModelError::InvalidInput {
                    detail: "rotation deformer angle overflows".to_string(),
                });
            }
            if angle == 0.0 {
                return Ok(());
            }
            let (sin, cos) = angle.sin_cos();
            for point in points.iter_mut() {
                let x = point[0] - pivot[0];
                let y = point[1] - pivot[1];
                point[0] = pivot[0] + cos * x - sin * y;
                point[1] = pivot[1] + sin * x + cos * y;
            }
            Ok(())
        }
        Deformer2d::Warp {
            parameter,
            bounds,
            grid,
            offsets,
            ..
        } => {
            let value = checked_parameter(parameters, values, *parameter)?;

            let [x0, y0, width, height] = *bounds;
            if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                return Err(ModelError::InvalidInput {
                    detail: "warp bounds are degenerate".to_string(),
                });
            }
            let (cols, rows) = (grid[0], grid[1]);
            if cols < 2 || rows < 2 {
                return Err(ModelError::InvalidInput {
                    detail: "warp control grid needs both dimensions >= 2".to_string(),
                });
            }
            let expected = cols.checked_mul(rows).ok_or(ModelError::SizeOverflow {
                what: "warp control offsets",
            })?;
            if offsets.len() != expected {
                return Err(ModelError::InvalidInput {
                    detail: format!(
                        "warp grid is {cols}x{rows} but holds {} offsets",
                        offsets.len()
                    ),
                });
            }

            let max_u = (cols - 1) as f32;
            let max_v = (rows - 1) as f32;
            for point in points.iter_mut() {
                let u = ((point[0] - x0) / width).clamp(0.0, 1.0) * max_u;
                let v = ((point[1] - y0) / height).clamp(0.0, 1.0) * max_v;

                let cell_x = (u.floor() as usize).min(cols - 2);
                let cell_y = (v.floor() as usize).min(rows - 2);
                let fx = u - cell_x as f32;
                let fy = v - cell_y as f32;

                let row0 = cell_y * cols;
                let row1 = row0 + cols;
                let o00 = offsets[row0 + cell_x];
                let o10 = offsets[row0 + cell_x + 1];
                let o01 = offsets[row1 + cell_x];
                let o11 = offsets[row1 + cell_x + 1];

                // Bilinear blend of the four surrounding control offsets.
                let dx = lerp(lerp(o00[0], o10[0], fx), lerp(o01[0], o11[0], fx), fy);
                let dy = lerp(lerp(o00[1], o10[1], fx), lerp(o01[1], o11[1], fx), fy);

                point[0] += value * dx;
                point[1] += value * dy;
            }
            Ok(())
        }
    }
}

fn checked_parameter(
    parameters: &[Parameter],
    values: &[f32],
    index: usize,
) -> Result<f32, ModelError> {
    let parameter = parameters.get(index).ok_or(ModelError::OutOfRange {
        context: "deformer parameter".to_string(),
        index,
        len: parameters.len(),
    })?;
    let value = *values.get(index).ok_or(ModelError::OutOfRange {
        context: "deformer parameter value".to_string(),
        index,
        len: values.len(),
    })?;
    if !value.is_finite() {
        return Err(ModelError::InvalidInput {
            detail: format!("parameter `{}` value is not finite", parameter.name),
        });
    }
    Ok(value)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

// ---------------------------------------------------------------------------
// 3D: joints
// ---------------------------------------------------------------------------

/// Samples the local pose of every joint at `time`, starting from the bind
/// pose.
///
/// Returns exactly `model.joints.len()` transforms. Never panics: bad times
/// are treated as `0.0`, and an internally inconsistent model yields the bind
/// pose. Use [`try_sample_joints`] to distinguish those cases.
pub fn sample_joints(model: &CompiledSkeleton3d, time: f64) -> Vec<JointTransform> {
    try_sample_joints(model, sanitize_time(time)).unwrap_or_else(|_| bind_pose(model))
}

/// Fallible form of [`sample_joints`].
pub fn try_sample_joints(
    model: &CompiledSkeleton3d,
    time: f64,
) -> Result<Vec<JointTransform>, ModelError> {
    let time = checked_time(time)?;
    let mut locals = bind_pose(model);

    for animation in &model.animations {
        check_duration(animation.duration, &animation.name)?;
        let local_time = wrap_time(time, animation.duration);
        apply_skeletal_animation(model, animation, local_time, &mut locals)?;
    }

    check_joint_pose(&locals)?;
    Ok(locals)
}

/// Samples the joint animation at `clip_index` at `time`, starting from the
/// bind pose; other animations are not applied.
///
/// `Loop` wraps modulo the clip duration exactly like [`try_sample_joints`];
/// `Clamp` holds `[0, duration]` and never wraps the final frame back to the
/// first. An out-of-range `clip_index` is an explicit
/// [`ModelError::OutOfRange`]; there is no silent fallback.
pub fn try_sample_joints_clip(
    model: &CompiledSkeleton3d,
    clip_index: usize,
    time: f64,
    playback: ClipPlayback,
) -> Result<Vec<JointTransform>, ModelError> {
    let time = checked_time(time)?;
    let animation = model
        .animations
        .get(clip_index)
        .ok_or_else(|| ModelError::OutOfRange {
            context: "clip animation".to_string(),
            index: clip_index,
            len: model.animations.len(),
        })?;
    check_duration(animation.duration, &animation.name)?;

    let local_time = clip_local_time(time, animation.duration, playback);
    let mut locals = bind_pose(model);
    apply_skeletal_animation(model, animation, local_time, &mut locals)?;

    check_joint_pose(&locals)?;
    Ok(locals)
}

/// Applies one skeletal animation's channels at `local_time` (already mapped
/// onto the clip) to `locals`. Shared by the all-animation path and the
/// single-clip path.
fn apply_skeletal_animation(
    model: &CompiledSkeleton3d,
    animation: &SkeletalAnimation,
    local_time: f32,
    locals: &mut [JointTransform],
) -> Result<(), ModelError> {
    for channel in &animation.channels {
        let slot = locals
            .get_mut(channel.joint)
            .ok_or(ModelError::OutOfRange {
                context: format!("animation `{}` channel joint", animation.name),
                index: channel.joint,
                len: model.joints.len(),
            })?;
        let step = channel.interpolation == AnimationInterpolation::Step;

        if !channel.translation.is_empty() {
            slot.translation = sample_translation(
                &channel.translation,
                local_time,
                animation.duration,
                step,
                &animation.name,
            )?;
        }
        if !channel.rotation.is_empty() {
            slot.rotation = sample_rotation(
                &channel.rotation,
                local_time,
                animation.duration,
                step,
                &animation.name,
            )?;
        }
        if !channel.scale.is_empty() {
            slot.scale = sample_scale(
                &channel.scale,
                local_time,
                animation.duration,
                step,
                &animation.name,
            )?;
        }
    }
    Ok(())
}

/// Rejects a pose with a non-finite component; runs after every sampling
/// path so neither entry point can hand out `NaN`s.
fn check_joint_pose(locals: &[JointTransform]) -> Result<(), ModelError> {
    for local in locals {
        if !local.translation.is_finite() || !local.rotation.is_finite() || !local.scale.is_finite()
        {
            return Err(ModelError::InvalidInput {
                detail: "sampled joint pose contains non-finite values".to_string(),
            });
        }
    }
    Ok(())
}

fn bind_pose(model: &CompiledSkeleton3d) -> Vec<JointTransform> {
    model
        .joints
        .iter()
        .map(|joint| JointTransform {
            translation: joint.translation,
            rotation: joint.rotation,
            scale: joint.scale,
        })
        .collect()
}

/// Validates a track's times and returns them, so sampling can binary-search
/// without re-validating.
fn checked_times(
    times: impl Iterator<Item = f32> + Clone,
    count: usize,
    duration: f32,
    animation: &str,
) -> Result<Vec<f32>, ModelError> {
    let mut out = Vec::with_capacity(count);
    let mut previous = f32::NEG_INFINITY;
    for time in times {
        if !time.is_finite() || time < 0.0 || time > duration || time <= previous {
            return Err(ModelError::InvalidInput {
                detail: format!("animation `{animation}` has out-of-order or non-finite key times"),
            });
        }
        previous = time;
        out.push(time);
    }
    Ok(out)
}

/// Index pair bracketing `time`, plus the interpolation fraction.
fn locate(times: &[f32], time: f32) -> (usize, usize, f32) {
    let last = times.len() - 1;
    if time <= times[0] {
        return (0, 0, 0.0);
    }
    if time >= times[last] {
        return (last, last, 0.0);
    }
    let mut lo = 0usize;
    let mut hi = last;
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if times[mid] <= time {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let span = times[hi] - times[lo];
    let fraction = if span > 0.0 {
        (time - times[lo]) / span
    } else {
        1.0
    };
    (lo, hi, fraction)
}

fn sample_translation(
    keys: &[TranslationKey],
    time: f32,
    duration: f32,
    step: bool,
    animation: &str,
) -> Result<Vec3, ModelError> {
    let times = checked_times(keys.iter().map(|k| k.time), keys.len(), duration, animation)?;
    let (a, b, fraction) = locate(&times, time);
    if !keys.iter().all(|k| k.value.is_finite()) {
        return Err(ModelError::InvalidInput {
            detail: format!("animation `{animation}` has non-finite translation keys"),
        });
    }
    let value = if step || a == b {
        keys[a].value
    } else {
        keys[a].value.lerp(keys[b].value, fraction)
    };
    if !value.is_finite() {
        return Err(ModelError::InvalidInput {
            detail: format!("animation `{animation}` produced a non-finite translation"),
        });
    }
    Ok(value)
}

fn sample_rotation(
    keys: &[RotationKey],
    time: f32,
    duration: f32,
    step: bool,
    animation: &str,
) -> Result<Quat, ModelError> {
    let times = checked_times(keys.iter().map(|k| k.time), keys.len(), duration, animation)?;
    for key in keys {
        if !key.value.is_finite() || key.value.length_squared() < 1e-8 {
            return Err(ModelError::InvalidInput {
                detail: format!("animation `{animation}` has an invalid rotation key"),
            });
        }
    }
    let (a, b, fraction) = locate(&times, time);
    let value = if step || a == b {
        keys[a].value.normalize()
    } else {
        // Shortest-arc interpolation; keys are unit quaternions.
        let (ka, kb) = (keys[a].value, keys[b].value);
        let kb = if ka.dot(kb) < 0.0 { -kb } else { kb };
        ka.slerp(kb, fraction)
    };
    if !value.is_finite() {
        return Err(ModelError::InvalidInput {
            detail: format!("animation `{animation}` produced a non-finite rotation"),
        });
    }
    Ok(value.normalize())
}

fn sample_scale(
    keys: &[ScaleKey],
    time: f32,
    duration: f32,
    step: bool,
    animation: &str,
) -> Result<Vec3, ModelError> {
    let times = checked_times(keys.iter().map(|k| k.time), keys.len(), duration, animation)?;
    if !keys.iter().all(|k| k.value.is_finite()) {
        return Err(ModelError::InvalidInput {
            detail: format!("animation `{animation}` has non-finite scale keys"),
        });
    }
    let (a, b, fraction) = locate(&times, time);
    let value = if step || a == b {
        keys[a].value
    } else {
        keys[a].value.lerp(keys[b].value, fraction)
    };
    // Signs are validated to be stable and non-zero, so a linear blend cannot
    // reach or cross zero; re-check anyway because this is untrusted-derived.
    if !value.is_finite() || value.x == 0.0 || value.y == 0.0 || value.z == 0.0 {
        return Err(ModelError::InvalidInput {
            detail: format!("animation `{animation}` produced a singular or non-finite scale"),
        });
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// 3D: skinning
// ---------------------------------------------------------------------------

/// Builds the skinning matrix of every joint from local poses:
/// `world * inverse_bind`.
///
/// Returns an empty vector when `locals` does not match the joint count or
/// when a matrix overflows; the fallible [`try_skin_matrices`] reports those
/// cases explicitly. Indices of `locals` follow
/// [`CompiledSkeleton3d::joints`] order (parents before children).
pub fn skin_matrices(model: &CompiledSkeleton3d, locals: &[JointTransform]) -> Vec<Mat4> {
    // A mismatched locals slice is a caller error: hand back nothing
    // rather than panicking. `try_skin_matrices` reports it explicitly.
    skin_matrices_checked(model, locals).unwrap_or_default()
}

/// Fallible form of [`skin_matrices`].
pub fn try_skin_matrices(
    model: &CompiledSkeleton3d,
    locals: &[JointTransform],
) -> Result<Vec<Mat4>, ModelError> {
    skin_matrices_checked(model, locals)
}

/// Shared core of [`skin_matrices`] and [`try_skin_matrices`].
fn skin_matrices_checked(
    model: &CompiledSkeleton3d,
    locals: &[JointTransform],
) -> Result<Vec<Mat4>, ModelError> {
    if locals.len() != model.joints.len() || model.inverse_bind.len() != model.joints.len() {
        return Err(ModelError::InvalidInput {
            detail: format!(
                "expected {} local poses and inverse-bind matrices, got {} and {}",
                model.joints.len(),
                locals.len(),
                model.inverse_bind.len()
            ),
        });
    }

    // Parent lookups need the accumulated *world* transform, while the
    // returned vector holds the skinning matrix; keep both.
    let mut worlds: Vec<Mat4> = Vec::with_capacity(locals.len());
    let mut out: Vec<Mat4> = Vec::with_capacity(locals.len());
    for (index, (joint, local)) in model.joints.iter().zip(locals.iter()).enumerate() {
        if !local.translation.is_finite() || !local.rotation.is_finite() || !local.scale.is_finite()
        {
            return Err(ModelError::InvalidInput {
                detail: format!("local pose {index} contains non-finite values"),
            });
        }
        let local_matrix =
            Mat4::from_scale_rotation_translation(local.scale, local.rotation, local.translation);
        if !local_matrix.is_finite() {
            return Err(ModelError::InvalidInput {
                detail: format!("local pose {index} overflows the representable range"),
            });
        }
        let world = match joint.parent {
            Some(parent) => {
                let parent = worlds.get(parent).copied().ok_or(ModelError::OutOfRange {
                    context: "joint parent".to_string(),
                    index: parent,
                    len: worlds.len(),
                })?;
                parent * local_matrix
            }
            None => local_matrix,
        };
        if !world.is_finite() {
            return Err(ModelError::InvalidInput {
                detail: format!("world pose {index} overflows the representable range"),
            });
        }
        let skin = world * model.inverse_bind[index];
        if !skin.is_finite() {
            return Err(ModelError::InvalidInput {
                detail: format!("skinning matrix {index} overflows the representable range"),
            });
        }
        worlds.push(world);
        out.push(skin);
    }
    Ok(out)
}

fn clamp(value: f32, min: f32, max: f32) -> f32 {
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

    #[test]
    fn clamp_never_wraps_the_endpoint_back_to_the_first_frame() {
        // `Loop` maps the duration onto the first frame; `Clamp` holds it.
        assert_eq!(clip_local_time(3.0, 3.0, ClipPlayback::Loop), 0.0);
        assert_eq!(clip_local_time(3.0, 3.0, ClipPlayback::Clamp), 3.0);
        assert_eq!(clip_local_time(4.5, 3.0, ClipPlayback::Clamp), 3.0);
    }

    #[test]
    fn loop_reduces_the_phase_in_f64_before_narrowing() {
        let time = 262_144.125_f64;
        assert_eq!(
            clip_local_time(time, 3.0, ClipPlayback::Loop),
            (time % 3.0) as f32
        );
        assert_eq!(clip_local_time(time, 3.0, ClipPlayback::Clamp), 3.0);
    }
}
