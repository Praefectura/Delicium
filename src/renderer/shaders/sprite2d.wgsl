// Dedicated 2D pipeline: one draw per asset layer, premultiplied alpha output.
//
// Bindings:
//   group 0, binding 0: scene globals (view projection matrix)
//   group 1, binding 0: per-layer constants (color, mask mapping, mask flag)
//   group 1, binding 1: per-layer mask coverage texture (R8, 1x1 white when the
//                       layer has no mask; the flag then disables sampling)
//   group 1, binding 2: mask sampler (linear, clamp to edge)
//   group 2, binding 0: per-draw instance model matrix (dynamic offset)
//
// The vertex buffers carry the deformed model-local vertices supplied by the
// simulation for this frame. The mask polygon was rasterized once at asset
// registration time in model-local coordinates and does not follow runtime
// deformation; the shader maps the deformed local point through the recorded
// padded bounds to a mask UV and discards coverage outside [0, 1].

struct Globals {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct Layer {
    color: vec4<f32>,
    mask_min: vec2<f32>,
    mask_extent: vec2<f32>,
    flags: vec4<u32>,
};

@group(1) @binding(0) var<uniform> layer: Layer;
@group(1) @binding(1) var mask_texture: texture_2d<f32>;
@group(1) @binding(2) var mask_sampler: sampler;

struct Instance {
    model: mat4x4<f32>,
};

@group(2) @binding(0) var<uniform> instance: Instance;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) mask_uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) position: vec2<f32>) -> VertexOutput {
    var out: VertexOutput;
    let world = instance.model * vec4<f32>(position, 0.0, 1.0);
    out.clip_position = globals.view_proj * world;
    let extent = vec2<f32>(max(layer.mask_extent.x, 1e-6), max(layer.mask_extent.y, 1e-6));
    out.mask_uv = (position - layer.mask_min) / extent;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var alpha = layer.color.a;
    if (layer.flags.x == 1u) {
        if (in.mask_uv.x < 0.0 || in.mask_uv.x > 1.0 || in.mask_uv.y < 0.0 || in.mask_uv.y > 1.0) {
            // Outside the recorded mask bounds the layer is fully transparent.
            alpha = 0.0;
        } else {
            // Linear sampling of the supersampled coverage gives edge AA.
            alpha = alpha * textureSample(mask_texture, mask_sampler, in.mask_uv).r;
        }
    }
    // The pipeline blends with PREMULTIPLIED_ALPHA_BLENDING.
    return vec4<f32>(layer.color.rgb * alpha, alpha);
}
