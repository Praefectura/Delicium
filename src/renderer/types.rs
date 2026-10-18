use std::{sync::Arc, time::Duration};

use glam::Mat4;

/// Re-exported model CPU DTOs so tests and callers can name the exact types
/// the renderer accepts without importing from `crate::model` directly.
pub use crate::model::{AlphaMode, Material3d, ModelImage};

pub type RenderAssetId = u64;
pub type RenderInstanceId = u64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendChoice {
    #[default]
    Auto,
    Dx12,
    Vulkan,
}

#[derive(Clone, Debug)]
pub struct LayerAsset2d {
    pub vertices: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub color: [f32; 4],
    pub mask: Option<Vec<[f32; 2]>>,
    /// Static artist UVs, one per vertex, origin top-left, values in `[0, 1]`.
    ///
    /// May be empty for an untextured layer; the renderer then generates a
    /// static zero UV region once at registration.
    pub uvs: Vec<[f32; 2]>,
    /// Index into [`RenderAsset::Sprite2d`]'s `images` for the base color
    /// texture, or `None` for the untextured color-only path.
    pub texture: Option<usize>,
}

/// One skinned vertex. `repr(C)` pins the exact 80-byte layout the GPU vertex
/// buffer expects (offsets 0/12/24/40/56/72, asserted in the renderer).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SkinVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 4],
    pub joints: [u32; 4],
    pub weights: [f32; 4],
    /// Static base color texture coordinate; not recomputed per skinning step.
    pub uv: [f32; 2],
}

#[derive(Clone, Debug)]
pub enum RenderAsset {
    Sprite2d {
        layers: Vec<LayerAsset2d>,
        /// Model images referenced by layer textures, shared via `Arc`.
        images: Vec<Arc<ModelImage>>,
    },
    Skeleton3d {
        vertices: Vec<SkinVertex>,
        indices: Vec<u32>,
        joint_count: usize,
        /// Model images referenced by the base color texture.
        images: Vec<Arc<ModelImage>>,
        /// Index into `images`; `None` keeps the untextured path.
        texture: Option<usize>,
        /// Basic material; `None` together with a texture selects the default
        /// PBR opaque material. An asset with no images, texture or material
        /// keeps the original vertex-color Lambert path.
        material: Option<Material3d>,
    },
}

#[derive(Clone, Debug)]
pub enum InstanceDraw {
    Sprite2d {
        asset: RenderAssetId,
        instance: RenderInstanceId,
        transform: Mat4,
        layers: Vec<Vec<[f32; 2]>>,
    },
    Skeleton3d {
        asset: RenderAssetId,
        instance: RenderInstanceId,
        transform: Mat4,
        skin_matrices: Vec<Mat4>,
    },
}

impl InstanceDraw {
    pub fn instance(&self) -> RenderInstanceId {
        match self {
            Self::Sprite2d { instance, .. } | Self::Skeleton3d { instance, .. } => *instance,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RenderFrame {
    pub instances: Vec<InstanceDraw>,
}

/// Why a scene frame was not submitted to the GPU.
///
/// Every variant names a frame that was skipped without becoming an error:
/// the surface could not provide a texture this instant, or the overlay had no
/// drawable size. Fatal conditions stay on the [`super::error::RenderError`]
/// path instead of being classified here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneSkipReason {
    /// The overlay surface is zero-sized; no frame work was attempted at all.
    ZeroSize,
    /// The acquired texture was suboptimal: it is dropped, the surface is
    /// reconfigured and the frame is skipped.
    Suboptimal,
    /// The surface acquire timed out.
    Timeout,
    /// The window is occluded.
    Occluded,
    /// The surface is outdated; it was reconfigured and the frame is skipped.
    Outdated,
    /// The surface was lost and recreated; the frame is skipped.
    Lost,
}

impl SceneSkipReason {
    /// Stable snake_case name for structured output.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ZeroSize => "zero_size",
            Self::Suboptimal => "suboptimal",
            Self::Timeout => "timeout",
            Self::Occluded => "occluded",
            Self::Outdated => "outdated",
            Self::Lost => "lost",
        }
    }
}

/// The result of one measured scene frame attempt: either the main scene was
/// submitted and presented, or it was skipped for a specific reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneFrameOutcome {
    /// The main scene command buffer was submitted and its texture presented.
    Submitted,
    /// No main scene submit/present happened this attempt.
    Skipped(SceneSkipReason),
}

/// CPU wall-clock durations of the stages of one measured scene frame.
///
/// These are host-side `Instant` measurements around the calls, not GPU
/// execution time: acquire may include driver waits/backpressure, and only
/// stages that actually executed have samples. A stage that did not run (or a
/// frame that was skipped before reaching it) stays `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneFrameTimings {
    /// Surface acquire, including any recovery/reconfiguration inside it.
    pub acquire: Option<Duration>,
    /// `SceneRenderer::render_to` plus encoder creation/finish.
    pub encode_upload: Option<Duration>,
    /// The main `queue.submit` call.
    pub submit: Option<Duration>,
    /// The `queue.present(texture)` call.
    pub present: Option<Duration>,
}

/// A measured scene frame: its outcome plus the subset of stages that ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeasuredSceneFrame {
    pub outcome: SceneFrameOutcome,
    pub timings: SceneFrameTimings,
}

/// Logical resource accounting of the scene renderer.
///
/// These are the sizes the renderer tracks, not actual VRAM usage: mask bytes
/// are the R8 mask texture bytes currently registered, and the capacity fields
/// are the current sizes of the grow-only GPU buffers (which never shrink).
#[derive(Clone, Debug, serde::Serialize)]
pub struct SceneResourceDiagnostics {
    /// Assets currently registered with the scene renderer.
    pub asset_count: usize,
    /// Logical mask texture bytes currently registered.
    pub mask_bytes: u64,
    /// Capacity of the grow-only 2D vertex buffer.
    pub sprite_vertex_capacity_bytes: u64,
    /// Capacity of the grow-only 2D instance-slot buffer.
    pub instance_capacity_bytes: u64,
    /// Capacity of the grow-only 3D palette-slot buffer.
    pub palette_capacity_bytes: u64,
    /// Model images currently registered (logical, not a VRAM measurement).
    pub texture_count: usize,
    /// Logical RGBA bytes of the currently registered model images.
    pub texture_bytes: u64,
    /// Monotonic count of successful model image texture creations. The fixed
    /// 1x1 white fallback does not count.
    pub texture_creations: u64,
    /// Monotonic count of successful static UV uploads, including those done
    /// as part of a 3D skin vertex upload.
    pub uv_uploads: u64,
}

/// Adapter/surface facts of the running [`super::graphics::Graphics`].
#[derive(Clone, Debug, serde::Serialize)]
pub struct GraphicsDiagnostics {
    /// Adapter name from `Adapter::get_info`.
    pub adapter: String,
    /// Backend name from `Adapter::get_info` (e.g. `dx12`).
    pub backend: String,
    /// Adapter device type from `Adapter::get_info` (e.g. `DiscreteGpu`),
    /// for software-adapter disclosure.
    pub device_type: String,
    /// Physical overlay surface size in pixels.
    pub physical_size: [u32; 2],
    /// Physical control-window surface size in pixels.
    pub control_physical_size: [u32; 2],
    /// Present mode configured for the overlay surface.
    pub present_mode: String,
    /// Alpha mode configured for the overlay surface.
    pub alpha_mode: String,
    /// Texture format configured for the overlay surface.
    pub format: String,
}

/// The vertical world window a scene is rendered with: the view spans
/// `[bottom, bottom + span]` in world Y and is centered horizontally on
/// `center_x`, with the horizontal span following the client aspect ratio.
///
/// The renderer, picking, dragging and the native hit region all consume the
/// same value so they can never disagree about where a world point lands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneView {
    pub span: f32,
    pub bottom: f32,
    /// World X at the horizontal center of the view: `0.0` for the bounded
    /// and primary-only scenes, the union-center offset otherwise.
    pub center_x: f32,
}

/// The frozen camera every bounded scene used before desktop-span mode: 4.5
/// world units from -0.9 to 3.6, unchanged in value and arithmetic.
pub const LEGACY: SceneView = SceneView {
    span: 4.5,
    bottom: -0.9,
    center_x: 0.0,
};

impl Default for SceneView {
    fn default() -> Self {
        LEGACY
    }
}

/// The scene view for a desktop-span scene covering the primary monitor.
///
/// Desktop-span mode keeps the vertical pixel-per-world-unit scale of a 640
/// logical pixel tall bounded scene at overlay `scale`: the world window
/// widens together with the physical height instead of enlarging characters.
/// The bottom stays at the legacy -0.9 so the visual origin does not move, and
/// the horizontal center stays at the legacy 0.0.
pub fn desktop_view(physical_height: u32, scale: f32) -> SceneView {
    let span = LEGACY.span * physical_height.max(1) as f32 / (640.0 * scale.max(f32::MIN_POSITIVE));
    SceneView {
        span,
        bottom: LEGACY.bottom,
        center_x: 0.0,
    }
}

/// The scene view for a desktop-span scene covering a monitor rectangle that
/// is not the primary monitor, offset from it inside a multi-monitor union.
///
/// `physical_height` and `scale` feed [`desktop_view`] for the vertical
/// window. `center_offset_px` is the horizontal distance from the primary
/// monitor's center to the rectangle's center, and `below_primary_px` is how
/// far the rectangle's bottom edge lies below the primary's bottom edge, both
/// in physical pixels (X right, Y down, Win32 desktop coordinates).
///
/// Both offsets are divided by the same pixel-per-world-unit factor, so the
/// rectangle stays anchored to the primary: a world point keeps its
/// primary-local position whether it is viewed through this rectangle or
/// through the primary alone. Callers pass offsets from a checked physical
/// layout, so both values are finite and consistent with the height.
pub fn desktop_view_at(
    physical_height: u32,
    scale: f32,
    center_offset_px: f32,
    below_primary_px: f32,
) -> SceneView {
    let span = desktop_view(physical_height, scale).span;
    let q = 640.0 * scale.max(f32::MIN_POSITIVE) / LEGACY.span;
    SceneView {
        span,
        bottom: LEGACY.bottom - below_primary_px / q,
        center_x: center_offset_px / q,
    }
}

pub fn scene_projection(width: u32, height: u32) -> Mat4 {
    scene_projection_with(width, height, LEGACY)
}

/// [`scene_projection`] under an explicit [`SceneView`]. With [`LEGACY`] this
/// is value-identical to the frozen function (the `bottom + span` sum rounds
/// to the same `3.6` f32, and adding the `0.0` center is exact).
pub fn scene_projection_with(width: u32, height: u32, view: SceneView) -> Mat4 {
    let aspect = width.max(1) as f32 / height.max(1) as f32;
    let half_height = view.span * 0.5;
    let half_width = half_height * aspect;
    let top = view.bottom + view.span;
    glam::camera::rh::proj::directx::orthographic(
        -half_width + view.center_x,
        half_width + view.center_x,
        view.bottom,
        top,
        0.1,
        100.0,
    ) * glam::camera::rh::view::look_at_mat4(
        glam::Vec3::new(0.0, 0.0, 10.0),
        glam::Vec3::ZERO,
        glam::Vec3::Y,
    )
}

pub fn screen_to_world(cursor: [f32; 2], size: [u32; 2]) -> [f32; 2] {
    screen_to_world_with(cursor, size, LEGACY)
}

/// [`screen_to_world`] under an explicit [`SceneView`]; with [`LEGACY`] the
/// arithmetic is bit-identical, so frozen tests and old call sites keep their
/// exact values.
pub fn screen_to_world_with(cursor: [f32; 2], size: [u32; 2], view: SceneView) -> [f32; 2] {
    let width = size[0].max(1) as f32;
    let height = size[1].max(1) as f32;
    let half_width = view.span * 0.5 * width / height;
    let top = view.bottom + view.span;
    [
        (cursor[0] / width * 2.0 - 1.0) * half_width + view.center_x,
        top - cursor[1] / height * view.span,
    ]
}

/// Map a scene-space point to physical window pixels, the exact inverse of
/// [`screen_to_world`] under the same fixed camera as [`scene_projection`], so
/// picking, outlines and rendering agree.
pub fn world_to_screen(point: [f32; 2], size: [u32; 2]) -> [f32; 2] {
    world_to_screen_with(point, size, LEGACY)
}

/// [`world_to_screen`] under an explicit [`SceneView`]; with [`LEGACY`] the
/// arithmetic is bit-identical, so outline and frozen-anchor tests keep their
/// exact values.
pub fn world_to_screen_with(point: [f32; 2], size: [u32; 2], view: SceneView) -> [f32; 2] {
    let width = size[0].max(1) as f32;
    let height = size[1].max(1) as f32;
    let half_width = view.span * 0.5 * width / height;
    let top = view.bottom + view.span;
    [
        ((point[0] - view.center_x) / half_width + 1.0) * 0.5 * width,
        (top - point[1]) / view.span * height,
    ]
}
