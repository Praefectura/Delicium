// Dedicated 3D pipeline with true GPU vertex skinning.
//
// Bindings:
//   group 0, binding 0: scene globals (view projection matrix)
//   group 1, binding 0: per-instance model matrix + joint matrix palette
//                       (dynamic uniform offset, one slot per drawn instance)
//   group 2, binding 0: per-asset material constants
//   group 2, binding 1: base color image (sRGB, linear premultiplied; the 1x1
//                       white fallback when the asset declares no texture, and
//                       the flag then disables sampling)
//   group 2, binding 2: shared linear clamp sampler
//
// The vertex buffer holds the static bind-pose mesh; the skinned position is
// computed on the GPU as the weighted sum of palette matrices selected by the
// per-vertex joint indices. Normals are transformed with the inverse-transpose
// of the blended skin matrix, computed with a determinant guard so singular or
// non-uniformly scaled matrices never produce NaN. The UV is a static vertex
// attribute: it rides along with the static skin vertex upload and is never
// recomputed by a frame.
//
// Lighting contract:
//   - `flags.z == 0`: legacy path, output preserved exactly:
//       intensity = 0.25 + 0.75 * max(dot(n, l), 0)
//       result    = vec4(color.rgb * intensity * color.a, color.a)
//   - `flags.z == 1`: material path (GGX/Schlick, see `material_lighting`).
//     The image stores linear-premultiplied RGB with the original alpha, so
//     the straight base color is recovered in linear space by dividing by the
//     sample alpha (0 when the sample is fully transparent). Cutout compares
//     the effective alpha `texture.a * color.a` against the cutoff, discards
//     failing fragments before any depth write and outputs fully opaque
//     fragments otherwise; opaque material assets are already required by the
//     renderer preflight to be fully opaque, and also output alpha 1.

const MAX_JOINTS: u32 = 128u;
const PI: f32 = 3.141592653589793;
// Fixed directional light and basic brightness reused from the legacy path.
const LIGHT_DIR: vec3<f32> = vec3<f32>(0.35, 0.8, 0.45);
// The scene camera is orthographic looking down -Z, so the view direction is
// constant in world space.
const VIEW_DIR: vec3<f32> = vec3<f32>(0.0, 0.0, 1.0);
const AMBIENT_LEVEL: f32 = 0.25;
const DIRECT_SCALE: f32 = 0.75;
const DIELECTRIC_F0: f32 = 0.04;

struct Globals {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct Palette {
    model: mat4x4<f32>,
    joints: array<mat4x4<f32>, 128>,
};

@group(1) @binding(0) var<uniform> palette: Palette;

struct Material {
    // x: roughness, y: metallic, z: alpha cutoff (0 when not cutout).
    params: vec4<f32>,
    // x: has texture, y: cutout, z: use material (0 = legacy Lambert path).
    flags: vec4<u32>,
};

@group(2) @binding(0) var<uniform> material: Material;
@group(2) @binding(1) var base_texture: texture_2d<f32>;
@group(2) @binding(2) var linear_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
};

// Inverse-transpose of a 3x3 matrix with a determinant guard. Cross products
// are taken in column order so the returned matrix is already transposed.
fn normal_matrix(m: mat3x3<f32>) -> mat3x3<f32> {
    let a = m[0];
    let b = m[1];
    let c = m[2];
    let cross_bc = cross(b, c);
    let det = dot(a, cross_bc);
    if (abs(det) < 1e-8) {
        return mat3x3<f32>(
            vec3<f32>(1.0, 0.0, 0.0),
            vec3<f32>(0.0, 1.0, 0.0),
            vec3<f32>(0.0, 0.0, 1.0),
        );
    }
    let inv_det = 1.0 / det;
    return mat3x3<f32>(
        cross_bc * inv_det,
        cross(c, a) * inv_det,
        cross(a, b) * inv_det,
    );
}

fn safe_normalize(v: vec3<f32>) -> vec3<f32> {
    let len_sq = dot(v, v);
    if (len_sq < 1e-12) {
        return vec3<f32>(0.0, 0.0, 1.0);
    }
    return v * inverseSqrt(len_sq);
}

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) joints: vec4<u32>,
    @location(4) weights: vec4<f32>,
    @location(5) uv: vec2<f32>,
) -> VertexOutput {
    var out: VertexOutput;
    let total = (weights.x + weights.y) + (weights.z + weights.w);
    var skin = mat4x4<f32>(
        vec4<f32>(1.0, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, 1.0, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, 1.0, 0.0),
        vec4<f32>(0.0, 0.0, 0.0, 1.0),
    );
    if (total > 0.0) {
        let inv_total = 1.0 / total;
        skin = (weights.x * inv_total) * palette.joints[joints.x]
            + (weights.y * inv_total) * palette.joints[joints.y]
            + (weights.z * inv_total) * palette.joints[joints.z]
            + (weights.w * inv_total) * palette.joints[joints.w];
    }
    let world = palette.model * skin * vec4<f32>(position, 1.0);
    out.clip_position = globals.view_proj * world;

    let skin3 = mat3x3<f32>(skin[0].xyz, skin[1].xyz, skin[2].xyz);
    let model3 = mat3x3<f32>(palette.model[0].xyz, palette.model[1].xyz, palette.model[2].xyz);
    out.world_normal = normal_matrix(model3) * (normal_matrix(skin3) * normal);
    out.color = color;
    out.uv = uv;
    return out;
}

// Cook-Torrance GGX/Schlick for the fixed light and orthographic view.
//
//   l = normalize(0.35, 0.8, 0.45), v = (0, 0, 1), h = normalize(l + v)
//   ndl = max(dot(n, l), 0), ndv = max(dot(n, v), 1e-4)
//   ndh = max(dot(n, h), 0), vdh = max(dot(v, h), 0)
//   alpha = max(roughness * roughness, 1e-4), alpha2 = alpha * alpha
//   D = alpha2 / (PI * (ndh^2 * (alpha2 - 1) + 1)^2)
//   F = f0 + (1 - f0) * (1 - vdh)^5, f0 = mix(0.04, base, metallic)
//   k = alpha / 2
//   G = (ndl / (ndl * (1 - k) + k)) * (ndv / (ndv * (1 - k) + k))
//   spec   = D * G * F / max(4 * ndl * ndv, 1e-4)
//   kd     = (1 - F) * (1 - metallic)
//   direct = (kd * base / PI + spec) * ndl * 0.75
//   ambient = base * (1 - metallic) * 0.25
//   result = ambient + direct
fn material_lighting(base: vec3<f32>, normal: vec3<f32>, roughness: f32, metallic: f32) -> vec3<f32> {
    let n = normal;
    let l = safe_normalize(LIGHT_DIR);
    let v = VIEW_DIR;
    let h = safe_normalize(l + v);
    let ndl = max(dot(n, l), 0.0);
    let ndv = max(dot(n, v), 1e-4);
    // Clamped to [0, 1] so float overshoot can never reach pow() with a
    // negative base.
    let ndh = clamp(dot(n, h), 0.0, 1.0);
    let vdh = clamp(dot(v, h), 0.0, 1.0);

    let alpha = max(roughness * roughness, 1e-4);
    let alpha2 = alpha * alpha;
    // Avoid cancelling the small alpha2 against 1 at roughness zero.
    let ndh2 = ndh * ndh;
    let denom = (1.0 - ndh2) + ndh2 * alpha2;
    let d = alpha2 / (PI * denom * denom);

    let f0 = mix(vec3<f32>(DIELECTRIC_F0), base, vec3<f32>(metallic));
    let f = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vdh, 5.0);

    let k = alpha * 0.5;
    let gl = ndl / (ndl * (1.0 - k) + k);
    let gv = ndv / (ndv * (1.0 - k) + k);
    let g = gl * gv;

    let spec = (d * g * f) / max(4.0 * ndl * ndv, 1e-4);
    let kd = (vec3<f32>(1.0) - f) * (1.0 - metallic);
    let direct = (kd * base / PI + spec) * ndl * DIRECT_SCALE;
    let ambient = base * (1.0 - metallic) * AMBIENT_LEVEL;
    return ambient + direct;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let normal = safe_normalize(in.world_normal);
    if (material.flags.z == 0u) {
        // Legacy path: the original vertex-color Lambert output, unchanged.
        let light_dir = safe_normalize(LIGHT_DIR);
        let lambert = max(dot(normal, light_dir), 0.0);
        let intensity = 0.25 + 0.75 * lambert;
        let alpha = in.color.a;
        // The pipeline blends with PREMULTIPLIED_ALPHA_BLENDING.
        return vec4<f32>(in.color.rgb * intensity * alpha, alpha);
    }

    var base = in.color.rgb;
    var alpha = in.color.a;
    if (material.flags.x == 1u) {
        let sample = textureSample(base_texture, linear_sampler, in.uv);
        // The image holds linear-premultiplied RGB with the original alpha:
        // recover the straight base color in linear space. A fully transparent
        // sample has premultiplied RGB 0, so the guarded reciprocal yields 0
        // instead of inf/NaN.
        let inv_alpha = 1.0 / max(sample.a, 1e-6);
        base = sample.rgb * inv_alpha * in.color.rgb;
        alpha = sample.a * in.color.a;
    }
    if (material.flags.y == 1u) {
        // Cutout: compare the effective alpha against the cutoff, discard
        // failing fragments before any depth write, and pass surviving ones as
        // fully opaque so they neither blend nor fail to occlude.
        if (alpha < material.params.z) {
            discard;
        }
    }
    // Opaque material assets are enforced by the renderer preflight to be
    // fully opaque; both modes therefore output alpha 1 to avoid depth
    // occlusion artifacts from premultiplied blending.
    return vec4<f32>(material_lighting(base, normal, material.params.x, material.params.y), 1.0);
}
