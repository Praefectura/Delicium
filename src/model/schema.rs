//! Raw `.dmodel.json` schema: the wire shape, before validation.
//!
//! This module is intentionally dumb: every structure is a plain serde struct
//! with `deny_unknown_fields`, so an unknown field anywhere in the document is
//! rejected instead of silently ignored. Enumerations (`kind`, deformer
//! `type`, animation `interpolation`) are carried as plain strings and checked
//! explicitly in [`validate`](super::validate), which yields exact error
//! messages instead of relying on serde tag-dispatch subtleties.
//!
//! The schema is fully self-contained: no file paths, URLs, scripts or
//! executable content, no external resources. Vector conventions:
//!
//! - 2D points and offsets are `[x, y]`; warp bounds are `[x, y, width, height]`.
//! - 3D positions/translations/scales are `[x, y, z]` (right-handed, Y up).
//! - Rotations are quaternions written `[x, y, z, w]`.
//! - Colors are straight (non-premultiplied) `[r, g, b, a]` in `[0, 1]`.

use serde::Deserialize;

/// Exact `format` string of every v0.1 document.
pub const FORMAT: &str = "delicium.model";
/// Exact `format_version` string accepted by this build.
pub const FORMAT_VERSION: &str = "0.1";

/// Top-level document.
///
/// `kind` selects which payload must be present: `sprite2d` requires
/// `sprite2d` and forbids `skeleton3d`, `skeleton3d` is the mirror image.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawEnvelope {
    /// Must equal [`FORMAT`].
    pub format: String,
    /// Must equal [`FORMAT_VERSION`].
    pub format_version: String,
    /// Model name, `1..=max_name_len` characters, no control characters.
    pub name: String,
    /// `"sprite2d"` or `"skeleton3d"`.
    pub kind: String,
    /// Optional interaction capabilities.
    #[serde(default)]
    pub interaction: RawInteraction,
    /// Payload for `kind == "sprite2d"`.
    #[serde(default)]
    pub sprite2d: Option<RawSprite2d>,
    /// Payload for `kind == "skeleton3d"`.
    #[serde(default)]
    pub skeleton3d: Option<RawSkeleton3d>,
}

/// Capabilities the simulation may drive. Unresolvable names are load errors;
/// a capability the model does not declare is simply absent.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawInteraction {
    /// `sprite2d` only: parameter driven by the gaze interaction.
    #[serde(default)]
    pub gaze_parameter: Option<String>,
    /// `skeleton3d` only: joint that turns toward the gaze target.
    #[serde(default)]
    pub head_joint: Option<String>,
}

// ---------------------------------------------------------------------------
// 2D
// ---------------------------------------------------------------------------

/// Payload of a `sprite2d` model.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSprite2d {
    /// Scalar parameters, unique by name.
    pub parameters: Vec<RawParameter>,
    /// Draw-ordered layers, index `0` is drawn first.
    pub layers: Vec<RawLayer2d>,
    /// Deformers; layers and deformers reference them by name.
    #[serde(default)]
    pub deformers: Vec<RawDeformer2d>,
    /// Parameter animations.
    #[serde(default)]
    pub animations: Vec<RawParameterAnimation>,
}

/// One scalar parameter.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawParameter {
    /// Unique name within the model.
    pub name: String,
    /// Lower bound; `min <= default <= max` and `min < max` are required.
    pub min: f32,
    /// Upper bound.
    pub max: f32,
    /// Rest value.
    pub default: f32,
}

/// One colored layer: either an explicit mesh or a generated grid.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawLayer2d {
    /// Unique name within the model.
    pub name: String,
    /// Straight RGBA in `[0, 1]`.
    pub color: [f32; 4],
    /// Explicit local-space vertices; requires `indices` and forbids `grid`.
    #[serde(default)]
    pub vertices: Option<Vec<[f32; 2]>>,
    /// Triangle list, length a multiple of 3; requires `vertices`.
    #[serde(default)]
    pub indices: Option<Vec<u32>>,
    /// Generated grid; forbids `vertices`/`indices`.
    #[serde(default)]
    pub grid: Option<RawGrid2d>,
    /// Name of the deformer applied to this layer, if any.
    #[serde(default)]
    pub deformer: Option<String>,
    /// Optional polygon mask, same local space as the vertices.
    #[serde(default)]
    pub mask: Option<RawMask2d>,
}

/// A rectangular grid mesh.
///
/// `columns` and `rows` count quads, so the generated vertex grid is
/// `(columns + 1) * (rows + 1)` with both dimensions `>= 2`; the triangle
/// count is `columns * rows * 2`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawGrid2d {
    /// `[x, y, width, height]`, `width > 0`, `height > 0`.
    pub bounds: [f32; 4],
    /// Quads along X, `>= 1`.
    pub columns: usize,
    /// Quads along Y, `>= 1`.
    pub rows: usize,
}

/// A closed polygon mask in layer-local space; the last point is implicitly
/// joined to the first. Must be simple (non self-intersecting) with at least
/// three non-collinear points.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawMask2d {
    /// Polygon points, `3..=max_mask_points`.
    pub points: Vec<[f32; 2]>,
}

/// A deformer declaration.
///
/// `type` selects the shape of the record: `"rotation"` requires `pivot` and
/// `max_angle_radians` and forbids the warp fields, `"warp"` is the mirror
/// image. Fields that a given type does not use must be absent.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawDeformer2d {
    /// Unique name within the model.
    pub name: String,
    /// Deformer kind: `"rotation"` or `"warp"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Name of the deformer applied before this one, if any.
    #[serde(default)]
    pub parent: Option<String>,
    /// Name of the parameter that drives this deformer.
    pub parameter: String,
    /// `rotation` only: rotation center in layer-local space.
    #[serde(default)]
    pub pivot: Option<[f32; 2]>,
    /// `rotation` only: radians applied at parameter value `1.0`.
    #[serde(default)]
    pub max_angle_radians: Option<f32>,
    /// `warp` only: `[x, y, width, height]` of the warped region.
    #[serde(default)]
    pub bounds: Option<[f32; 4]>,
    /// `warp` only: control grid `[columns, rows]`, both `>= 2`.
    #[serde(default)]
    pub grid: Option<[usize; 2]>,
    /// `warp` only: `columns * rows` control displacements, row-major.
    #[serde(default)]
    pub offsets: Option<Vec<[f32; 2]>>,
}

/// A looping animation that drives 2D parameters.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawParameterAnimation {
    /// Unique name within the model.
    pub name: String,
    /// Cycle length in seconds, `> 0` and finite.
    pub duration: f32,
    /// Parameter tracks; at most one per parameter.
    pub channels: Vec<RawParamChannel>,
}

/// One animated parameter.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawParamChannel {
    /// Name of the animated parameter.
    pub parameter: String,
    /// Keys sorted by strictly increasing time inside `[0, duration]`.
    pub keys: Vec<RawScalarKey>,
}

/// One scalar keyframe.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawScalarKey {
    /// Time in seconds.
    pub time: f32,
    /// Value, inside the parameter's declared range.
    pub value: f32,
}

// ---------------------------------------------------------------------------
// 3D
// ---------------------------------------------------------------------------

/// Payload of a `skeleton3d` model.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSkeleton3d {
    /// Skinned vertices in bind space.
    pub vertices: Vec<RawVertex3d>,
    /// Triangle list, length a multiple of 3.
    pub indices: Vec<u32>,
    /// Joints; parents are referenced by name and may be declared in any
    /// order (the compiler reorders them).
    pub joints: Vec<RawJoint>,
    /// Joint animations.
    #[serde(default)]
    pub animations: Vec<RawSkeletalAnimation>,
}

/// One vertex of the bind mesh.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawVertex3d {
    /// Bind-space position.
    pub position: [f32; 3],
    /// Unit-length outward normal.
    pub normal: [f32; 3],
    /// Straight RGBA in `[0, 1]`.
    pub color: [f32; 4],
    /// Four indices into `joints`.
    pub joints: [u32; 4],
    /// Four non-negative weights with a positive sum; normalized on compile.
    pub weights: [f32; 4],
}

/// One joint of the hierarchy.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawJoint {
    /// Unique name within the model.
    pub name: String,
    /// Name of the parent joint; `null` (or absent) for a root joint.
    #[serde(default)]
    pub parent: Option<String>,
    /// Rest translation relative to the parent.
    pub translation: [f32; 3],
    /// Rest rotation relative to the parent, `[x, y, z, w]`.
    pub rotation: [f32; 4],
    /// Rest scale relative to the parent, all components non-zero.
    pub scale: [f32; 3],
}

/// A looping joint animation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSkeletalAnimation {
    /// Unique name within the model.
    pub name: String,
    /// Cycle length in seconds, `> 0` and finite.
    pub duration: f32,
    /// Joint tracks; at most one per joint.
    pub channels: Vec<RawSkeletalChannel>,
}

/// Tracks for a single joint. Empty tracks leave that component at its bind
/// pose; at least one track must be non-empty.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSkeletalChannel {
    /// Name of the animated joint.
    pub joint: String,
    /// `"linear"` (default) or `"step"`.
    #[serde(default)]
    pub interpolation: Option<String>,
    /// Translation keys.
    #[serde(default)]
    pub translation: Vec<RawTranslationKey>,
    /// Rotation keys.
    #[serde(default)]
    pub rotation: Vec<RawRotationKey>,
    /// Scale keys.
    #[serde(default)]
    pub scale: Vec<RawScaleKey>,
}

/// A translation keyframe.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTranslationKey {
    /// Time in seconds.
    pub time: f32,
    /// Translation relative to the parent.
    pub value: [f32; 3],
}

/// A rotation keyframe.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRotationKey {
    /// Time in seconds.
    pub time: f32,
    /// Rotation relative to the parent, `[x, y, z, w]`.
    pub value: [f32; 4],
}

/// A scale keyframe.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawScaleKey {
    /// Time in seconds.
    pub time: f32,
    /// Scale relative to the parent; every component keeps one sign across
    /// the whole track so the runtime scale never crosses zero.
    pub value: [f32; 3],
}
