//! Structural and semantic validation of a parsed [`RawEnvelope`].
//!
//! Runs before any GPU resource exists and before any derived work happens.
//! Design rules:
//!
//! - every float is checked finite before it is used in comparisons;
//! - every derived count (grid meshes, warp control grids, key totals) is
//!   computed with checked arithmetic, so a small file cannot request
//!   unbounded memory or overflow a `usize`;
//! - every name reference is resolved, because the compiler turns names into
//!   indices and an unresolved name must fail *now*, not at draw time;
//! - error contexts name the exact JSON path (`layers[2].grid.columns`).
//!
//! The checks here are intentionally redundant with [`compile`](super::compile):
//! validation produces the good error messages, compilation re-resolves
//! indices and can still fail safely on its own.

use std::{cmp::Ordering, collections::HashSet};

use crate::model::error::ModelError;
use crate::model::limits::LoadLimits;
use crate::model::schema as raw;
use crate::model::schema_v02 as raw2;

/// Maximum length of any name in the document (model, parameter, layer,
/// deformer, animation, joint); mirrors [`crate::model::MAX_NAME_LEN`].
pub(super) const MAX_NAME_LEN: usize = 128;

/// Bound on 2D coordinates. Far outside any plausible sprite, but small
/// enough that all interpolation stays finite.
const MAX_ABS_2D: f32 = 1e7;

/// Bound on 3D coordinates, translations and scales. Keeps hierarchy
/// products finite; derived matrices are additionally checked for finiteness
/// during compilation.
const MAX_ABS_3D: f32 = 1e6;

/// Smallest non-zero magnitude accepted for a scale component or for an
/// animated scale key, so runtime scales never collapse to a singularity.
const MIN_ABS_SCALE: f32 = 1e-6;

/// Bound on parameter ranges. Wide ranges would make deformer output
/// degenerate at runtime through loss of precision; the runtime guards the
/// result, this rejects the input early.
const MAX_ABS_PARAMETER: f32 = 1e9;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Validates a parsed document against the format rules and the budgets.
pub(super) fn validate(env: &raw::RawEnvelope, limits: &LoadLimits) -> Result<(), ModelError> {
    if env.format != raw::FORMAT {
        return Err(ModelError::UnsupportedFormat {
            found: env.format.clone(),
        });
    }
    if env.format_version != raw::FORMAT_VERSION {
        return Err(ModelError::UnsupportedVersion {
            found: env.format_version.clone(),
        });
    }
    check_name(&env.name, "name")?;

    match env.kind.as_str() {
        "sprite2d" => {
            let payload = env
                .sprite2d
                .as_ref()
                .ok_or(ModelError::MissingPayload { kind: "sprite2d" })?;
            if env.skeleton3d.is_some() {
                return Err(ModelError::UnexpectedPayload {
                    kind: "sprite2d",
                    other: "skeleton3d",
                });
            }
            validate_sprite2d(payload, &env.interaction, limits)
        }
        "skeleton3d" => {
            let payload = env
                .skeleton3d
                .as_ref()
                .ok_or(ModelError::MissingPayload { kind: "skeleton3d" })?;
            if env.sprite2d.is_some() {
                return Err(ModelError::UnexpectedPayload {
                    kind: "skeleton3d",
                    other: "sprite2d",
                });
            }
            validate_skeleton3d(payload, &env.interaction, limits)
        }
        other => Err(ModelError::invalid(
            "kind",
            format!("unknown model kind `{other}`; expected `sprite2d` or `skeleton3d`"),
        )),
    }
}

// ---------------------------------------------------------------------------
// 0.2 entry point
// ---------------------------------------------------------------------------

/// Validates a parsed `0.2` document against the format rules and the budgets.
///
/// Shares every check that did not change with the `0.1` validators; the
/// additions are the image table, texture references, UVs and the basic
/// material, plus the effective-alpha rule for the opaque mode.
pub(super) fn validate_v02(env: &raw2::RawEnvelope, limits: &LoadLimits) -> Result<(), ModelError> {
    if env.format != raw::FORMAT {
        return Err(ModelError::UnsupportedFormat {
            found: env.format.clone(),
        });
    }
    if env.format_version != raw2::FORMAT_VERSION {
        return Err(ModelError::UnsupportedVersion {
            found: env.format_version.clone(),
        });
    }
    check_name(&env.name, "name")?;

    let image_names = validate_images(&env.images, limits)?;

    match env.kind.as_str() {
        "sprite2d" => {
            let payload = env
                .sprite2d
                .as_ref()
                .ok_or(ModelError::MissingPayload { kind: "sprite2d" })?;
            if env.skeleton3d.is_some() {
                return Err(ModelError::UnexpectedPayload {
                    kind: "sprite2d",
                    other: "skeleton3d",
                });
            }
            validate_sprite2d_v02(payload, &env.interaction, &image_names, limits)
        }
        "skeleton3d" => {
            let payload = env
                .skeleton3d
                .as_ref()
                .ok_or(ModelError::MissingPayload { kind: "skeleton3d" })?;
            if env.sprite2d.is_some() {
                return Err(ModelError::UnexpectedPayload {
                    kind: "skeleton3d",
                    other: "sprite2d",
                });
            }
            validate_skeleton3d_v02(payload, &env.interaction, &image_names, limits)
        }
        other => Err(ModelError::invalid(
            "kind",
            format!("unknown model kind `{other}`; expected `sprite2d` or `skeleton3d`"),
        )),
    }
}

/// Image table: count budget, unique non-empty ids. Returns the id list used
/// to resolve texture references.
fn validate_images<'a>(
    images: &'a [raw2::RawImage],
    limits: &LoadLimits,
) -> Result<Vec<&'a str>, ModelError> {
    if images.len() > limits.max_images {
        return Err(ModelError::LimitExceeded {
            what: "images",
            actual: images.len() as u64,
            max: limits.max_images as u64,
        });
    }
    let mut names = Vec::with_capacity(images.len());
    let mut seen = HashSet::new();
    for (i, image) in images.iter().enumerate() {
        check_name(&image.id, &format!("images[{i}].id"))?;
        if !seen.insert(image.id.clone()) {
            return Err(ModelError::DuplicateName {
                kind: "image",
                name: image.id.clone(),
            });
        }
        names.push(image.id.as_str());
    }
    Ok(names)
}

/// Every declared image must be referenced; an unused embedded payload is a
/// document error, not a silent waste.
fn reject_unused_images(names: &[&str], used: &[bool]) -> Result<(), ModelError> {
    for (i, name) in names.iter().enumerate() {
        if !used[i] {
            return Err(ModelError::invalid(
                format!("images[{i}]"),
                format!("image `{name}` is never referenced by a layer or mesh"),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 2D
// ---------------------------------------------------------------------------

fn validate_sprite2d(
    s: &raw::RawSprite2d,
    interaction: &raw::RawInteraction,
    limits: &LoadLimits,
) -> Result<(), ModelError> {
    let parameter_names = validate_parameters(&s.parameters, limits)?;
    validate_layers(&s.layers, &s.deformers, limits)?;
    validate_deformers(&s.deformers, &parameter_names, limits)?;
    validate_parameter_animations(&s.animations, &s.parameters, limits)?;

    // Interaction: names must resolve, wrong-kind capabilities are errors.
    if let Some(name) = &interaction.gaze_parameter {
        resolve_name(
            &parameter_names,
            name,
            "parameter",
            "interaction.gaze_parameter",
        )?;
    }
    if let Some(name) = &interaction.head_joint {
        return Err(ModelError::invalid(
            "interaction.head_joint",
            format!(
                "`{name}` is only valid for kind `skeleton3d`; 2D models declare `gaze_parameter`"
            ),
        ));
    }
    Ok(())
}

fn validate_parameters(
    parameters: &[raw::RawParameter],
    limits: &LoadLimits,
) -> Result<Vec<String>, ModelError> {
    if parameters.len() > limits.max_parameters {
        return Err(ModelError::LimitExceeded {
            what: "parameters",
            actual: parameters.len() as u64,
            max: limits.max_parameters as u64,
        });
    }

    let mut names = Vec::with_capacity(parameters.len());
    let mut seen = HashSet::new();
    for (i, p) in parameters.iter().enumerate() {
        let ctx = format!("parameters[{i}]");
        check_name(&p.name, &format!("{ctx}.name"))?;
        if !seen.insert(p.name.clone()) {
            return Err(ModelError::DuplicateName {
                kind: "parameter",
                name: p.name.clone(),
            });
        }
        let min = finite(p.min, &format!("{ctx}.min"))?;
        let max = finite(p.max, &format!("{ctx}.max"))?;
        let default = finite(p.default, &format!("{ctx}.default"))?;
        if min.abs() > MAX_ABS_PARAMETER || max.abs() > MAX_ABS_PARAMETER {
            return Err(ModelError::invalid(
                &ctx,
                format!("parameter bounds must stay within +/-{MAX_ABS_PARAMETER}"),
            ));
        }
        if min.partial_cmp(&max) != Some(Ordering::Less) {
            return Err(ModelError::invalid(
                &ctx,
                format!("parameter range must be increasing, found min {min} >= max {max}"),
            ));
        }
        // A width of `0` cannot survive `min < max`; this catches a range so
        // small that `default` can never be distinguished, and a difference
        // that overflows to infinity.
        if max.partial_cmp(&min) != Some(Ordering::Greater) || !(max - min).is_finite() {
            return Err(ModelError::invalid(
                &ctx,
                "parameter range is too narrow or too wide to be represented",
            ));
        }
        if default < min || default > max {
            return Err(ModelError::invalid(
                &ctx,
                format!("default {default} is outside the declared range [{min}, {max}]"),
            ));
        }
        names.push(p.name.clone());
    }
    Ok(names)
}

fn validate_layers(
    layers: &[raw::RawLayer2d],
    deformers: &[raw::RawDeformer2d],
    limits: &LoadLimits,
) -> Result<(), ModelError> {
    if layers.is_empty() {
        return Err(ModelError::invalid(
            "layers",
            "a model needs at least one layer",
        ));
    }
    if layers.len() > limits.max_layers {
        return Err(ModelError::LimitExceeded {
            what: "layers",
            actual: layers.len() as u64,
            max: limits.max_layers as u64,
        });
    }

    // Deformer names are resolved here as well as in `validate_deformers`, so
    // a layer cannot reference an undeclared deformer regardless of order.
    let deformer_names: Vec<&str> = deformers.iter().map(|d| d.name.as_str()).collect();

    let mut total_vertices: usize = 0;
    let mut total_indices: usize = 0;
    let mut seen = HashSet::new();

    for (i, layer) in layers.iter().enumerate() {
        let ctx = format!("layers[{i}]");
        check_name(&layer.name, &format!("{ctx}.name"))?;
        if !seen.insert(layer.name.clone()) {
            return Err(ModelError::DuplicateName {
                kind: "layer",
                name: layer.name.clone(),
            });
        }
        check_color(layer.color, &format!("{ctx}.color"))?;

        let (vertex_count, index_count) = check_layer_mesh(
            layer.vertices.as_deref(),
            layer.indices.as_deref(),
            layer.grid.as_ref(),
            &ctx,
            limits,
        )?;
        total_vertices = add_budget(total_vertices, vertex_count, "vertices")?;
        total_indices = add_budget(total_indices, index_count, "indices")?;

        if let Some(name) = &layer.deformer {
            resolve_names(
                &deformer_names,
                name,
                "deformer",
                &format!("{ctx}.deformer"),
            )?;
        }
        if let Some(mask) = &layer.mask {
            check_mask(&mask.points, &format!("{ctx}.mask"), limits)?;
        }
    }

    if total_vertices > limits.max_vertices {
        return Err(ModelError::LimitExceeded {
            what: "vertices",
            actual: total_vertices as u64,
            max: limits.max_vertices as u64,
        });
    }
    if total_indices > limits.max_indices {
        return Err(ModelError::LimitExceeded {
            what: "indices",
            actual: total_indices as u64,
            max: limits.max_indices as u64,
        });
    }
    Ok(())
}

/// `0.2` sprite: the `0.1` checks plus per-layer texture bindings and UVs.
fn validate_sprite2d_v02(
    s: &raw2::RawSprite2d,
    interaction: &raw::RawInteraction,
    image_names: &[&str],
    limits: &LoadLimits,
) -> Result<(), ModelError> {
    let parameter_names = validate_parameters(&s.parameters, limits)?;
    let used_images = validate_layers_v02(&s.layers, &s.deformers, image_names, limits)?;
    reject_unused_images(image_names, &used_images)?;
    validate_deformers(&s.deformers, &parameter_names, limits)?;
    validate_parameter_animations(&s.animations, &s.parameters, limits)?;

    if let Some(name) = &interaction.gaze_parameter {
        resolve_name(
            &parameter_names,
            name,
            "parameter",
            "interaction.gaze_parameter",
        )?;
    }
    if let Some(name) = &interaction.head_joint {
        return Err(ModelError::invalid(
            "interaction.head_joint",
            format!(
                "`{name}` is only valid for kind `skeleton3d`; 2D models declare `gaze_parameter`"
            ),
        ));
    }
    Ok(())
}

/// `0.2` layers. Returns one `used` flag per declared image (referenced by
/// any layer) so the caller can reject unused resources.
fn validate_layers_v02(
    layers: &[raw2::RawLayer2d],
    deformers: &[raw::RawDeformer2d],
    image_names: &[&str],
    limits: &LoadLimits,
) -> Result<Vec<bool>, ModelError> {
    if layers.is_empty() {
        return Err(ModelError::invalid(
            "layers",
            "a model needs at least one layer",
        ));
    }
    if layers.len() > limits.max_layers {
        return Err(ModelError::LimitExceeded {
            what: "layers",
            actual: layers.len() as u64,
            max: limits.max_layers as u64,
        });
    }

    let deformer_names: Vec<&str> = deformers.iter().map(|d| d.name.as_str()).collect();
    let mut used_images = vec![false; image_names.len()];

    let mut total_vertices: usize = 0;
    let mut total_indices: usize = 0;
    let mut seen = HashSet::new();

    for (i, layer) in layers.iter().enumerate() {
        let ctx = format!("layers[{i}]");
        check_name(&layer.name, &format!("{ctx}.name"))?;
        if !seen.insert(layer.name.clone()) {
            return Err(ModelError::DuplicateName {
                kind: "layer",
                name: layer.name.clone(),
            });
        }
        check_color(layer.color, &format!("{ctx}.color"))?;

        let (vertex_count, index_count) = check_layer_mesh(
            layer.vertices.as_deref(),
            layer.indices.as_deref(),
            layer.grid.as_ref(),
            &ctx,
            limits,
        )?;
        total_vertices = add_budget(total_vertices, vertex_count, "vertices")?;
        total_indices = add_budget(total_indices, index_count, "indices")?;

        // Texture reference and UVs. The grid vertex count is the generated
        // `(columns + 1) * (rows + 1)`; explicit meshes use their vertices.
        let texture = match &layer.texture {
            Some(name) => {
                let index = resolve_names(image_names, name, "image", &format!("{ctx}.texture"))?;
                used_images[index] = true;
                Some(index)
            }
            None => None,
        };
        match (&layer.uvs, &layer.grid) {
            (Some(uvs), Some(grid)) => {
                let expected = generated_grid_vertices(grid, &format!("{ctx}.grid"), limits)?;
                check_uvs(uvs, expected, &format!("{ctx}.uvs"))?;
            }
            (Some(uvs), None) => {
                let count = layer.vertices.as_ref().map_or(0, Vec::len);
                check_uvs(uvs, count, &format!("{ctx}.uvs"))?;
            }
            // A grid without explicit UVs auto-generates them when textured;
            // untextured layers ignore UVs entirely.
            (None, Some(_)) => {}
            (None, None) => {
                if texture.is_some() {
                    return Err(ModelError::invalid(
                        &ctx,
                        "a textured explicit mesh requires `uvs`",
                    ));
                }
            }
        }

        if let Some(name) = &layer.deformer {
            resolve_names(
                &deformer_names,
                name,
                "deformer",
                &format!("{ctx}.deformer"),
            )?;
        }
        if let Some(mask) = &layer.mask {
            check_mask(&mask.points, &format!("{ctx}.mask"), limits)?;
        }
    }

    if total_vertices > limits.max_vertices {
        return Err(ModelError::LimitExceeded {
            what: "vertices",
            actual: total_vertices as u64,
            max: limits.max_vertices as u64,
        });
    }
    if total_indices > limits.max_indices {
        return Err(ModelError::LimitExceeded {
            what: "indices",
            actual: total_indices as u64,
            max: limits.max_indices as u64,
        });
    }
    Ok(used_images)
}

/// Vertex count a generated grid produces, `(columns + 1) * (rows + 1)`,
/// with the same bounds as [`grid_counts`] so an invalid grid is reported
/// once, by the mesh check.
fn generated_grid_vertices(
    grid: &raw::RawGrid2d,
    ctx: &str,
    limits: &LoadLimits,
) -> Result<usize, ModelError> {
    let (vertices, _) = grid_counts(grid, ctx, limits)?;
    Ok(vertices)
}

/// UV arrays: one entry per vertex, finite, inside `[0, 1]`.
fn check_uvs(uvs: &[[f32; 2]], expected: usize, ctx: &str) -> Result<(), ModelError> {
    if uvs.len() != expected {
        return Err(ModelError::invalid(
            ctx,
            format!(
                "expected {expected} UVs (one per vertex), found {}",
                uvs.len()
            ),
        ));
    }
    for (i, uv) in uvs.iter().enumerate() {
        for (k, value) in uv.iter().enumerate() {
            let value = finite(*value, &format!("{ctx}[{i}][{k}]"))?;
            if !(0.0..=1.0).contains(&value) {
                return Err(ModelError::invalid(
                    format!("{ctx}[{i}][{k}]"),
                    format!("UV components must be in [0, 1], found {value}"),
                ));
            }
        }
    }
    Ok(())
}

/// Basic material: every present field finite and in range; an explicit
/// `alpha_cutoff` is only valid with cutout.
fn validate_material(m: &raw2::RawMaterial3d, ctx: &str) -> Result<(), ModelError> {
    if let Some(roughness) = m.roughness {
        let roughness = finite(roughness, &format!("{ctx}.roughness"))?;
        if !(0.0..=1.0).contains(&roughness) {
            return Err(ModelError::invalid(
                format!("{ctx}.roughness"),
                format!("roughness must be in [0, 1], found {roughness}"),
            ));
        }
    }
    if let Some(metallic) = m.metallic {
        let metallic = finite(metallic, &format!("{ctx}.metallic"))?;
        if !(0.0..=1.0).contains(&metallic) {
            return Err(ModelError::invalid(
                format!("{ctx}.metallic"),
                format!("metallic must be in [0, 1], found {metallic}"),
            ));
        }
    }

    match m.alpha_mode.as_deref() {
        None | Some("opaque") => {
            if m.alpha_cutoff.is_some() {
                return Err(ModelError::invalid(
                    format!("{ctx}.alpha_cutoff"),
                    "`alpha_cutoff` is only valid with `alpha_mode: \"cutout\"`",
                ));
            }
        }
        Some("cutout") => {
            if let Some(cutoff) = m.alpha_cutoff {
                let cutoff = finite(cutoff, &format!("{ctx}.alpha_cutoff"))?;
                if cutoff <= 0.0 || cutoff > 1.0 {
                    return Err(ModelError::invalid(
                        format!("{ctx}.alpha_cutoff"),
                        format!("alpha_cutoff must be in (0, 1], found {cutoff}"),
                    ));
                }
            }
        }
        Some(other) => {
            return Err(ModelError::invalid(
                format!("{ctx}.alpha_mode"),
                format!("unknown alpha mode `{other}`; expected `opaque` or `cutout`"),
            ));
        }
    }
    Ok(())
}

/// Checks one layer mesh (explicit or generated) and returns the
/// `(vertices, indices)` counts it contributes to the model budgets.
///
/// Shared by the `0.1` and `0.2` layer validators so both versions enforce
/// exactly the same geometry rules and messages.
fn check_layer_mesh(
    vertices: Option<&[[f32; 2]]>,
    indices: Option<&[u32]>,
    grid: Option<&raw::RawGrid2d>,
    ctx: &str,
    limits: &LoadLimits,
) -> Result<(usize, usize), ModelError> {
    match (vertices, indices, grid) {
        (Some(vertices), Some(indices), None) => {
            if vertices.is_empty() {
                return Err(ModelError::invalid(
                    format!("{ctx}.vertices"),
                    "explicit meshes need at least one vertex",
                ));
            }
            for (k, v) in vertices.iter().enumerate() {
                check_2d(*v, &format!("{ctx}.vertices[{k}]"))?;
            }
            check_triangle_list_2d(indices, vertices, &format!("{ctx}.indices"))?;
            Ok((vertices.len(), indices.len()))
        }
        (None, None, Some(grid)) => grid_counts(grid, &format!("{ctx}.grid"), limits),
        (Some(_), None, None) => Err(ModelError::invalid(ctx, "`vertices` requires `indices`")),
        (None, Some(_), None) => Err(ModelError::invalid(ctx, "`indices` requires `vertices`")),
        (None, None, None) => Err(ModelError::invalid(
            ctx,
            "layer needs either `vertices` + `indices` or `grid`",
        )),
        (_, _, Some(_)) => Err(ModelError::invalid(
            ctx,
            "`grid` cannot be combined with an explicit mesh",
        )),
    }
}

/// Returns `(vertices, indices)` a grid will generate, with every step of
/// the arithmetic checked before it is used as a bound.
fn grid_counts(
    grid: &raw::RawGrid2d,
    ctx: &str,
    limits: &LoadLimits,
) -> Result<(usize, usize), ModelError> {
    check_bounds(grid.bounds, &format!("{ctx}.bounds"))?;
    if grid.columns == 0 || grid.rows == 0 {
        return Err(ModelError::invalid(
            ctx,
            "`columns` and `rows` must be at least 1",
        ));
    }
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
    if cols > max_dim || rows > max_dim {
        return Err(ModelError::LimitExceeded {
            what: "grid dimension",
            actual: cols.max(rows) as u64,
            max: limits.max_grid_dimension as u64,
        });
    }

    let vertices = mul_budget(cols, rows, "grid vertices")?;
    let quads = mul_budget(grid.columns, grid.rows, "grid quads")?;
    let indices = mul_budget(quads, 6, "grid indices")?;
    Ok((vertices, indices))
}

/// Polygon mask: finite, bounded, at least three points, no repeated
/// consecutive point, non-degenerate area and simple (non self-intersecting).
fn check_mask(points: &[[f32; 2]], ctx: &str, limits: &LoadLimits) -> Result<(), ModelError> {
    if points.len() < 3 {
        return Err(ModelError::invalid(
            format!("{ctx}.points"),
            format!(
                "a mask polygon needs at least 3 points, found {}",
                points.len()
            ),
        ));
    }
    if points.len() > limits.max_mask_points {
        return Err(ModelError::LimitExceeded {
            what: "mask points",
            actual: points.len() as u64,
            max: limits.max_mask_points as u64,
        });
    }
    for (i, p) in points.iter().enumerate() {
        check_2d(*p, &format!("{ctx}.points[{i}]"))?;
    }

    let n = points.len();
    for i in 0..n {
        let a = points[i];
        let b = points[(i + 1) % n];
        if distance2(a, b) <= 1e-12 {
            return Err(ModelError::invalid(
                ctx,
                format!("points {i} and {} coincide", (i + 1) % n),
            ));
        }
    }

    // Degenerate (near-zero area) polygons cannot be triangulated by the
    // renderer reliably; the threshold is relative to the polygon's own
    // extent (both are areas), so small but valid masks are not rejected.
    let (area, diag2) = polygon_area_and_extent(points);
    let span2 = diag2.max(1e-12);
    if (area / span2).partial_cmp(&1e-9) != Some(Ordering::Greater) {
        return Err(ModelError::invalid(
            ctx,
            "mask polygon is degenerate (zero or near-zero area)",
        ));
    }

    if !is_simple_polygon(points) {
        return Err(ModelError::invalid(
            ctx,
            "mask polygon must be simple (no self-intersections or touching edges)",
        ));
    }
    Ok(())
}

fn validate_deformers(
    deformers: &[raw::RawDeformer2d],
    parameters: &[String],
    limits: &LoadLimits,
) -> Result<(), ModelError> {
    if deformers.len() > limits.max_deformers {
        return Err(ModelError::LimitExceeded {
            what: "deformers",
            actual: deformers.len() as u64,
            max: limits.max_deformers as u64,
        });
    }

    let mut seen = HashSet::new();
    let mut total_warp_offsets: usize = 0;

    for (i, d) in deformers.iter().enumerate() {
        let ctx = format!("deformers[{i}]");
        check_name(&d.name, &format!("{ctx}.name"))?;
        if !seen.insert(d.name.clone()) {
            return Err(ModelError::DuplicateName {
                kind: "deformer",
                name: d.name.clone(),
            });
        }

        resolve_name(
            parameters,
            &d.parameter,
            "parameter",
            &format!("{ctx}.parameter"),
        )?;

        match d.kind.as_str() {
            "rotation" => {
                require_absent(d.bounds.as_ref(), &format!("{ctx}.bounds"), "rotation")?;
                require_absent(d.grid.as_ref(), &format!("{ctx}.grid"), "rotation")?;
                require_absent(d.offsets.as_ref(), &format!("{ctx}.offsets"), "rotation")?;
                let pivot = d.pivot.as_ref().ok_or_else(|| {
                    ModelError::invalid(&ctx, "`rotation` deformers require `pivot`")
                })?;
                check_2d(*pivot, &format!("{ctx}.pivot"))?;
                let angle = d.max_angle_radians.ok_or_else(|| {
                    ModelError::invalid(&ctx, "`rotation` deformers require `max_angle_radians`")
                })?;
                finite(angle, &format!("{ctx}.max_angle_radians"))?;
            }
            "warp" => {
                require_absent(d.pivot.as_ref(), &format!("{ctx}.pivot"), "warp")?;
                require_absent(
                    d.max_angle_radians.as_ref(),
                    &format!("{ctx}.max_angle_radians"),
                    "warp",
                )?;
                let bounds = d.bounds.as_ref().ok_or_else(|| {
                    ModelError::invalid(&ctx, "`warp` deformers require `bounds`")
                })?;
                check_bounds(*bounds, &format!("{ctx}.bounds"))?;
                let grid = d
                    .grid
                    .as_ref()
                    .ok_or_else(|| ModelError::invalid(&ctx, "`warp` deformers require `grid`"))?;
                let offsets = d.offsets.as_ref().ok_or_else(|| {
                    ModelError::invalid(&ctx, "`warp` deformers require `offsets`")
                })?;

                let max_dim = limits.max_grid_dimension as usize;
                if grid[0] < 2 || grid[1] < 2 {
                    return Err(ModelError::invalid(
                        format!("{ctx}.grid"),
                        format!(
                            "warp control grids need both dimensions >= 2, found [{}, {}]",
                            grid[0], grid[1]
                        ),
                    ));
                }
                if grid[0] > max_dim || grid[1] > max_dim {
                    return Err(ModelError::LimitExceeded {
                        what: "warp grid dimension",
                        actual: grid[0].max(grid[1]) as u64,
                        max: limits.max_grid_dimension as u64,
                    });
                }
                let expected = mul_budget(grid[0], grid[1], "warp control offsets")?;
                if offsets.len() != expected {
                    return Err(ModelError::invalid(
                        format!("{ctx}.offsets"),
                        format!(
                            "expected {} offsets for a {}x{} control grid, found {}",
                            expected,
                            grid[0],
                            grid[1],
                            offsets.len()
                        ),
                    ));
                }
                for (k, o) in offsets.iter().enumerate() {
                    check_2d(*o, &format!("{ctx}.offsets[{k}]"))?;
                }
                total_warp_offsets =
                    add_budget(total_warp_offsets, offsets.len(), "warp control offsets")?;
            }
            other => {
                return Err(ModelError::invalid(
                    format!("{ctx}.type"),
                    format!("unknown deformer type `{other}`; expected `rotation` or `warp`"),
                ));
            }
        }
    }

    if total_warp_offsets > limits.max_total_warp_offsets {
        return Err(ModelError::LimitExceeded {
            what: "warp control offsets",
            actual: total_warp_offsets as u64,
            max: limits.max_total_warp_offsets as u64,
        });
    }

    // Parent links: resolve, reject self-parenting, then reject cycles.
    let name_list: Vec<String> = deformers.iter().map(|d| d.name.clone()).collect();
    let mut parents: Vec<Option<usize>> = Vec::with_capacity(deformers.len());
    for (i, d) in deformers.iter().enumerate() {
        match &d.parent {
            None => parents.push(None),
            Some(parent) => {
                let index = resolve_name(
                    &name_list,
                    parent,
                    "deformer",
                    &format!("deformers[{i}].parent"),
                )?;
                if index == i {
                    return Err(ModelError::invalid(
                        format!("deformers[{i}].parent"),
                        "a deformer cannot be its own parent",
                    ));
                }
                parents.push(Some(index));
            }
        }
    }
    ensure_acyclic(&parents, "deformers")?;
    Ok(())
}

fn validate_parameter_animations(
    animations: &[raw::RawParameterAnimation],
    parameters: &[raw::RawParameter],
    limits: &LoadLimits,
) -> Result<(), ModelError> {
    if animations.len() > limits.max_animations {
        return Err(ModelError::LimitExceeded {
            what: "animations",
            actual: animations.len() as u64,
            max: limits.max_animations as u64,
        });
    }

    let parameter_names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
    let mut total_keys: usize = 0;
    let mut seen = HashSet::new();

    for (i, anim) in animations.iter().enumerate() {
        let ctx = format!("animations[{i}]");
        check_name(&anim.name, &format!("{ctx}.name"))?;
        if !seen.insert(anim.name.clone()) {
            return Err(ModelError::DuplicateName {
                kind: "animation",
                name: anim.name.clone(),
            });
        }
        let duration = check_duration(anim.duration, &format!("{ctx}.duration"))?;

        if anim.channels.is_empty() {
            return Err(ModelError::invalid(
                &ctx,
                "an animation needs at least one channel",
            ));
        }
        if anim.channels.len() > limits.max_channels_per_animation {
            return Err(ModelError::LimitExceeded {
                what: "channels",
                actual: anim.channels.len() as u64,
                max: limits.max_channels_per_animation as u64,
            });
        }

        let mut used_parameters = HashSet::new();
        for (c, channel) in anim.channels.iter().enumerate() {
            let cctx = format!("{ctx}.channels[{c}]");
            let parameter = resolve_names(
                &parameter_names,
                &channel.parameter,
                "parameter",
                &format!("{cctx}.parameter"),
            )?;
            if !used_parameters.insert(parameter) {
                return Err(ModelError::invalid(
                    &cctx,
                    format!("duplicate channel for parameter `{}`", channel.parameter),
                ));
            }

            if channel.keys.is_empty() {
                return Err(ModelError::invalid(
                    format!("{cctx}.keys"),
                    "a channel needs at least one key",
                ));
            }
            if channel.keys.len() > limits.max_keys_per_channel {
                return Err(ModelError::LimitExceeded {
                    what: "keys per channel",
                    actual: channel.keys.len() as u64,
                    max: limits.max_keys_per_channel as u64,
                });
            }

            let (min, max) = (parameters[parameter].min, parameters[parameter].max);
            let tolerance = parameter_tolerance(min, max);
            let mut previous_time = f32::NEG_INFINITY;
            for (k, key) in channel.keys.iter().enumerate() {
                let kctx = format!("{cctx}.keys[{k}]");
                let time = finite(key.time, &format!("{kctx}.time"))?;
                let value = finite(key.value, &format!("{kctx}.value"))?;
                check_key_time(time, previous_time, duration, &kctx)?;
                previous_time = time;
                if value < min - tolerance || value > max + tolerance {
                    return Err(ModelError::invalid(
                        format!("{kctx}.value"),
                        format!(
                            "value {value} is outside the range [{min}, {max}] of parameter `{}`",
                            channel.parameter
                        ),
                    ));
                }
            }
            total_keys = add_budget(total_keys, channel.keys.len(), "animation keys")?;
        }
    }

    if total_keys > limits.max_total_keys {
        return Err(ModelError::LimitExceeded {
            what: "animation keys",
            actual: total_keys as u64,
            max: limits.max_total_keys as u64,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 3D
// ---------------------------------------------------------------------------

fn validate_skeleton3d(
    s: &raw::RawSkeleton3d,
    interaction: &raw::RawInteraction,
    limits: &LoadLimits,
) -> Result<(), ModelError> {
    let joint_names = validate_joints(&s.joints, limits)?;
    validate_vertices(&s.vertices, s.joints.len(), limits)?;
    validate_mesh3d(&s.indices, &s.vertices, limits)?;

    validate_skeletal_animations(&s.animations, &joint_names, &s.joints, limits)?;

    if let Some(name) = &interaction.head_joint {
        resolve_name(&joint_names, name, "joint", "interaction.head_joint")?;
    }
    if let Some(name) = &interaction.gaze_parameter {
        return Err(ModelError::invalid(
            "interaction.gaze_parameter",
            format!("`{name}` is only valid for kind `sprite2d`; 3D models declare `head_joint`"),
        ));
    }
    Ok(())
}

/// `0.2` skeleton: the `0.1` checks plus texture/UVs/material, the unused
/// image rejection and the effective-alpha rule of the opaque mode.
fn validate_skeleton3d_v02(
    s: &raw2::RawSkeleton3d,
    interaction: &raw::RawInteraction,
    image_names: &[&str],
    limits: &LoadLimits,
) -> Result<(), ModelError> {
    let joint_names = validate_joints(&s.joints, limits)?;
    validate_vertices(&s.vertices, s.joints.len(), limits)?;
    validate_mesh3d(&s.indices, &s.vertices, limits)?;
    validate_skeletal_animations(&s.animations, &joint_names, &s.joints, limits)?;

    let mut used_images = vec![false; image_names.len()];
    let texture = match &s.texture {
        Some(name) => {
            let index = resolve_names(image_names, name, "image", "skeleton3d.texture")?;
            used_images[index] = true;
            Some(index)
        }
        None => None,
    };
    reject_unused_images(image_names, &used_images)?;

    match &s.uvs {
        Some(uvs) => check_uvs(uvs, s.vertices.len(), "skeleton3d.uvs")?,
        None => {
            if texture.is_some() {
                return Err(ModelError::invalid(
                    "skeleton3d",
                    "a textured mesh requires `uvs` (one per vertex)",
                ));
            }
        }
    }

    if let Some(material) = &s.material {
        validate_material(material, "skeleton3d.material")?;
    }

    // Effective alpha rule: as soon as the model takes the PBR path (texture
    // or material declared) with the opaque mode, every vertex color must be
    // exactly opaque; the bound image is checked against its decoded alpha in
    // the compiler. Cutout leaves the comparison to the GPU.
    let takes_pbr_path = texture.is_some() || s.material.is_some();
    // An unknown mode never reaches this point: `validate_material` above
    // rejects it, so `cutout` is the only non-opaque case left.
    let mode_is_opaque = !matches!(
        s.material.as_ref().and_then(|m| m.alpha_mode.as_deref()),
        Some("cutout")
    );
    if takes_pbr_path && mode_is_opaque {
        for (i, v) in s.vertices.iter().enumerate() {
            if v.color[3] != 1.0 {
                return Err(ModelError::invalid(
                    format!("skeleton3d.vertices[{i}].color[3]"),
                    format!(
                        "opaque models require vertex alpha exactly 1, found {}",
                        v.color[3]
                    ),
                ));
            }
        }
    }

    if let Some(name) = &interaction.head_joint {
        resolve_name(&joint_names, name, "joint", "interaction.head_joint")?;
    }
    if let Some(name) = &interaction.gaze_parameter {
        return Err(ModelError::invalid(
            "interaction.gaze_parameter",
            format!("`{name}` is only valid for kind `sprite2d`; 3D models declare `head_joint`"),
        ));
    }
    Ok(())
}

/// Joint names, per-joint fields, parenting and cycles. Shared by both
/// versions; returns the declared joint names.
fn validate_joints(
    joints: &[raw::RawJoint],
    limits: &LoadLimits,
) -> Result<Vec<String>, ModelError> {
    if joints.is_empty() {
        return Err(ModelError::invalid(
            "skeleton3d.joints",
            "a skeleton needs at least one joint",
        ));
    }
    if joints.len() > limits.max_joints {
        return Err(ModelError::LimitExceeded {
            what: "joints",
            actual: joints.len() as u64,
            max: limits.max_joints as u64,
        });
    }

    // Joint names + per-joint fields.
    let mut joint_names = Vec::with_capacity(joints.len());
    let mut seen = HashSet::new();
    for (i, joint) in joints.iter().enumerate() {
        let ctx = format!("skeleton3d.joints[{i}]");
        check_name(&joint.name, &format!("{ctx}.name"))?;
        if !seen.insert(joint.name.clone()) {
            return Err(ModelError::DuplicateName {
                kind: "joint",
                name: joint.name.clone(),
            });
        }
        check_bounded_vec3(joint.translation, &format!("{ctx}.translation"))?;
        check_quat(joint.rotation, &format!("{ctx}.rotation"))?;
        check_joint_scale(joint.scale, &format!("{ctx}.scale"))?;
        joint_names.push(joint.name.clone());
    }

    // Joint parenting: resolve, self-parenting, cycles.
    let mut parents: Vec<Option<usize>> = Vec::with_capacity(joints.len());
    for (i, joint) in joints.iter().enumerate() {
        match &joint.parent {
            None => parents.push(None),
            Some(parent) => {
                let index = resolve_name(
                    &joint_names,
                    parent,
                    "joint",
                    &format!("skeleton3d.joints[{i}].parent"),
                )?;
                if index == i {
                    return Err(ModelError::invalid(
                        format!("skeleton3d.joints[{i}].parent"),
                        "a joint cannot be its own parent",
                    ));
                }
                parents.push(Some(index));
            }
        }
    }
    ensure_acyclic(&parents, "skeleton3d.joints")?;
    Ok(joint_names)
}

/// Vertex fields, joint indices and weight sums. Shared by both versions.
fn validate_vertices(
    vertices: &[raw::RawVertex3d],
    joint_count: usize,
    limits: &LoadLimits,
) -> Result<(), ModelError> {
    if vertices.is_empty() {
        return Err(ModelError::invalid(
            "skeleton3d.vertices",
            "a skeleton needs at least one vertex",
        ));
    }
    if vertices.len() > limits.max_vertices {
        return Err(ModelError::LimitExceeded {
            what: "vertices",
            actual: vertices.len() as u64,
            max: limits.max_vertices as u64,
        });
    }
    for (i, v) in vertices.iter().enumerate() {
        let ctx = format!("skeleton3d.vertices[{i}]");
        check_bounded_vec3(v.position, &format!("{ctx}.position"))?;
        check_normal(v.normal, &format!("{ctx}.normal"))?;
        check_color(v.color, &format!("{ctx}.color"))?;
        for (k, joint) in v.joints.iter().enumerate() {
            let joint = *joint as usize;
            if joint >= joint_count {
                return Err(ModelError::invalid(
                    format!("{ctx}.joints[{k}]"),
                    format!("joint index {joint} is out of range ({joint_count} joints)"),
                ));
            }
        }
        let mut sum = 0.0f32;
        for (k, weight) in v.weights.iter().enumerate() {
            let weight = finite(*weight, &format!("{ctx}.weights[{k}]"))?;
            if weight < 0.0 {
                return Err(ModelError::invalid(
                    format!("{ctx}.weights[{k}]"),
                    format!("skin weights must not be negative, found {weight}"),
                ));
            }
            sum += weight;
            if !sum.is_finite() {
                return Err(ModelError::invalid(
                    format!("{ctx}.weights"),
                    "skin weight sum overflows",
                ));
            }
        }
        if sum <= 0.0 {
            return Err(ModelError::invalid(
                format!("{ctx}.weights"),
                "skin weights must sum to a positive value",
            ));
        }
        {
            // A vertex whose weights touch no real joint is unusable: the
            // normalization in the compiler would divide by a zero sum for
            // the zero-weight slots.
            let used = v.weights.iter().zip(v.joints.iter()).any(|(w, _)| *w > 0.0);
            if !used {
                return Err(ModelError::invalid(
                    format!("{ctx}.weights"),
                    "at least one skin weight must be positive",
                ));
            }
        }
    }
    Ok(())
}

/// Index budget plus triangle-list degeneracy checks. Shared by both
/// versions.
fn validate_mesh3d(
    indices: &[u32],
    vertices: &[raw::RawVertex3d],
    limits: &LoadLimits,
) -> Result<(), ModelError> {
    if indices.len() > limits.max_indices {
        return Err(ModelError::LimitExceeded {
            what: "indices",
            actual: indices.len() as u64,
            max: limits.max_indices as u64,
        });
    }
    check_triangle_list_3d(indices, vertices, "skeleton3d.indices")
}

fn validate_skeletal_animations(
    animations: &[raw::RawSkeletalAnimation],
    joint_names: &[String],
    joints: &[raw::RawJoint],
    limits: &LoadLimits,
) -> Result<(), ModelError> {
    if animations.len() > limits.max_animations {
        return Err(ModelError::LimitExceeded {
            what: "animations",
            actual: animations.len() as u64,
            max: limits.max_animations as u64,
        });
    }

    let mut total_keys: usize = 0;
    let mut seen = HashSet::new();

    for (i, anim) in animations.iter().enumerate() {
        let ctx = format!("skeleton3d.animations[{i}]");
        check_name(&anim.name, &format!("{ctx}.name"))?;
        if !seen.insert(anim.name.clone()) {
            return Err(ModelError::DuplicateName {
                kind: "animation",
                name: anim.name.clone(),
            });
        }
        let duration = check_duration(anim.duration, &format!("{ctx}.duration"))?;

        if anim.channels.is_empty() {
            return Err(ModelError::invalid(
                &ctx,
                "an animation needs at least one channel",
            ));
        }
        if anim.channels.len() > limits.max_channels_per_animation {
            return Err(ModelError::LimitExceeded {
                what: "channels",
                actual: anim.channels.len() as u64,
                max: limits.max_channels_per_animation as u64,
            });
        }

        let mut used_joints = HashSet::new();
        for (c, channel) in anim.channels.iter().enumerate() {
            let cctx = format!("{ctx}.channels[{c}]");
            let joint = resolve_name(
                joint_names,
                &channel.joint,
                "joint",
                &format!("{cctx}.joint"),
            )?;
            if !used_joints.insert(joint) {
                return Err(ModelError::invalid(
                    &cctx,
                    format!("duplicate channel for joint `{}`", channel.joint),
                ));
            }

            match channel.interpolation.as_deref() {
                None | Some("linear") | Some("step") => {}
                Some(other) => {
                    return Err(ModelError::invalid(
                        format!("{cctx}.interpolation"),
                        format!("unknown interpolation `{other}`; expected `linear` or `step`"),
                    ));
                }
            }

            if channel.translation.is_empty()
                && channel.rotation.is_empty()
                && channel.scale.is_empty()
            {
                return Err(ModelError::invalid(
                    &cctx,
                    "a channel needs at least one non-empty track (translation, rotation or scale)",
                ));
            }

            // Translation track.
            let mut previous = f32::NEG_INFINITY;
            for (k, key) in channel.translation.iter().enumerate() {
                let kctx = format!("{cctx}.translation[{k}]");
                let time = finite(key.time, &format!("{kctx}.time"))?;
                check_key_time(time, previous, duration, &kctx)?;
                previous = time;
                check_bounded_vec3(key.value, &format!("{kctx}.value"))?;
            }
            total_keys = add_budget(total_keys, channel.translation.len(), "animation keys")?;

            // Rotation track.
            let mut previous = f32::NEG_INFINITY;
            for (k, key) in channel.rotation.iter().enumerate() {
                let kctx = format!("{cctx}.rotation[{k}]");
                let time = finite(key.time, &format!("{kctx}.time"))?;
                check_key_time(time, previous, duration, &kctx)?;
                previous = time;
                check_quat(key.value, &format!("{kctx}.value"))?;
            }
            total_keys = add_budget(total_keys, channel.rotation.len(), "animation keys")?;

            // Scale track: every component must keep one sign across all keys
            // (including the loop back to the first key), otherwise runtime
            // scale would cross zero and produce a singular pose.
            let mut previous = f32::NEG_INFINITY;
            for (k, key) in channel.scale.iter().enumerate() {
                let kctx = format!("{cctx}.scale[{k}]");
                let time = finite(key.time, &format!("{kctx}.time"))?;
                check_key_time(time, previous, duration, &kctx)?;
                previous = time;
                check_scale_value(key.value, &format!("{kctx}.value"))?;
            }
            check_sign_consistency(
                &channel.scale,
                &format!("{cctx}.scale"),
                &joints[joint].scale,
                &format!("skeleton3d.joints[{joint}].scale"),
            )?;
            total_keys = add_budget(total_keys, channel.scale.len(), "animation keys")?;

            if channel.translation.len() > limits.max_keys_per_channel
                || channel.rotation.len() > limits.max_keys_per_channel
                || channel.scale.len() > limits.max_keys_per_channel
            {
                let actual = channel
                    .translation
                    .len()
                    .max(channel.rotation.len())
                    .max(channel.scale.len());
                return Err(ModelError::LimitExceeded {
                    what: "keys per channel",
                    actual: actual as u64,
                    max: limits.max_keys_per_channel as u64,
                });
            }
        }
    }

    if total_keys > limits.max_total_keys {
        return Err(ModelError::LimitExceeded {
            what: "animation keys",
            actual: total_keys as u64,
            max: limits.max_total_keys as u64,
        });
    }
    Ok(())
}

/// Every animated scale component must keep a single sign, both across the
/// track and relative to the joint's bind scale (so that the rest pose fed to
/// animation is compatible), and must stay away from zero.
fn check_sign_consistency(
    keys: &[raw::RawScaleKey],
    keys_ctx: &str,
    bind_scale: &[f32; 3],
    bind_ctx: &str,
) -> Result<(), ModelError> {
    if keys.is_empty() {
        return Ok(());
    }

    for (component, &bind_value) in bind_scale.iter().enumerate() {
        let reference = keys[0].value[component];
        let reference_sign = reference.is_sign_negative();
        for (k, key) in keys.iter().enumerate() {
            let value = key.value[component];
            if value.abs() < MIN_ABS_SCALE {
                return Err(ModelError::invalid(
                    format!("{keys_ctx}[{k}].value"),
                    format!(
                        "scale component {component} is {value}, too close to zero; \
                         animated scales must not collapse"
                    ),
                ));
            }
            if value.is_sign_negative() != reference_sign {
                return Err(ModelError::invalid(
                    format!("{keys_ctx}[{k}].value"),
                    format!(
                        "scale component {component} changes sign within the track \
                         (crossing zero is not allowed)"
                    ),
                ));
            }
        }
        // The bind pose is the base for channels without a scale track, and
        // the animation loops back to its first key; a sign mismatch between
        // the two would flip the joint.
        if bind_value.is_sign_negative() != reference_sign {
            return Err(ModelError::invalid(
                format!("{keys_ctx}[0].value"),
                format!(
                    "animated scale component {component} has a different sign than {bind_ctx} \
                     component {component}; the runtime pose would flip"
                ),
            ));
        }
    }
    Ok(())
}

/// Bounds and float checks for the mesh of a 3D model.
fn check_triangle_list_3d(
    indices: &[u32],
    vertices: &[raw::RawVertex3d],
    ctx: &str,
) -> Result<(), ModelError> {
    if indices.is_empty() {
        return Err(ModelError::invalid(
            ctx,
            "a mesh needs at least one triangle",
        ));
    }
    if !indices.len().is_multiple_of(3) {
        return Err(ModelError::invalid(
            ctx,
            format!(
                "index count {} is not a multiple of 3 (triangles only)",
                indices.len()
            ),
        ));
    }
    for (t, triangle) in indices.as_chunks::<3>().0.iter().enumerate() {
        let [i0, i1, i2] = [triangle[0], triangle[1], triangle[2]];
        for (k, index) in triangle.iter().enumerate() {
            if *index as usize >= vertices.len() {
                return Err(ModelError::invalid(
                    format!("{ctx}[{}]", t * 3 + k),
                    format!(
                        "vertex index {index} is out of range ({} vertices)",
                        vertices.len()
                    ),
                ));
            }
        }
        if i0 == i1 || i1 == i2 || i0 == i2 {
            return Err(ModelError::invalid(
                ctx,
                format!("triangle {t} repeats a vertex index ({i0}, {i1}, {i2})"),
            ));
        }
        let a = vertices[i0 as usize].position;
        let b = vertices[i1 as usize].position;
        let c = vertices[i2 as usize].position;
        if !triangle_area_ok(a, b, c) {
            return Err(ModelError::invalid(
                ctx,
                format!("triangle {t} is degenerate (zero area)"),
            ));
        }
    }
    Ok(())
}

/// Bounds and degeneracy checks for an authored 2D mesh.
fn check_triangle_list_2d(
    indices: &[u32],
    vertices: &[[f32; 2]],
    ctx: &str,
) -> Result<(), ModelError> {
    if indices.is_empty() {
        return Err(ModelError::invalid(
            ctx,
            "a mesh needs at least one triangle",
        ));
    }
    if !indices.len().is_multiple_of(3) {
        return Err(ModelError::invalid(
            ctx,
            format!(
                "index count {} is not a multiple of 3 (triangles only)",
                indices.len()
            ),
        ));
    }
    for (k, index) in indices.iter().enumerate() {
        if *index as usize >= vertices.len() {
            return Err(ModelError::invalid(
                format!("{ctx}[{k}]"),
                format!(
                    "vertex index {index} is out of range ({} vertices)",
                    vertices.len()
                ),
            ));
        }
    }
    for (t, triangle) in indices.as_chunks::<3>().0.iter().enumerate() {
        let [i0, i1, i2] = [triangle[0], triangle[1], triangle[2]];
        if i0 == i1 || i1 == i2 || i0 == i2 {
            return Err(ModelError::invalid(
                ctx,
                format!("triangle {t} repeats a vertex index ({i0}, {i1}, {i2})"),
            ));
        }
        let a = vertices[i0 as usize];
        let b = vertices[i1 as usize];
        let c = vertices[i2 as usize];
        let area2 = ((b[0] - a[0]) as f64) * ((c[1] - a[1]) as f64)
            - ((b[1] - a[1]) as f64) * ((c[0] - a[0]) as f64);
        let e1 = (((b[0] - a[0]) as f64).powi(2) + ((b[1] - a[1]) as f64).powi(2)).sqrt();
        let e2 = (((c[0] - a[0]) as f64).powi(2) + ((c[1] - a[1]) as f64).powi(2)).sqrt();
        if area2.abs().partial_cmp(&(1e-6 * (e1 * e2).max(1e-12))) != Some(Ordering::Greater) {
            return Err(ModelError::invalid(
                ctx,
                format!("triangle {t} is degenerate (zero area)"),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared checks
// ---------------------------------------------------------------------------

fn check_name(name: &str, ctx: &str) -> Result<(), ModelError> {
    if name.is_empty() {
        return Err(ModelError::invalid(ctx, "name must not be empty"));
    }
    let length = name.chars().count();
    if length > MAX_NAME_LEN {
        return Err(ModelError::invalid(
            ctx,
            format!("name is {length} characters, the limit is {MAX_NAME_LEN}"),
        ));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(ModelError::invalid(
            ctx,
            "name must not contain control characters",
        ));
    }
    Ok(())
}

fn check_duration(duration: f32, ctx: &str) -> Result<f32, ModelError> {
    let duration = finite(duration, ctx)?;
    if duration <= 0.0 {
        return Err(ModelError::invalid(
            ctx,
            format!("animation duration must be positive, found {duration}"),
        ));
    }
    Ok(duration)
}

fn check_key_time(time: f32, previous: f32, duration: f32, ctx: &str) -> Result<(), ModelError> {
    if time < 0.0 || time > duration {
        return Err(ModelError::invalid(
            format!("{ctx}.time"),
            format!("key time {time} is outside [0, {duration}]"),
        ));
    }
    if time <= previous {
        return Err(ModelError::invalid(
            format!("{ctx}.time"),
            format!("key times must strictly increase, found {time} after {previous}"),
        ));
    }
    Ok(())
}

fn check_color(color: [f32; 4], ctx: &str) -> Result<(), ModelError> {
    for (i, c) in color.iter().enumerate() {
        let c = finite(*c, &format!("{ctx}[{i}]"))?;
        if !(0.0..=1.0).contains(&c) {
            return Err(ModelError::invalid(
                format!("{ctx}[{i}]"),
                format!("color components must be in [0, 1], found {c}"),
            ));
        }
    }
    Ok(())
}

fn check_2d(point: [f32; 2], ctx: &str) -> Result<(), ModelError> {
    for (i, v) in point.iter().enumerate() {
        let v = finite(*v, &format!("{ctx}[{i}]"))?;
        if v.abs() > MAX_ABS_2D {
            return Err(ModelError::invalid(
                format!("{ctx}[{i}]"),
                format!("coordinate {v} is out of the supported range (+/-{MAX_ABS_2D})"),
            ));
        }
    }
    Ok(())
}

/// `[x, y, width, height]` with a positive, finite extent.
fn check_bounds(bounds: [f32; 4], ctx: &str) -> Result<(), ModelError> {
    for (i, v) in bounds.iter().enumerate() {
        let v = finite(*v, &format!("{ctx}[{i}]"))?;
        if v.abs() > MAX_ABS_2D {
            return Err(ModelError::invalid(
                format!("{ctx}[{i}]"),
                format!("value {v} is out of the supported range (+/-{MAX_ABS_2D})"),
            ));
        }
    }
    if bounds[2].partial_cmp(&0.0) != Some(Ordering::Greater)
        || bounds[3].partial_cmp(&0.0) != Some(Ordering::Greater)
    {
        return Err(ModelError::invalid(
            ctx,
            format!(
                "width and height must be positive, found [{}, {}]",
                bounds[2], bounds[3]
            ),
        ));
    }
    Ok(())
}

fn check_bounded_vec3(v: [f32; 3], ctx: &str) -> Result<(), ModelError> {
    for (i, x) in v.iter().enumerate() {
        let x = finite(*x, &format!("{ctx}[{i}]"))?;
        if x.abs() > MAX_ABS_3D {
            return Err(ModelError::invalid(
                format!("{ctx}[{i}]"),
                format!("value {x} is out of the supported range (+/-{MAX_ABS_3D})"),
            ));
        }
    }
    Ok(())
}

fn check_normal(n: [f32; 3], ctx: &str) -> Result<(), ModelError> {
    check_bounded_vec3(n, ctx)?;
    let length2 = n.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>();
    if length2 < 1e-8 {
        return Err(ModelError::invalid(ctx, "normal must be a non-zero vector"));
    }
    Ok(())
}

fn check_quat(q: [f32; 4], ctx: &str) -> Result<(), ModelError> {
    for (i, x) in q.iter().enumerate() {
        finite(*x, &format!("{ctx}[{i}]"))?;
    }
    let norm2 = q.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>();
    if !norm2.is_finite() {
        return Err(ModelError::invalid(ctx, "quaternion magnitude overflows"));
    }
    if norm2 < 1e-6 {
        return Err(ModelError::invalid(
            ctx,
            format!("quaternion is too close to zero (|q|^2 = {norm2})"),
        ));
    }
    Ok(())
}

/// Joint bind scale: finite, non-zero components, bounded magnitude and not
/// near-zero (the inverse-bind transform must exist).
fn check_joint_scale(scale: [f32; 3], ctx: &str) -> Result<(), ModelError> {
    check_bounded_vec3(scale, ctx)?;
    for (i, x) in scale.iter().enumerate() {
        if x.abs() < MIN_ABS_SCALE {
            return Err(ModelError::invalid(
                format!("{ctx}[{i}]"),
                format!("scale component {x} is too close to zero; it cannot be inverted"),
            ));
        }
    }
    Ok(())
}

/// Animated scale key: finite, bounded and away from zero.
fn check_scale_value(value: [f32; 3], ctx: &str) -> Result<(), ModelError> {
    check_bounded_vec3(value, ctx)?;
    for (i, x) in value.iter().enumerate() {
        if x.abs() < MIN_ABS_SCALE {
            return Err(ModelError::invalid(
                format!("{ctx}[{i}]"),
                format!("scale component {x} is too close to zero; it cannot be inverted"),
            ));
        }
    }
    Ok(())
}

fn finite(value: f32, ctx: &str) -> Result<f32, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::invalid(
            ctx,
            format!("value must be finite, found {value}"),
        ))
    }
}

fn require_absent<T>(value: Option<&T>, ctx: &str, kind: &str) -> Result<(), ModelError> {
    if value.is_some() {
        return Err(ModelError::invalid(
            ctx,
            format!("`{kind}` deformers do not accept this field"),
        ));
    }
    Ok(())
}

fn resolve_names(
    names: &[&str],
    name: &str,
    kind: &'static str,
    ctx: &str,
) -> Result<usize, ModelError> {
    match names.iter().position(|candidate| *candidate == name) {
        Some(index) => Ok(index),
        None => Err(ModelError::UnknownReference {
            kind,
            name: name.to_string(),
            context: ctx.to_string(),
        }),
    }
}

fn resolve_name(
    names: &[String],
    name: &str,
    kind: &'static str,
    ctx: &str,
) -> Result<usize, ModelError> {
    match names.iter().position(|candidate| candidate == name) {
        Some(index) => Ok(index),
        None => Err(ModelError::UnknownReference {
            kind,
            name: name.to_string(),
            context: ctx.to_string(),
        }),
    }
}

fn ensure_acyclic(parents: &[Option<usize>], context: &str) -> Result<(), ModelError> {
    let mut state = vec![0u8; parents.len()];
    for start in 0..parents.len() {
        if state[start] != 0 {
            continue;
        }
        let mut path = Vec::new();
        let mut node = start;
        loop {
            match state[node] {
                2 => break,
                1 => {
                    return Err(ModelError::Cycle {
                        context: context.to_string(),
                    });
                }
                _ => {}
            }
            state[node] = 1;
            path.push(node);
            match parents[node] {
                Some(parent) if parent < parents.len() => node = parent,
                _ => break,
            }
        }
        for visited in path {
            state[visited] = 2;
        }
    }
    Ok(())
}

/// Relative tolerance for key values that are compared against a parameter
/// range, so that a value stored as `max` by a different writer is accepted.
fn parameter_tolerance(min: f32, max: f32) -> f32 {
    (1e-6_f32).max((max - min).abs() * 1e-6)
}

fn add_budget(current: usize, add: usize, what: &'static str) -> Result<usize, ModelError> {
    current
        .checked_add(add)
        .ok_or(ModelError::SizeOverflow { what })
}

fn mul_budget(a: usize, b: usize, what: &'static str) -> Result<usize, ModelError> {
    a.checked_mul(b).ok_or(ModelError::SizeOverflow { what })
}

fn distance2(a: [f32; 2], b: [f32; 2]) -> f64 {
    let dx = (a[0] - b[0]) as f64;
    let dy = (a[1] - b[1]) as f64;
    dx * dx + dy * dy
}

/// Twice the signed area of the polygon, plus the squared diagonal of its
/// bounding box (used to scale the degeneracy threshold).
fn polygon_area_and_extent(points: &[[f32; 2]]) -> (f64, f64) {
    let n = points.len();
    let mut area2 = 0.0f64;
    for i in 0..n {
        let a = points[i];
        let b = points[(i + 1) % n];
        area2 += (a[0] as f64) * (b[1] as f64) - (b[0] as f64) * (a[1] as f64);
    }
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in points {
        min_x = min_x.min(p[0] as f64);
        min_y = min_y.min(p[1] as f64);
        max_x = max_x.max(p[0] as f64);
        max_y = max_y.max(p[1] as f64);
    }
    let diag2 = (max_x - min_x).powi(2) + (max_y - min_y).powi(2);
    (area2.abs() * 0.5, diag2)
}

/// Simple-polygon test: no two non-adjacent edges may intersect or touch.
fn is_simple_polygon(points: &[[f32; 2]]) -> bool {
    let n = points.len();
    if n < 3 {
        return false;
    }
    for i in 0..n {
        let a1 = points[i];
        let a2 = points[(i + 1) % n];
        for j in (i + 1)..n {
            // Skip the edge itself and its two neighbours (shared endpoint).
            if j == i || j == (i + 1) % n || (i == 0 && j == n - 1) {
                continue;
            }
            let b1 = points[j];
            let b2 = points[(j + 1) % n];
            if segments_intersect(a1, a2, b1, b2) {
                return false;
            }
        }
    }
    true
}

fn segments_intersect(p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], p4: [f32; 2]) -> bool {
    let d1 = orient(p3, p4, p1);
    let d2 = orient(p3, p4, p2);
    let d3 = orient(p1, p2, p3);
    let d4 = orient(p1, p2, p4);

    if ((d1 > 0.0) != (d2 > 0.0))
        && ((d3 > 0.0) != (d4 > 0.0))
        && d1 != 0.0
        && d2 != 0.0
        && d3 != 0.0
        && d4 != 0.0
    {
        return true;
    }
    // Collinear / touching cases.
    (d1 == 0.0 && on_segment(p3, p4, p1))
        || (d2 == 0.0 && on_segment(p3, p4, p2))
        || (d3 == 0.0 && on_segment(p1, p2, p3))
        || (d4 == 0.0 && on_segment(p1, p2, p4))
}

fn orient(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f64 {
    let abx = (b[0] - a[0]) as f64;
    let aby = (b[1] - a[1]) as f64;
    let acx = (c[0] - a[0]) as f64;
    let acy = (c[1] - a[1]) as f64;
    abx * acy - aby * acx
}

/// `r` lies on segment `pq` (assuming `r` is collinear with it).
fn on_segment(p: [f32; 2], q: [f32; 2], r: [f32; 2]) -> bool {
    r[0] >= p[0].min(q[0])
        && r[0] <= p[0].max(q[0])
        && r[1] >= p[1].min(q[1])
        && r[1] <= p[1].max(q[1])
}

/// A triangle is usable when its vertices are distinct and its area is not
/// vanishing relative to its edge lengths.
fn triangle_area_ok(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> bool {
    let e1 = [
        (b[0] - a[0]) as f64,
        (b[1] - a[1]) as f64,
        (b[2] - a[2]) as f64,
    ];
    let e2 = [
        (c[0] - a[0]) as f64,
        (c[1] - a[1]) as f64,
        (c[2] - a[2]) as f64,
    ];
    let cross = [
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ];
    let area = (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
    let len1 = (e1[0] * e1[0] + e1[1] * e1[1] + e1[2] * e1[2]).sqrt();
    let len2 = (e2[0] * e2[0] + e2[1] * e2[1] + e2[2] * e2[2]).sqrt();
    let scale = len1 * len2;
    scale > 0.0 && area > 1e-6 * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_reject_nonfinite_and_nonpositive_extents() {
        assert!(check_bounds([0.0, 0.0, 1.0, 1.0], "bounds").is_ok());
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 0.0] {
            for component in 2..4 {
                let mut bounds = [0.0, 0.0, 1.0, 1.0];
                bounds[component] = value;
                assert!(check_bounds(bounds, "bounds").is_err());
            }
        }
    }

    #[test]
    fn triangle_lists_reject_remainders_before_chunking() {
        let positions = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let vertices = positions.map(|[x, y]| raw::RawVertex3d {
            position: [x, y, 0.0],
            normal: [0.0, 0.0, 1.0],
            color: [1.0; 4],
            joints: [0; 4],
            weights: [1.0, 0.0, 0.0, 0.0],
        });
        let indices = [0, 1, 2, 0, 1, 2];
        for length in [1, 2, 4, 5] {
            assert!(check_triangle_list_2d(&indices[..length], &positions, "indices").is_err());
            assert!(check_triangle_list_3d(&indices[..length], &vertices, "indices").is_err());
        }
        assert!(check_triangle_list_2d(&indices, &positions, "indices").is_ok());
        assert!(check_triangle_list_3d(&indices, &vertices, "indices").is_ok());
    }

    #[test]
    fn scale_consistency_checks_every_component() {
        let keys = [
            raw::RawScaleKey {
                time: 0.0,
                value: [1.0; 3],
            },
            raw::RawScaleKey {
                time: 1.0,
                value: [2.0; 3],
            },
        ];
        assert!(check_sign_consistency(&keys, "keys", &[1.0; 3], "bind").is_ok());
        for component in 0..3 {
            let mut bind = [1.0; 3];
            bind[component] = -1.0;
            assert!(check_sign_consistency(&keys, "keys", &bind, "bind").is_err());
            let mut value = [2.0; 3];
            value[component] = -2.0;
            let changed = [
                raw::RawScaleKey {
                    time: 0.0,
                    value: [1.0; 3],
                },
                raw::RawScaleKey { time: 1.0, value },
            ];
            assert!(check_sign_consistency(&changed, "keys", &[1.0; 3], "bind").is_err());
        }
    }
}
