// Dedicated 2D pipeline: one draw per asset layer, premultiplied alpha output.
//
// Bindings:
//   group 0, binding 0: scene globals (view projection matrix)
//   group 1, binding 0: per-layer constants (color, mask mapping, flags)
//   group 1, binding 1: per-layer mask coverage texture (R8, 1x1 white when the
//                       layer has no mask; the flag then disables sampling)
//   group 1, binding 2: shared linear clamp sampler
//   group 1, binding 3: base color image (sRGB, linear premultiplied; the 1x1
//                       white fallback when the layer is untextured and the
//                       flag then disables sampling)
//   group 2, binding 0: per-draw instance model matrix (dynamic offset)
//
// The vertex buffers carry the deformed model-local vertices supplied by the
// simulation for this frame plus the static artist UVs (never rewritten while
// the asset is registered; untextured legacy layers carry generated zeros and
// never sample). The mask polygon was rasterized once at asset registration
// time in model-local coordinates and does not follow runtime deformation; the
// shader maps the deformed local point through the recorded padded bounds to a
// mask UV and discards coverage outside [0, 1].
//
// Color contract: the base image texels are sRGB-encoded, linear-premultiplied
// RGB with the original 8-bit alpha. Sampling the Rgba8UnormSrgb texture
// decodes to linear premultiplied RGB directly, so the tint is applied without
// multiplying the sampled RGB by the sampled alpha again:
//   rgb   = sample.rgb * tint.rgb * tint.a * coverage
//   alpha = sample.a   * tint.a * coverage
// Legacy untextured layers take exactly the old path: rgb = tint.rgb * alpha
// with alpha = tint.a * coverage.

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
@group(1) @binding(2) var linear_sampler: sampler;
@group(1) @binding(3) var base_texture: texture_2d<f32>;

struct Instance {
    model: mat4x4<f32>,
};

@group(2) @binding(0) var<uniform> instance: Instance;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) mask_uv: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
) -> VertexOutput {
    var out: VertexOutput;
    let world = instance.model * vec4<f32>(position, 0.0, 1.0);
    out.clip_position = globals.view_proj * world;
    let extent = vec2<f32>(max(layer.mask_extent.x, 1e-6), max(layer.mask_extent.y, 1e-6));
    out.mask_uv = (position - layer.mask_min) / extent;
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Coverage starts at the layer's own alpha and is only reduced further.
    var coverage = layer.color.a;
    if (layer.flags.x == 1u) {
        if (in.mask_uv.x < 0.0 || in.mask_uv.x > 1.0 || in.mask_uv.y < 0.0 || in.mask_uv.y > 1.0) {
            // Outside the recorded mask bounds the layer is fully transparent.
            coverage = 0.0;
        } else {
            // Linear sampling of the supersampled coverage gives edge AA.
            coverage = coverage * textureSample(mask_texture, linear_sampler, in.mask_uv).r;
        }
    }
    // The pipeline blends with PREMULTIPLIED_ALPHA_BLENDING.
    if (layer.flags.y == 1u) {
        // Textured: the sample is already linear premultiplied, so the tint is
        // applied directly and the RGB is not multiplied by the sample alpha
        // again. This keeps transparent edges free of dark bleed.
        let sample = textureSample(base_texture, linear_sampler, in.uv);
        let alpha = sample.a * coverage;
        return vec4<f32>(sample.rgb * layer.color.rgb * coverage, alpha);
    }
    // Untextured: exactly the original color-only math.
    return vec4<f32>(layer.color.rgb * coverage, coverage);
}
