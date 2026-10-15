//! Prototype resource budgets applied while loading a model.
//!
//! All limits are enforced *before* any GPU resource is created and are
//! intentionally adjustable: tests and the control UI may lower them to
//! exercise rejection paths. They are prototype budgets, not permanent
//! format limits.
//!
//! The counters marked `derived` bound quantities the compiler computes from
//! the raw description (grid meshes, warp control grids, merged animation
//! keys); they exist so that a tiny input file cannot expand into unbounded
//! work or memory.

/// Budget for a single model load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadLimits {
    /// Maximum accepted payload size in bytes, enforced on the bytes actually
    /// read from disk (not only on file metadata).
    pub max_file_bytes: usize,
    /// Maximum accumulated vertices across the whole model (authored vertices
    /// plus vertices generated from layer grids).
    pub max_vertices: usize,
    /// Maximum accumulated triangle indices across the whole model
    /// (authored indices plus indices generated from layer grids).
    pub max_indices: usize,
    /// Maximum number of joints.
    pub max_joints: usize,
    /// Maximum number of 2D layers.
    pub max_layers: usize,
    /// Maximum number of parameters.
    pub max_parameters: usize,
    /// Maximum number of 2D deformers (combined rotation and warp).
    pub max_deformers: usize,
    /// Maximum number of points in a single 2D layer mask (derived: the
    /// renderer turns this polygon into mask geometry).
    pub max_mask_points: usize,
    /// Maximum number of keys in a single animation channel.
    pub max_keys_per_channel: usize,
    /// Maximum number of channels in a single animation.
    pub max_channels_per_animation: usize,
    /// Maximum number of animations in a single model.
    pub max_animations: usize,
    /// Maximum accumulated keys across all channels of all animations
    /// (derived budget, checked with overflow-safe arithmetic).
    pub max_total_keys: usize,
    /// Maximum number of vertices along either axis of a generated 2D layer
    /// grid or of a warp control grid (both axes are `>= 2`).
    pub max_grid_dimension: u32,
    /// Maximum accumulated warp control offsets across all warp deformers
    /// (derived budget).
    pub max_total_warp_offsets: usize,
    /// Maximum number of embedded images per model (`0.2` documents).
    pub max_images: usize,
    /// Maximum width and height of one decoded image, in pixels.
    pub max_image_dimension: u32,
    /// Maximum decoded RGBA8 bytes of a single image.
    pub max_image_bytes: usize,
    /// Maximum accumulated decoded RGBA8 bytes across all images of one model
    /// (derived budget, checked before each output allocation).
    pub max_total_image_bytes: usize,
    /// Internal allocation limit handed to the `png` decoder for one image;
    /// the library limit does not cover the caller's output buffer, so image
    /// dimensions and decoded bytes are checked explicitly as well.
    pub max_png_decoder_bytes: usize,
}

impl Default for LoadLimits {
    fn default() -> Self {
        // Measured in the same units the compiler produces: raw grid layers
        // and warp grids expand into these counts.
        Self {
            max_file_bytes: 8 * 1024 * 1024,
            max_vertices: 50_000,
            max_indices: 150_000,
            max_joints: 128,
            max_layers: 64,
            max_parameters: 64,
            max_deformers: 128,
            max_mask_points: 64,
            max_keys_per_channel: 4_096,
            max_channels_per_animation: 1_024,
            max_animations: 32,
            max_total_keys: 262_144,
            max_grid_dimension: 64,
            max_total_warp_offsets: 16_384,
            max_images: 8,
            // 2048x2048x4 == 16 MiB, so the dimension and byte budgets meet
            // exactly; the 8 MiB payload budget may bind first for
            // poorly-compressed images.
            max_image_dimension: 2_048,
            max_image_bytes: 16 * 1024 * 1024,
            max_total_image_bytes: 32 * 1024 * 1024,
            max_png_decoder_bytes: 16 * 1024 * 1024,
        }
    }
}

impl LoadLimits {
    /// A set of limits suitable for tests: everything is kept but the payload
    /// budget is small enough to be reached with a tiny file.
    pub fn tiny_for_tests() -> Self {
        Self {
            max_file_bytes: 4 * 1024,
            ..Self::default()
        }
    }
}
