//! Turns a validated [`RawEnvelope`] into the immutable [`CompiledModel`].
//!
//! Compilation is where names become indices and where the ordering
//! guarantees the runtime relies on are established:
//!
//! - **joints and deformers are reordered** so that `parent < index` holds for
//!   every entry; every reference (vertices, channels, layers, interaction,
//!   inverse-bind) is remapped through the permutation, so a single forward
//!   pass is always sufficient;
//! - **grid meshes are generated** with overflow-checked arithmetic;
//! - **skin weights are normalized** to sum to `1.0`, with zero-weight lanes
//!   pointing at a real joint;
//! - **inverse-bind matrices are computed and checked** for finiteness and
//!   invertibility, so a singular bind pose is a load error rather than a
//!   broken pose at runtime;
//! - **all quaternions are normalized** and every derived value is verified
//!   finite, so oversized numbers surface as errors instead of `NaN`s in the
//!   pose.
//!
//! The input file is never modified; normalization only affects the compiled
//! copy.

use std::{cmp::Ordering, collections::HashMap};

use glam::{Mat4, Quat, Vec3};

use crate::model::error::ModelError;
use crate::model::limits::LoadLimits;
use crate::model::schema as raw;
use crate::model::types::{
    AnimationInterpolation, CompiledModel, CompiledSkeleton3d, CompiledSprite2d, Deformer2d,
    InteractionBinding, Joint, Layer2d, Mask2d, ModelData, ParamChannel, Parameter,
    ParameterAnimation, RotationKey, ScalarKey, ScaleKey, SkeletalAnimation, SkeletalChannel,
    TranslationKey, Vertex3d,
};

/// Compiles a validated document. Validation has run first; the checks that
/// remain are re-resolution of indices and guards on derived values.
pub(super) fn compile(
    env: &raw::RawEnvelope,
    limits: &LoadLimits,
) -> Result<CompiledModel, ModelError> {
    match env.kind.as_str() {
        "sprite2d" => {
            let payload = env
                .sprite2d
                .as_ref()
                .ok_or(ModelError::MissingPayload { kind: "sprite2d" })?;
            let (sprite, gaze_parameter) = compile_sprite2d(payload, &env.interaction, limits)?;
            Ok(CompiledModel {
                name: env.name.clone(),
                data: ModelData::Sprite2d(sprite),
                interaction: InteractionBinding {
                    gaze_parameter,
                    head_joint: None,
                },
            })
        }
        "skeleton3d" => {
            let payload = env
                .skeleton3d
                .as_ref()
                .ok_or(ModelError::MissingPayload { kind: "skeleton3d" })?;
            let (skeleton, head_joint) = compile_skeleton3d(payload, &env.interaction)?;
            Ok(CompiledModel {
                name: env.name.clone(),
                data: ModelData::Skeleton3d(skeleton),
                interaction: InteractionBinding {
                    gaze_parameter: None,
                    head_joint,
                },
            })
        }
        other => Err(ModelError::invalid(
            "kind",
            format!("unknown model kind `{other}`"),
        )),
    }
}

// ---------------------------------------------------------------------------
// 2D
// ---------------------------------------------------------------------------

/// Returns the compiled payload and the resolved gaze parameter, if declared.
fn compile_sprite2d(
    s: &raw::RawSprite2d,
    interaction: &raw::RawInteraction,
    limits: &LoadLimits,
) -> Result<(CompiledSprite2d, Option<usize>), ModelError> {
    let parameters: Vec<Parameter> = s
        .parameters
        .iter()
        .map(|p| Parameter {
            name: p.name.clone(),
            min: p.min,
            max: p.max,
            default: p.default,
        })
        .collect();

    // Deformers are reordered so parents precede children; layer references
    // follow the permutation through `deformer_index`.
    let (deformers, deformer_index) = compile_deformers(&s.deformers, &parameters)?;

    let mut layers = Vec::with_capacity(s.layers.len());
    for (i, layer) in s.layers.iter().enumerate() {
        let ctx = format!("layers[{i}]");

        let (vertices, indices) = match (&layer.vertices, &layer.indices, &layer.grid) {
            (Some(v), Some(ind), None) => (v.clone(), ind.clone()),
            (None, None, Some(grid)) => build_grid(grid, &ctx, limits)?,
            _ => {
                return Err(ModelError::invalid(
                    &ctx,
                    "layer mesh must be either explicit vertices+indices or a grid",
                ));
            }
        };

        let deformer = match &layer.deformer {
            Some(name) => Some(*deformer_index.get(name.as_str()).ok_or_else(|| {
                ModelError::UnknownReference {
                    kind: "deformer",
                    name: name.clone(),
                    context: format!("{ctx}.deformer"),
                }
            })?),
            None => None,
        };

        let mask = layer.mask.as_ref().map(|m| Mask2d {
            points: m.points.clone(),
        });

        layers.push(Layer2d {
            name: layer.name.clone(),
            vertices,
            indices,
            color: layer.color,
            deformer,
            mask,
        });
    }

    let animations = compile_parameter_animations(&s.animations, &parameters)?;

    let gaze_parameter = match &interaction.gaze_parameter {
        Some(name) => Some(parameter_index(&parameters, name).ok_or_else(|| {
            ModelError::UnknownReference {
                kind: "parameter",
                name: name.clone(),
                context: "interaction.gaze_parameter".to_string(),
            }
        })?),
        None => None,
    };

    Ok((
        CompiledSprite2d {
            parameters,
            layers,
            deformers,
            animations,
        },
        gaze_parameter,
    ))
}

fn compile_deformers(
    deformers: &[raw::RawDeformer2d],
    parameters: &[Parameter],
) -> Result<(Vec<Deformer2d>, HashMap<String, usize>), ModelError> {
    let n = deformers.len();

    let mut declared_parent = Vec::with_capacity(n);
    let mut parameter_of = Vec::with_capacity(n);
    for (i, d) in deformers.iter().enumerate() {
        declared_parent.push(match &d.parent {
            Some(name) => Some(name_position(deformers, name).ok_or_else(|| {
                ModelError::UnknownReference {
                    kind: "deformer",
                    name: name.clone(),
                    context: format!("deformers[{i}].parent"),
                }
            })?),
            None => None,
        });
        parameter_of.push(parameter_index(parameters, &d.parameter).ok_or_else(|| {
            ModelError::UnknownReference {
                kind: "parameter",
                name: d.parameter.clone(),
                context: format!("deformers[{i}].parameter"),
            }
        })?);
    }

    let order = topological_order(&declared_parent, "deformers")?;
    let mut new_index = vec![0usize; n];
    for (position, &declared) in order.iter().enumerate() {
        new_index[declared] = position;
    }

    let mut out = Vec::with_capacity(n);
    for &declared in &order {
        let d = &deformers[declared];
        let ctx = format!("deformers[{declared}]");
        let parent = declared_parent[declared].map(|p| new_index[p]);
        let parameter = parameter_of[declared];
        let compiled = match d.kind.as_str() {
            "rotation" => Deformer2d::Rotation {
                parent,
                parameter,
                pivot: d.pivot.ok_or_else(|| {
                    ModelError::invalid(&ctx, "`rotation` deformers require `pivot`")
                })?,
                max_angle_radians: d.max_angle_radians.ok_or_else(|| {
                    ModelError::invalid(&ctx, "`rotation` deformers require `max_angle_radians`")
                })?,
            },
            "warp" => Deformer2d::Warp {
                parent,
                parameter,
                bounds: d.bounds.ok_or_else(|| {
                    ModelError::invalid(&ctx, "`warp` deformers require `bounds`")
                })?,
                grid: d
                    .grid
                    .ok_or_else(|| ModelError::invalid(&ctx, "`warp` deformers require `grid`"))?,
                offsets: d.offsets.clone().ok_or_else(|| {
                    ModelError::invalid(&ctx, "`warp` deformers require `offsets`")
                })?,
            },
            other => {
                return Err(ModelError::invalid(
                    format!("{ctx}.type"),
                    format!("unknown deformer type `{other}`"),
                ));
            }
        };
        out.push(compiled);
    }

    let index_map = order
        .iter()
        .enumerate()
        .map(|(position, &declared)| (deformers[declared].name.clone(), position))
        .collect();
    Ok((out, index_map))
}

fn compile_parameter_animations(
    animations: &[raw::RawParameterAnimation],
    parameters: &[Parameter],
) -> Result<Vec<ParameterAnimation>, ModelError> {
    let mut out = Vec::with_capacity(animations.len());
    for anim in animations {
        let mut channels = Vec::with_capacity(anim.channels.len());
        for channel in &anim.channels {
            let parameter = parameter_index(parameters, &channel.parameter).ok_or_else(|| {
                ModelError::UnknownReference {
                    kind: "parameter",
                    name: channel.parameter.clone(),
                    context: format!("animations[{}].channels", anim.name),
                }
            })?;
            let range = &parameters[parameter];
            let keys = channel
                .keys
                .iter()
                .map(|k| ScalarKey {
                    time: k.time,
                    // Validated against the declared range; clamping here
                    // makes the runtime guarantee exact.
                    value: clamp(k.value, range.min, range.max),
                })
                .collect();
            channels.push(ParamChannel { parameter, keys });
        }
        out.push(ParameterAnimation {
            name: anim.name.clone(),
            duration: anim.duration,
            channels,
        });
    }
    Ok(out)
}

/// Generates a grid mesh: `(columns + 1) * (rows + 1)` vertices and
/// `columns * rows * 2` triangles, both computed with checked arithmetic.
fn build_grid(
    grid: &raw::RawGrid2d,
    ctx: &str,
    limits: &LoadLimits,
) -> Result<(Vec<[f32; 2]>, Vec<u32>), ModelError> {
    let cols = grid
        .columns
        .checked_add(1)
        .ok_or(ModelError::SizeOverflow {
            what: "grid columns",
        })?;
    let rows = grid
        .rows
        .checked_add(1)
        .ok_or(ModelError::SizeOverflow { what: "grid rows" })?;
    let max_dim = limits.max_grid_dimension as usize;
    if cols < 2 || rows < 2 || cols > max_dim || rows > max_dim {
        return Err(ModelError::invalid(
            format!("{ctx}.grid"),
            "grid dimensions are out of the supported range",
        ));
    }

    let vertex_count = cols.checked_mul(rows).ok_or(ModelError::SizeOverflow {
        what: "grid vertices",
    })?;
    let quad_count = grid
        .columns
        .checked_mul(grid.rows)
        .ok_or(ModelError::SizeOverflow { what: "grid quads" })?;
    let index_count = quad_count.checked_mul(6).ok_or(ModelError::SizeOverflow {
        what: "grid indices",
    })?;

    let [x0, y0, width, height] = grid.bounds;
    let denom_x = (cols - 1) as f32;
    let denom_y = (rows - 1) as f32;

    let mut vertices = Vec::with_capacity(vertex_count);
    for row in 0..rows {
        let v = row as f32 / denom_y;
        let y = y0 + height * v;
        for col in 0..cols {
            let u = col as f32 / denom_x;
            vertices.push([x0 + width * u, y]);
        }
    }

    let mut indices = Vec::with_capacity(index_count);
    let cols_u32 = cols as u32;
    for row in 0..grid.rows {
        for col in 0..grid.columns {
            let top_left = (row * cols + col) as u32;
            let top_right = top_left + 1;
            let bottom_left = top_left + cols_u32;
            let bottom_right = bottom_left + 1;
            indices.extend_from_slice(&[top_left, bottom_left, bottom_right]);
            indices.extend_from_slice(&[top_left, bottom_right, top_right]);
        }
    }

    Ok((vertices, indices))
}

// ---------------------------------------------------------------------------
// 3D
// ---------------------------------------------------------------------------

/// Returns the compiled payload and the resolved head joint, if declared.
fn compile_skeleton3d(
    s: &raw::RawSkeleton3d,
    interaction: &raw::RawInteraction,
) -> Result<(CompiledSkeleton3d, Option<usize>), ModelError> {
    let n = s.joints.len();

    let mut declared_parent = Vec::with_capacity(n);
    for (i, joint) in s.joints.iter().enumerate() {
        declared_parent.push(match &joint.parent {
            Some(name) => {
                Some(
                    joint_position(s, name).ok_or_else(|| ModelError::UnknownReference {
                        kind: "joint",
                        name: name.clone(),
                        context: format!("skeleton3d.joints[{i}].parent"),
                    })?,
                )
            }
            None => None,
        });
    }

    let order = topological_order(&declared_parent, "skeleton3d.joints")?;
    let mut new_index = vec![0usize; n];
    for (position, &declared) in order.iter().enumerate() {
        new_index[declared] = position;
    }

    let mut joints = Vec::with_capacity(n);
    for &declared in &order {
        let joint = &s.joints[declared];
        joints.push(Joint {
            name: joint.name.clone(),
            parent: declared_parent[declared].map(|p| new_index[p]),
            translation: Vec3::from_array(joint.translation),
            rotation: normalize_quat(
                joint.rotation,
                &format!("skeleton3d.joints[{declared}].rotation"),
            )?,
            scale: Vec3::from_array(joint.scale),
        });
    }

    let mut vertices = Vec::with_capacity(s.vertices.len());
    for (i, v) in s.vertices.iter().enumerate() {
        let ctx = format!("skeleton3d.vertices[{i}]");
        let (vertex_joints, weights) = normalize_weights(v.joints, v.weights, n, &ctx)?;
        let remapped = [
            new_index[vertex_joints[0] as usize] as u32,
            new_index[vertex_joints[1] as usize] as u32,
            new_index[vertex_joints[2] as usize] as u32,
            new_index[vertex_joints[3] as usize] as u32,
        ];
        vertices.push(Vertex3d {
            position: v.position,
            normal: normalize_vec3(v.normal, &format!("{ctx}.normal"))?.to_array(),
            color: v.color,
            joints: remapped,
            weights,
        });
    }

    let inverse_bind = compute_inverse_bind(&joints)?;
    let animations = compile_skeletal_animations(&s.animations, &s.joints, &new_index)?;

    let head_joint = match &interaction.head_joint {
        Some(name) => Some(
            new_index[joint_position(s, name).ok_or_else(|| ModelError::UnknownReference {
                kind: "joint",
                name: name.clone(),
                context: "interaction.head_joint".to_string(),
            })?],
        ),
        None => None,
    };

    Ok((
        CompiledSkeleton3d {
            vertices,
            indices: s.indices.clone(),
            joints,
            inverse_bind,
            animations,
        },
        head_joint,
    ))
}

/// Normalizes the four weights of a vertex to a sum of exactly `1.0` and
/// rewrites zero-weight lanes to a real joint, so every emitted joint index is
/// valid even for unused lanes. The source file is untouched.
fn normalize_weights(
    joints: [u32; 4],
    weights: [f32; 4],
    joint_count: usize,
    ctx: &str,
) -> Result<([u32; 4], [f32; 4]), ModelError> {
    for (k, j) in joints.iter().enumerate() {
        if *j as usize >= joint_count {
            return Err(ModelError::invalid(
                format!("{ctx}.joints[{k}]"),
                format!("joint index {j} is out of range ({joint_count} joints)"),
            ));
        }
    }

    let mut sum = 0.0f64;
    for w in weights {
        if !w.is_finite() || w < 0.0 {
            return Err(ModelError::invalid(
                format!("{ctx}.weights"),
                "skin weights must be finite and non-negative",
            ));
        }
        sum += w as f64;
    }
    if sum.partial_cmp(&0.0) != Some(Ordering::Greater) {
        return Err(ModelError::invalid(
            format!("{ctx}.weights"),
            "skin weights must sum to a positive value",
        ));
    }

    let mut normalized = [0.0f32; 4];
    let mut primary = 0usize;
    for k in 0..4 {
        normalized[k] = (weights[k] as f64 / sum) as f32;
        if normalized[k] > normalized[primary] {
            primary = k;
        }
    }
    // A positive sum plus the chosen dominant lane cannot be zero; validation
    // guarantees at least one positive weight, and normalization preserves it.
    if normalized[primary] <= 0.0 {
        return Err(ModelError::invalid(
            format!("{ctx}.weights"),
            "skin weights cannot be normalized",
        ));
    }

    // Absorb the f32 rounding residual into the dominant lane so the runtime
    // can rely on a unit sum, then zero out the unused lanes and point them at
    // a real joint.
    let total: f32 = normalized.iter().sum();
    normalized[primary] += 1.0 - total;

    let mut out_joints = joints;
    for k in 0..4 {
        if normalized[k] <= 0.0 {
            normalized[k] = 0.0;
            out_joints[k] = joints[primary];
        }
    }

    let total: f32 = normalized.iter().sum();
    normalized[primary] += 1.0 - total;

    if !normalized.iter().all(|w| w.is_finite()) {
        return Err(ModelError::invalid(
            format!("{ctx}.weights"),
            "normalized skin weights are not finite",
        ));
    }
    Ok((out_joints, normalized))
}

/// World bind transform and its inverse for every joint, in topological
/// order. A singular or overflowing bind pose is a load error, never a
/// runtime `NaN`.
fn compute_inverse_bind(joints: &[Joint]) -> Result<Vec<Mat4>, ModelError> {
    // Worlds and inverses are kept apart: the parent lookup needs the *world*
    // bind transform, while the caller stores only the inverses.
    let mut worlds: Vec<Mat4> = Vec::with_capacity(joints.len());
    let mut out: Vec<Mat4> = Vec::with_capacity(joints.len());
    for (i, joint) in joints.iter().enumerate() {
        let local =
            Mat4::from_scale_rotation_translation(joint.scale, joint.rotation, joint.translation);
        if !local.is_finite() {
            return Err(ModelError::invalid(
                format!("skeleton3d.joints[{i}]"),
                "bind pose overflows the representable range",
            ));
        }
        let world = match joint.parent {
            Some(parent) => {
                let parent = worlds.get(parent).copied().ok_or_else(|| {
                    ModelError::invalid(
                        format!("skeleton3d.joints[{i}].parent"),
                        "parent index does not precede the child",
                    )
                })?;
                parent * local
            }
            None => local,
        };
        if !world.is_finite() {
            return Err(ModelError::invalid(
                format!("skeleton3d.joints[{i}]"),
                "bind pose is too large; the accumulated transform overflows",
            ));
        }
        // The determinant of a scale/rotation/translation product is the
        // product of the scales, so a vanishing determinant means a singular
        // (uninvertible) bind pose.
        let determinant = world.determinant();
        if !determinant.is_finite() || determinant == 0.0 {
            return Err(ModelError::SingularBindPose {
                joint: joint.name.clone(),
            });
        }
        let inverse = world.inverse();
        if !inverse.is_finite() {
            return Err(ModelError::SingularBindPose {
                joint: joint.name.clone(),
            });
        }
        worlds.push(world);
        out.push(inverse);
    }
    Ok(out)
}

fn compile_skeletal_animations(
    animations: &[raw::RawSkeletalAnimation],
    raw_joints: &[raw::RawJoint],
    new_index: &[usize],
) -> Result<Vec<SkeletalAnimation>, ModelError> {
    let mut out = Vec::with_capacity(animations.len());
    for anim in animations {
        let mut channels = Vec::with_capacity(anim.channels.len());
        for channel in &anim.channels {
            let declared = raw_joints
                .iter()
                .position(|j| j.name == channel.joint)
                .ok_or_else(|| ModelError::UnknownReference {
                    kind: "joint",
                    name: channel.joint.clone(),
                    context: format!("animations[{}].channels", anim.name),
                })?;
            let joint = new_index[declared];

            let interpolation = match channel.interpolation.as_deref() {
                None | Some("linear") => AnimationInterpolation::Linear,
                Some("step") => AnimationInterpolation::Step,
                Some(other) => {
                    return Err(ModelError::invalid(
                        format!("animations[{}].channels.interpolation", anim.name),
                        format!("unknown interpolation `{other}`; expected `linear` or `step`"),
                    ));
                }
            };

            let translation = channel
                .translation
                .iter()
                .map(|k| TranslationKey {
                    time: k.time,
                    value: Vec3::from_array(k.value),
                })
                .collect();

            let mut rotation = Vec::with_capacity(channel.rotation.len());
            for key in &channel.rotation {
                rotation.push(RotationKey {
                    time: key.time,
                    value: normalize_quat(
                        key.value,
                        &format!("animations[{}].channels.rotation", anim.name),
                    )?,
                });
            }

            // Scale keys stay as authored: their signs were validated to be
            // stable and non-zero, and the bind scale shares that sign.
            let scale = channel
                .scale
                .iter()
                .map(|k| ScaleKey {
                    time: k.time,
                    value: Vec3::from_array(k.value),
                })
                .collect();

            channels.push(SkeletalChannel {
                joint,
                interpolation,
                translation,
                rotation,
                scale,
            });
        }
        out.push(SkeletalAnimation {
            name: anim.name.clone(),
            duration: anim.duration,
            channels,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Reorders a parent forest so that a parent always precedes its children and
/// returns the permutation `order[position] = declared_index`. A cycle (or an
/// out-of-range parent) is a rejected document, not a panic.
fn topological_order(parents: &[Option<usize>], context: &str) -> Result<Vec<usize>, ModelError> {
    let n = parents.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, parent) in parents.iter().enumerate() {
        match parent {
            Some(p) if *p < n => children[*p].push(i),
            Some(_) => {
                return Err(ModelError::invalid(context, "parent index is out of range"));
            }
            None => {}
        }
    }

    let mut order = Vec::with_capacity(n);
    let mut stack: Vec<usize> = (0..n).rev().filter(|i| parents[*i].is_none()).collect();
    while let Some(node) = stack.pop() {
        order.push(node);
        for &child in children[node].iter().rev() {
            stack.push(child);
        }
    }

    if order.len() != n {
        return Err(ModelError::Cycle {
            context: context.to_string(),
        });
    }

    // Confirm the invariant the runtime depends on before handing it out.
    let mut position = vec![0usize; n];
    for (i, &node) in order.iter().enumerate() {
        position[node] = i;
    }
    for (i, &node) in order.iter().enumerate() {
        if let Some(parent) = parents[node]
            && position[parent] >= i
        {
            return Err(ModelError::Cycle {
                context: context.to_string(),
            });
        }
    }
    Ok(order)
}

fn name_position(deformers: &[raw::RawDeformer2d], name: &str) -> Option<usize> {
    deformers.iter().position(|d| d.name == name)
}

fn joint_position(s: &raw::RawSkeleton3d, name: &str) -> Option<usize> {
    s.joints.iter().position(|j| j.name == name)
}

fn parameter_index(parameters: &[Parameter], name: &str) -> Option<usize> {
    parameters.iter().position(|p| p.name == name)
}

fn normalize_quat(value: [f32; 4], ctx: &str) -> Result<Quat, ModelError> {
    let q = Quat::from_xyzw(value[0], value[1], value[2], value[3]);
    if !q.is_finite() {
        return Err(ModelError::invalid(ctx, "quaternion must be finite"));
    }
    let length = q.length();
    if !length.is_finite() || length < 1e-4 {
        return Err(ModelError::invalid(ctx, "quaternion is too close to zero"));
    }
    Ok(q * (1.0 / length))
}

fn normalize_vec3(value: [f32; 3], ctx: &str) -> Result<Vec3, ModelError> {
    let v = Vec3::from_array(value);
    if !v.is_finite() {
        return Err(ModelError::invalid(ctx, "vector must be finite"));
    }
    let length = v.length();
    if !length.is_finite() || length < 1e-6 {
        return Err(ModelError::invalid(ctx, "vector is too close to zero"));
    }
    Ok(v * (1.0 / length))
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
