//! Public runtime types produced by the compiler.
//!
//! Everything here is immutable, `Send + Sync` and shareable through
//! [`Arc`](std::sync::Arc). The renderer and the simulation read these
//! structures directly; they never see the raw JSON schema.

use glam::{Mat4, Quat, Vec3};

/// Which native model family a compiled asset belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelKind {
    /// Layered parameter-driven 2D mesh with rotation/warp deformers.
    Sprite2d,
    /// Skinned 3D triangle mesh with a joint hierarchy and joint animations.
    Skeleton3d,
}

impl ModelKind {
    /// Canonical schema string, matching the `kind` field of the file format.
    pub const fn as_str(self) -> &'static str {
        match self {
            ModelKind::Sprite2d => "sprite2d",
            ModelKind::Skeleton3d => "skeleton3d",
        }
    }
}

impl std::fmt::Display for ModelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A fully validated, compiled model asset.
///
/// Shared between every instance that uses it; instance state (time,
/// parameter values, joint poses) lives in the simulation, never here.
#[derive(Debug)]
pub struct CompiledModel {
    /// Model name as declared by the file.
    pub name: String,
    /// Kind-specific compiled payload.
    pub data: ModelData,
    /// Declared interaction capabilities, resolved to indices at compile time.
    pub interaction: InteractionBinding,
}

impl CompiledModel {
    /// Kind of this model; always agrees with the `data` variant.
    pub fn kind(&self) -> ModelKind {
        self.data.kind()
    }
}

/// Kind-specific compiled payload.
#[derive(Debug)]
pub enum ModelData {
    /// 2D payload for [`ModelKind::Sprite2d`].
    Sprite2d(CompiledSprite2d),
    /// 3D payload for [`ModelKind::Skeleton3d`].
    Skeleton3d(CompiledSkeleton3d),
}

impl ModelData {
    /// Kind of this payload.
    pub fn kind(&self) -> ModelKind {
        match self {
            ModelData::Sprite2d(_) => ModelKind::Sprite2d,
            ModelData::Skeleton3d(_) => ModelKind::Skeleton3d,
        }
    }
}

// ---------------------------------------------------------------------------
// Interaction
// ---------------------------------------------------------------------------

/// How the simulation may interact with this model.
///
/// Both fields are resolved from names in the file to indices in the compiled
/// payload. A capability the model does not declare is `None`; the simulation
/// must skip it rather than guess an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InteractionBinding {
    /// Index into [`CompiledSprite2d::parameters`] driven by the gaze
    /// interaction (2D models only).
    pub gaze_parameter: Option<usize>,
    /// Index into [`CompiledSkeleton3d::joints`] that turns toward the gaze
    /// target (3D models only).
    pub head_joint: Option<usize>,
}

// ---------------------------------------------------------------------------
// 2D
// ---------------------------------------------------------------------------

/// Compiled 2D asset: parameter set, draw-ordered layers, deformers and
/// parameter animations.
///
/// Layers are stored in draw order: index `0` is drawn first (bottom).
#[derive(Debug)]
pub struct CompiledSprite2d {
    /// Declared parameters, indices are the parameter namespace.
    pub parameters: Vec<Parameter>,
    /// Draw-ordered layers.
    pub layers: Vec<Layer2d>,
    /// Deformers, stored in dependency order: a deformer's parent (if any)
    /// always has a smaller index, so a single forward pass is enough.
    pub deformers: Vec<Deformer2d>,
    /// Parameter animations.
    pub animations: Vec<ParameterAnimation>,
}

impl CompiledSprite2d {
    /// Index of the parameter with `name`, if declared.
    pub fn parameter_index(&self, name: &str) -> Option<usize> {
        self.parameters.iter().position(|p| p.name == name)
    }
}

/// A scalar parameter with a declared range.
#[derive(Debug, Clone)]
pub struct Parameter {
    /// Unique name within the model.
    pub name: String,
    /// Lower bound, must satisfy `min <= default`.
    pub min: f32,
    /// Upper bound, must satisfy `default <= max` and `min < max`.
    pub max: f32,
    /// Rest value.
    pub default: f32,
}

/// One colored mesh layer.
///
/// `vertices` and `indices` are in layer-local space; the renderer offsets
/// them with the instance transform. `indices` are triangle indices into
/// `vertices`.
#[derive(Debug)]
pub struct Layer2d {
    /// Unique layer name (draw order is the vector order, not the name).
    pub name: String,
    /// Local-space triangle vertices.
    pub vertices: Vec<[f32; 2]>,
    /// Triangle list indices, length is a multiple of 3.
    pub indices: Vec<u32>,
    /// Straight (non-premultiplied) RGBA color.
    pub color: [f32; 4],
    /// Index into [`CompiledSprite2d::deformers`], if the layer is deformed.
    pub deformer: Option<usize>,
    /// Optional polygon mask in the same local space as `vertices`.
    pub mask: Option<Mask2d>,
}

/// A closed polygon mask; geometry outside it is clipped by the renderer.
///
/// The polygon is simple (non self-intersecting), has at least three
/// non-degenerate points and is not closed explicitly: the last point is
/// implicitly connected to the first.
#[derive(Debug, Clone)]
pub struct Mask2d {
    /// Polygon points in layer-local space.
    pub points: Vec<[f32; 2]>,
}

/// A parameter-driven deformation applied to a layer.
///
/// Deformers form a hierarchy: `parent` names another deformer to apply
/// first, and the compiler guarantees `parent < index` for every deformer.
/// A parameter value of `0.0` is the neutral pose; positive and negative
/// values scale the deformation in opposite directions.
#[derive(Debug)]
pub enum Deformer2d {
    /// Rotates the layer around `pivot` by `parameter * max_angle_radians`
    /// (counter-clockwise, radians).
    Rotation {
        /// Deformer applied before this one, if any. Always `< own index`.
        parent: Option<usize>,
        /// Index into [`CompiledSprite2d::parameters`].
        parameter: usize,
        /// Rotation center in layer-local space.
        pivot: [f32; 2],
        /// Rotation applied at `parameter == 1.0`, in radians.
        max_angle_radians: f32,
    },
    /// Bilinear control-grid warp over `bounds`.
    ///
    /// `offsets[row * grid[0] + column]` is the displacement applied to the
    /// point at normalized position `(column / (grid[0] - 1), row / (grid[1] -
    /// 1))` inside `bounds`. That is: row-major with the **column count as the
    /// stride**, `grid[0] = columns`, `grid[1] = rows`, exactly
    /// `columns * rows` entries for any (including a non-square) grid.
    ///
    /// A vertex inside `bounds` is displaced by the bilinear interpolation of
    /// the four surrounding offsets; vertices outside are clamped to the
    /// border. The displacement is scaled by the parameter value, so the sign
    /// of the parameter reverses the deformation.
    Warp {
        /// Deformer applied before this one, if any. Always `< own index`.
        parent: Option<usize>,
        /// Index into [`CompiledSprite2d::parameters`].
        parameter: usize,
        /// `[x, y, width, height]` of the warp region, `width > 0`,
        /// `height > 0`.
        bounds: [f32; 4],
        /// Control grid `[columns, rows]`, both `>= 2`; the stored offset
        /// count is exactly `columns * rows`.
        grid: [usize; 2],
        /// Control displacements, row-major, `offsets[row * columns + col]`.
        offsets: Vec<[f32; 2]>,
    },
}

/// A looping animation that drives 2D parameters.
#[derive(Debug)]
pub struct ParameterAnimation {
    /// Unique name within the model.
    pub name: String,
    /// Cycle length in seconds, `> 0`; sampling wraps modulo the duration.
    pub duration: f32,
    /// Parameter tracks, at most one per parameter.
    pub channels: Vec<ParamChannel>,
}

/// One animated parameter.
#[derive(Debug)]
pub struct ParamChannel {
    /// Index into [`CompiledSprite2d::parameters`].
    pub parameter: usize,
    /// Keys sorted by strictly increasing time inside `[0, duration]`.
    pub keys: Vec<ScalarKey>,
}

/// A scalar keyframe. Sampled linearly, then clamped to the parameter range.
#[derive(Debug, Clone, Copy)]
pub struct ScalarKey {
    /// Time in seconds inside `[0, duration]`.
    pub time: f32,
    /// Parameter value at `time`.
    pub value: f32,
}

// ---------------------------------------------------------------------------
// 3D
// ---------------------------------------------------------------------------

/// Compiled 3D asset: skinned mesh, joint hierarchy and joint animations.
#[derive(Debug)]
pub struct CompiledSkeleton3d {
    /// Mesh vertices in bind/rest space.
    pub vertices: Vec<Vertex3d>,
    /// Triangle list indices into `vertices`, length is a multiple of 3.
    pub indices: Vec<u32>,
    /// Joints in topological order: `joints[i].parent < i` for every joint,
    /// so a single forward pass computes world transforms.
    pub joints: Vec<Joint>,
    /// Inverse of each joint's world bind transform, indexed like `joints`.
    pub inverse_bind: Vec<Mat4>,
    /// Joint animations.
    pub animations: Vec<SkeletalAnimation>,
}

impl CompiledSkeleton3d {
    /// Index of the joint with `name`, if declared.
    pub fn joint_index(&self, name: &str) -> Option<usize> {
        self.joints.iter().position(|j| j.name == name)
    }
}

/// A skinned vertex in bind space.
///
/// `joints` indexes into [`CompiledSkeleton3d::joints`], `weights` are the
/// matching skinning weights. Weights are normalized at compile time to sum
/// to `1.0`; entries beyond the last non-zero weight repeat the first joint
/// with weight `0.0` and never influence the result.
#[derive(Debug, Clone, Copy)]
pub struct Vertex3d {
    /// Bind-space position.
    pub position: [f32; 3],
    /// Unit-length bind-space normal.
    pub normal: [f32; 3],
    /// Straight (non-premultiplied) RGBA color.
    pub color: [f32; 4],
    /// Four joint indices.
    pub joints: [u32; 4],
    /// Four skinning weights summing to `1.0`.
    pub weights: [f32; 4],
}

/// A joint in the hierarchy.
///
/// `translation`/`rotation`/`scale` are the bind pose, relative to `parent`.
#[derive(Debug, Clone)]
pub struct Joint {
    /// Unique name within the model.
    pub name: String,
    /// Parent joint; always `< own index`.
    pub parent: Option<usize>,
    /// Rest translation relative to the parent.
    pub translation: Vec3,
    /// Rest rotation relative to the parent, normalized unit quaternion.
    pub rotation: Quat,
    /// Rest scale relative to the parent, all components non-zero.
    pub scale: Vec3,
}

/// Local pose of one joint relative to its parent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JointTransform {
    /// Translation relative to the parent.
    pub translation: Vec3,
    /// Rotation relative to the parent.
    pub rotation: Quat,
    /// Scale relative to the parent.
    pub scale: Vec3,
}

impl Default for JointTransform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

/// How keyframe values are interpolated between two keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationInterpolation {
    /// Linear blend of neighbouring keys (quaternions use the shortest arc).
    #[default]
    Linear,
    /// Hold the earlier key's value until the next key's time.
    Step,
}

/// A looping joint animation.
#[derive(Debug)]
pub struct SkeletalAnimation {
    /// Unique name within the model.
    pub name: String,
    /// Cycle length in seconds, `> 0`; sampling wraps modulo the duration.
    pub duration: f32,
    /// Per-joint tracks, at most one per joint.
    pub channels: Vec<SkeletalChannel>,
}

/// Tracks for a single joint.
///
/// Each track is either empty (that component keeps its bind pose) or holds
/// keys sorted by strictly increasing time inside `[0, duration]`; at least
/// one track is non-empty.
#[derive(Debug)]
pub struct SkeletalChannel {
    /// Index into [`CompiledSkeleton3d::joints`].
    pub joint: usize,
    /// Interpolation applied to every track of this channel.
    pub interpolation: AnimationInterpolation,
    /// Translation track.
    pub translation: Vec<TranslationKey>,
    /// Rotation track.
    pub rotation: Vec<RotationKey>,
    /// Scale track.
    pub scale: Vec<ScaleKey>,
}

/// A translation keyframe.
#[derive(Debug, Clone, Copy)]
pub struct TranslationKey {
    /// Time in seconds inside `[0, duration]`.
    pub time: f32,
    /// Translation relative to the parent at `time`.
    pub value: Vec3,
}

/// A rotation keyframe.
#[derive(Debug, Clone, Copy)]
pub struct RotationKey {
    /// Time in seconds inside `[0, duration]`.
    pub time: f32,
    /// Unit quaternion relative to the parent at `time`; interpolation takes
    /// the shortest arc.
    pub value: Quat,
}

/// A scale keyframe.
#[derive(Debug, Clone, Copy)]
pub struct ScaleKey {
    /// Time in seconds inside `[0, duration]`.
    pub time: f32,
    /// Scale relative to the parent at `time`, non-zero.
    pub value: Vec3,
}
