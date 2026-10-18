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

use std::{cmp::Ordering, collections::HashMap, sync::Arc};

use glam::{Mat4, Quat, Vec3};

use crate::model::error::ModelError;
use crate::model::image;
use crate::model::limits::LoadLimits;
use crate::model::schema as raw;
use crate::model::schema_v02 as raw2;
use crate::model::types::{
    ActionBindings, AlphaMode, AnimationInterpolation, CompiledModel, CompiledSkeleton3d,
    CompiledSprite2d, Deformer2d, InteractionBinding, Joint, Layer2d, Mask2d, Material3d,
    ModelData, ModelImage, ParamChannel, Parameter, ParameterAnimation, RotationKey, ScalarKey,
    ScaleKey, SkeletalAnimation, SkeletalChannel, TranslationKey, Vertex3d,
};

/// Safety margin of the conservative whole-model cutout visibility test.
///
/// An image whose effective alpha stays this far below `alpha_cutoff` cannot
/// contribute any fragment; the margin keeps borderline models visible
/// instead of risking a prematurely hidden mesh.
const CUTOUT_VISIBILITY_MARGIN: f32 = 1e-4;

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
                actions: None,
                images: Vec::new(),
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
                actions: None,
                images: Vec::new(),
            })
        }
        other => Err(ModelError::invalid(
            "kind",
            format!("unknown model kind `{other}`"),
        )),
    }
}

/// Compiles a validated `0.2` document.
///
/// Images are decoded first, with a running total checked against
/// [`LoadLimits::max_total_image_bytes`] before each output allocation, so a
/// bad image fails before the caller inserts anything into shared state.
/// Name references were resolved during validation; re-resolution here is
/// defensive.
pub(super) fn compile_v02(
    env: &raw2::RawEnvelope,
    limits: &LoadLimits,
) -> Result<CompiledModel, ModelError> {
    let mut total_image_bytes = 0usize;
    let mut images = Vec::with_capacity(env.images.len());
    let mut image_max_alpha = Vec::with_capacity(env.images.len());
    for (i, raw_image) in env.images.iter().enumerate() {
        let decoded = image::decode_model_image(
            &raw_image.id,
            &raw_image.png_base64,
            limits,
            total_image_bytes,
            &format!("images[{i}]"),
        )?;
        total_image_bytes = total_image_bytes
            .checked_add(decoded.image.rgba.len())
            .ok_or(ModelError::SizeOverflow {
                what: "total image bytes",
            })?;
        image_max_alpha.push(decoded.max_alpha);
        images.push(Arc::new(decoded.image));
    }

    match env.kind.as_str() {
        "sprite2d" => {
            let payload = env
                .sprite2d
                .as_ref()
                .ok_or(ModelError::MissingPayload { kind: "sprite2d" })?;
            let (sprite, gaze_parameter) =
                compile_sprite2d_v02(payload, &env.interaction, &images, limits)?;
            let actions = match &env.actions {
                Some(actions) => Some(resolve_action_bindings(
                    actions,
                    &sprite
                        .animations
                        .iter()
                        .map(|animation| animation.name.as_str())
                        .collect::<Vec<_>>(),
                )?),
                None => None,
            };
            Ok(CompiledModel {
                name: env.name.clone(),
                data: ModelData::Sprite2d(sprite),
                interaction: InteractionBinding {
                    gaze_parameter,
                    head_joint: None,
                },
                actions,
                images,
            })
        }
        "skeleton3d" => {
            let payload = env
                .skeleton3d
                .as_ref()
                .ok_or(ModelError::MissingPayload { kind: "skeleton3d" })?;
            let (skeleton, head_joint) =
                compile_skeleton3d_v02(payload, &env.interaction, &images, &image_max_alpha)?;
            let actions = match &env.actions {
                Some(actions) => Some(resolve_action_bindings(
                    actions,
                    &skeleton
                        .animations
                        .iter()
                        .map(|animation| animation.name.as_str())
                        .collect::<Vec<_>>(),
                )?),
                None => None,
            };
            Ok(CompiledModel {
                name: env.name.clone(),
                data: ModelData::Skeleton3d(skeleton),
                interaction: InteractionBinding {
                    gaze_parameter: None,
                    head_joint,
                },
                actions,
                images,
            })
        }
        other => Err(ModelError::invalid(
            "kind",
            format!("unknown model kind `{other}`"),
        )),
    }
}

/// Position of an image id in the decoded table. Validation already resolved
/// every reference; this is the compiler's defensive re-resolution.
fn image_index(images: &[Arc<ModelImage>], name: &str, ctx: &str) -> Result<usize, ModelError> {
    images
        .iter()
        .position(|image| image.id == name)
        .ok_or_else(|| ModelError::UnknownReference {
            kind: "image",
            name: name.to_string(),
            context: ctx.to_string(),
        })
}

/// `0.2` sprite compile: the `0.1` path plus texture binding, UVs (explicit
/// or auto-generated for grids) and the whole-layer visibility flag.
fn compile_sprite2d_v02(
    s: &raw2::RawSprite2d,
    interaction: &raw::RawInteraction,
    images: &[Arc<ModelImage>],
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

    let (deformers, deformer_index) = compile_deformers(&s.deformers, &parameters)?;

    let mut layers = Vec::with_capacity(s.layers.len());
    for (i, layer) in s.layers.iter().enumerate() {
        let ctx = format!("layers[{i}]");

        let (vertices, indices, generated_grid) =
            match (&layer.vertices, &layer.indices, &layer.grid) {
                (Some(v), Some(ind), None) => (v.clone(), ind.clone(), None),
                (None, None, Some(grid)) => {
                    let built = build_grid(grid, &ctx, limits)?;
                    (
                        built.vertices,
                        built.indices,
                        Some((built.columns, built.rows)),
                    )
                }
                _ => {
                    return Err(ModelError::invalid(
                        &ctx,
                        "layer mesh must be either explicit vertices+indices or a grid",
                    ));
                }
            };

        let texture = match &layer.texture {
            Some(name) => Some(image_index(images, name, &format!("{ctx}.texture"))?),
            None => None,
        };
        let uvs = match (&layer.uvs, generated_grid) {
            // Explicit UVs are validated against the vertex count; they are
            // kept even for untextured layers so a future bind is lossless.
            (Some(uvs), _) => uvs.clone(),
            (None, Some((columns, rows))) => {
                if texture.is_some() {
                    grid_uvs(columns, rows)
                } else {
                    Vec::new()
                }
            }
            (None, None) => Vec::new(),
        };
        let texture_visible = match texture {
            Some(index) => !images[index].fully_transparent,
            None => true,
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
            uvs,
            texture,
            texture_visible,
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

/// `0.2` skeleton compile: the core (joints/vertices/animations) plus the
/// texture binding, material resolution and conservative visibility.
fn compile_skeleton3d_v02(
    s: &raw2::RawSkeleton3d,
    interaction: &raw::RawInteraction,
    images: &[Arc<ModelImage>],
    image_max_alpha: &[f32],
) -> Result<(CompiledSkeleton3d, Option<usize>), ModelError> {
    let core = compile_skeleton_core(
        &s.vertices,
        &s.indices,
        &s.joints,
        &s.animations,
        s.uvs.as_deref(),
        interaction,
    )?;

    let texture = match &s.texture {
        Some(name) => Some(image_index(images, name, "skeleton3d.texture")?),
        None => None,
    };

    // A texture without an explicit material enters the PBR path with the
    // default (opaque) material; without either, the legacy vertex-color
    // path is kept.
    let material = match (&s.material, texture) {
        (Some(m), _) => Some(Material3d {
            roughness: m.roughness.unwrap_or(0.6),
            metallic: m.metallic.unwrap_or(0.0),
            alpha_mode: match m.alpha_mode.as_deref() {
                Some("cutout") => AlphaMode::Cutout,
                _ => AlphaMode::Opaque,
            },
            alpha_cutoff: m.alpha_cutoff.unwrap_or(0.5),
        }),
        (None, Some(_)) => Some(Material3d::default()),
        (None, None) => None,
    };

    let mut visible = true;
    if let Some(material) = &material {
        match material.alpha_mode {
            AlphaMode::Opaque => {
                // Validation already enforced vertex alpha 1.0; the bound
                // image must be exactly opaque as well.
                if let Some(index) = texture
                    && !images[index].opaque
                {
                    return Err(ModelError::invalid(
                        "skeleton3d.texture",
                        "opaque models require a fully opaque image (alpha 255 on every texel)",
                    ));
                }
            }
            AlphaMode::Cutout => {
                // Conservative whole-model test with no per-pixel geometry:
                // the best possible fragment coverage is the image's global
                // maximum alpha times the maximum vertex alpha (or just the
                // vertex alpha for an untextured mesh). If even that stays
                // clearly below the cutoff, no fragment can pass and the
                // model can be skipped wholesale. The margin keeps models
                // near the cutoff visible rather than risking a wrong skip.
                let vertex_max = s.vertices.iter().map(|v| v.color[3]).fold(0.0f32, f32::max);
                let effective_max = match texture {
                    Some(index) => image_max_alpha[index] * vertex_max,
                    None => vertex_max,
                };
                if effective_max <= 0.0
                    || effective_max + CUTOUT_VISIBILITY_MARGIN < material.alpha_cutoff
                {
                    visible = false;
                }
            }
        }
    }

    Ok((
        CompiledSkeleton3d {
            vertices: core.vertices,
            indices: core.indices,
            joints: core.joints,
            inverse_bind: core.inverse_bind,
            animations: core.animations,
            texture,
            material,
            visible,
        },
        core.head_joint,
    ))
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
            (None, None, Some(grid)) => {
                let grid = build_grid(grid, &ctx, limits)?;
                (grid.vertices, grid.indices)
            }
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
            uvs: Vec::new(),
            texture: None,
            texture_visible: true,
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

/// A generated grid mesh plus the dimensions needed for automatic UVs.
struct BuiltGrid {
    /// `(columns + 1) * (rows + 1)` vertices, row-major from the bottom row.
    vertices: Vec<[f32; 2]>,
    /// `columns * rows * 2` triangle indices.
    indices: Vec<u32>,
    /// Quad count along X as declared (`vertices` span `columns + 1`).
    columns: usize,
    /// Quad count along Y as declared (`vertices` span `rows + 1`).
    rows: usize,
}

/// Generates a grid mesh: `(columns + 1) * (rows + 1)` vertices and
/// `columns * rows * 2` triangles, both computed with checked arithmetic.
fn build_grid(
    grid: &raw::RawGrid2d,
    ctx: &str,
    limits: &LoadLimits,
) -> Result<BuiltGrid, ModelError> {
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

    Ok(BuiltGrid {
        vertices,
        indices,
        columns: grid.columns,
        rows: grid.rows,
    })
}

/// UVs of a generated grid, origin top-left: `u = col / columns`,
/// `v = 1 - row / rows` with `row` counted from the bottom row of the
/// generated mesh (so the model's top edge maps to `v = 0`).
fn grid_uvs(columns: usize, rows: usize) -> Vec<[f32; 2]> {
    let mut uvs = Vec::with_capacity((columns + 1) * (rows + 1));
    for row in 0..=rows {
        let v = 1.0 - row as f32 / rows as f32;
        for col in 0..=columns {
            uvs.push([col as f32 / columns as f32, v]);
        }
    }
    uvs
}

// ---------------------------------------------------------------------------
// 3D
// ---------------------------------------------------------------------------

/// The version-independent result of compiling a 3D skeleton.
struct CompiledSkeletonCore {
    vertices: Vec<Vertex3d>,
    indices: Vec<u32>,
    joints: Vec<Joint>,
    inverse_bind: Vec<Mat4>,
    animations: Vec<SkeletalAnimation>,
    head_joint: Option<usize>,
}

/// Compiles joints, vertices (with optional UVs), indices and animations.
///
/// Shared by both schema versions: the nested raw types are identical, so
/// only the top-level envelope differs.
fn compile_skeleton_core(
    raw_vertices: &[raw::RawVertex3d],
    raw_indices: &[u32],
    raw_joints: &[raw::RawJoint],
    raw_animations: &[raw::RawSkeletalAnimation],
    uvs: Option<&[[f32; 2]]>,
    interaction: &raw::RawInteraction,
) -> Result<CompiledSkeletonCore, ModelError> {
    let n = raw_joints.len();

    let mut declared_parent = Vec::with_capacity(n);
    for (i, joint) in raw_joints.iter().enumerate() {
        declared_parent.push(match &joint.parent {
            Some(name) => Some(joint_position(raw_joints, name).ok_or_else(|| {
                ModelError::UnknownReference {
                    kind: "joint",
                    name: name.clone(),
                    context: format!("skeleton3d.joints[{i}].parent"),
                }
            })?),
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
        let joint = &raw_joints[declared];
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

    let mut vertices = Vec::with_capacity(raw_vertices.len());
    for (i, v) in raw_vertices.iter().enumerate() {
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
            // Validation guarantees one UV per vertex; the fallback keeps a
            // hypothetical inconsistency from panicking.
            uv: uvs
                .and_then(|list| list.get(i))
                .copied()
                .unwrap_or([0.0, 0.0]),
        });
    }

    let inverse_bind = compute_inverse_bind(&joints)?;
    let animations = compile_skeletal_animations(raw_animations, raw_joints, &new_index)?;

    let head_joint = match &interaction.head_joint {
        Some(name) => Some(
            new_index[joint_position(raw_joints, name).ok_or_else(|| {
                ModelError::UnknownReference {
                    kind: "joint",
                    name: name.clone(),
                    context: "interaction.head_joint".to_string(),
                }
            })?],
        ),
        None => None,
    };

    Ok(CompiledSkeletonCore {
        vertices,
        indices: raw_indices.to_vec(),
        joints,
        inverse_bind,
        animations,
        head_joint,
    })
}

/// Returns the compiled payload and the resolved head joint, if declared.
fn compile_skeleton3d(
    s: &raw::RawSkeleton3d,
    interaction: &raw::RawInteraction,
) -> Result<(CompiledSkeleton3d, Option<usize>), ModelError> {
    let core = compile_skeleton_core(
        &s.vertices,
        &s.indices,
        &s.joints,
        &s.animations,
        None,
        interaction,
    )?;
    Ok((
        CompiledSkeleton3d {
            vertices: core.vertices,
            indices: core.indices,
            joints: core.joints,
            inverse_bind: core.inverse_bind,
            animations: core.animations,
            texture: None,
            material: None,
            visible: true,
        },
        core.head_joint,
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

fn joint_position(joints: &[raw::RawJoint], name: &str) -> Option<usize> {
    joints.iter().position(|j| j.name == name)
}

fn parameter_index(parameters: &[Parameter], name: &str) -> Option<usize> {
    parameters.iter().position(|p| p.name == name)
}

/// Resolves the `0.2` `actions` table against one kind's animation names.
///
/// Validation already resolved every reference; this is the compiler's
/// defensive re-resolution, mirroring `image_index`. A missing or ambiguous
/// name is an error, never a guessed index; an absent optional slot stays
/// `None`.
fn resolve_action_bindings(
    actions: &raw2::RawActions,
    animation_names: &[&str],
) -> Result<ActionBindings, ModelError> {
    let resolve = |role: &str, name: &str| -> Result<usize, ModelError> {
        let ctx = format!("actions.{role}");
        let Some(index) = animation_names.iter().position(|n| *n == name) else {
            return Err(ModelError::UnknownReference {
                kind: "animation",
                name: name.to_string(),
                context: ctx,
            });
        };
        if animation_names[index + 1..].contains(&name) {
            return Err(ModelError::invalid(
                ctx,
                format!(
                    "animation reference `{name}` is ambiguous; the name is declared more than once"
                ),
            ));
        }
        Ok(index)
    };
    let resolve_optional =
        |role: &str, name: Option<&String>| -> Result<Option<usize>, ModelError> {
            match name {
                Some(name) => Ok(Some(resolve(role, name)?)),
                None => Ok(None),
            }
        };
    Ok(ActionBindings {
        idle: resolve("idle", &actions.idle)?,
        walk: resolve("walk", &actions.walk)?,
        reaction: resolve("reaction", &actions.reaction)?,
        greet: resolve_optional("greet", actions.greet.as_ref())?,
        play: resolve_optional("play", actions.play.as_ref())?,
    })
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
