//! Raw `0.2` `.dmodel.json` schema: the wire shape, before validation.
//!
//! `0.2` is a strict superset of the `0.1` geometry with three additions:
//!
//! - a top-level `images` table (`id` + ordinary PNG bytes, standard padding
//!   Base64, no data URI / path / URL);
//! - optional per-layer / per-mesh `texture` id plus `uvs`, and a basic 3D
//!   `material` (roughness, metallic, alpha_mode, alpha_cutoff).
//!
//! Everything that did not change is reused verbatim from
//! [`schema`](super::schema) (`RawParameter`, `RawGrid2d`, `RawMask2d`,
//! deformers, animations, `RawVertex3d`, `RawJoint`), so the two versions
//! cannot drift apart. Like `0.1`, every structure denies unknown fields and
//! enums are carried as plain strings, checked in
//! [`validate`](super::validate) for exact error messages.
//!
//! A `0.2` document without `images`, `texture` or `material` is valid and
//! compiles to the original color/vertex-color output.
//!
//! An optional top-level `actions` table can bind the idle/walk/reaction
//! roles (required when the table is present) and the optional greet/play
//! slots to animations of this model by name. It is exclusive to `0.2` and
//! deliberately names the *capability*, not a clip kind: older builds reject
//! the field (`deny_unknown_fields`), so a document using it cannot be
//! half-loaded.

use serde::Deserialize;

use super::schema::{
    RawDeformer2d, RawGrid2d, RawInteraction, RawJoint, RawMask2d, RawParameter,
    RawParameterAnimation, RawSkeletalAnimation, RawVertex3d,
};

/// Exact `format_version` string of every v0.2 document.
pub const FORMAT_VERSION: &str = "0.2";

/// Top-level `0.2` document.
///
/// `kind` selects which payload must be present, mirroring `0.1`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawEnvelope {
    /// Must equal [`schema::FORMAT`](super::schema::FORMAT).
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
    /// Optional action-role bindings; all three roles are required once the
    /// table is declared, and each names an animation of this model.
    #[serde(default)]
    pub actions: Option<RawActions>,
    /// Embedded image table; layers and meshes reference entries by id.
    #[serde(default)]
    pub images: Vec<RawImage>,
    /// Payload for `kind == "sprite2d"`.
    #[serde(default)]
    pub sprite2d: Option<RawSprite2d>,
    /// Payload for `kind == "skeleton3d"`.
    #[serde(default)]
    pub skeleton3d: Option<RawSkeleton3d>,
}

/// Top-level `0.2` action-role bindings. The three base roles (`idle`,
/// `walk`, `reaction`) are required once the table is present; `greet` and
/// `play` are optional additional slots. Each declared value names an
/// animation of the model (the animation lists of the declared `kind`).
/// Reusing one clip for several roles is allowed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawActions {
    /// Name of the idle animation.
    pub idle: String,
    /// Name of the walk animation.
    pub walk: String,
    /// Name of the reaction animation.
    pub reaction: String,
    /// Optional name of the greeting animation.
    #[serde(default)]
    pub greet: Option<String>,
    /// Optional name of the play animation.
    #[serde(default)]
    pub play: Option<String>,
}

/// One embedded image: a unique id and the raw PNG bytes, standard-padded
/// Base64. No paths, URLs, data URIs or external resources.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawImage {
    /// Unique name within the model; references must match exactly.
    pub id: String,
    /// Standard Base64 (padded, no whitespace) of one PNG file.
    pub png_base64: String,
}

// ---------------------------------------------------------------------------
// 2D
// ---------------------------------------------------------------------------

/// Payload of a `sprite2d` model: the unchanged `0.1` shape plus optional
/// per-layer texture bindings.
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

/// One layer: either an explicit mesh or a generated grid, optionally bound
/// to an image.
///
/// `uvs` is required and one-to-one with the vertices when a `texture` is
/// declared on an explicit mesh; a generated grid derives its UVs from the
/// rest grid when `uvs` is absent.
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
    /// Id of the base color image, if the layer is textured.
    #[serde(default)]
    pub texture: Option<String>,
    /// Explicit UVs, one per vertex, origin top-left, finite and in `[0, 1]`.
    /// Without a texture this may be omitted; with one it is required for
    /// explicit meshes and optional (auto-derived) for grids.
    #[serde(default)]
    pub uvs: Option<Vec<[f32; 2]>>,
}

// ---------------------------------------------------------------------------
// 3D
// ---------------------------------------------------------------------------

/// Payload of a `skeleton3d` model: the unchanged flat `0.1` shape plus an
/// optional texture, parallel UVs and a basic material.
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
    /// Id of the base color image, if the mesh is textured.
    #[serde(default)]
    pub texture: Option<String>,
    /// Explicit UVs, one per vertex (same parallel-array contract as
    /// `vertices`), origin top-left, finite and in `[0, 1]`. Required when
    /// `texture` is present; optional without one.
    #[serde(default)]
    pub uvs: Option<Vec<[f32; 2]>>,
    /// Basic material; without one a texture selects the default material
    /// and an untextured model keeps the legacy vertex-color path.
    #[serde(default)]
    pub material: Option<RawMaterial3d>,
}

/// Basic material declaration. Every field is optional; absent fields take
/// the documented defaults. A model listing `alpha_mode: "opaque"` must be
/// fully opaque (no explicit `alpha_cutoff`), `"cutout"` may declare a cutoff
/// in `(0, 1]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawMaterial3d {
    /// Surface roughness in `[0, 1]`; default `0.6`.
    #[serde(default)]
    pub roughness: Option<f32>,
    /// Metalness in `[0, 1]`; default `0.0`.
    #[serde(default)]
    pub metallic: Option<f32>,
    /// `"opaque"` (default) or `"cutout"`.
    #[serde(default)]
    pub alpha_mode: Option<String>,
    /// Cutout threshold in `(0, 1]`; default `0.5`. Only valid with
    /// `"cutout"`.
    #[serde(default)]
    pub alpha_cutoff: Option<f32>,
}
