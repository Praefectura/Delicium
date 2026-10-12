// Dedicated 3D pipeline with true GPU vertex skinning.
//
// Bindings:
//   group 0, binding 0: scene globals (view projection matrix)
//   group 1, binding 0: per-instance model matrix + joint matrix palette
//                       (dynamic uniform offset, one slot per drawn instance)
//
// The vertex buffer holds the static bind-pose mesh; the skinned position is
// computed on the GPU as the weighted sum of palette matrices selected by the
// per-vertex joint indices. Normals are transformed with the inverse-transpose
// of the blended skin matrix, computed with a determinant guard so singular or
// non-uniformly scaled matrices never produce NaN.

const MAX_JOINTS: u32 = 128u;

struct Globals {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct Palette {
    model: mat4x4<f32>,
    joints: array<mat4x4<f32>, 128>,
};

@group(1) @binding(0) var<uniform> palette: Palette;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) color: vec4<f32>,
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
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let normal = safe_normalize(in.world_normal);
    let light_dir = safe_normalize(vec3<f32>(0.35, 0.8, 0.45));
    let lambert = max(dot(normal, light_dir), 0.0);
    let intensity = 0.25 + 0.75 * lambert;
    let alpha = in.color.a;
    // The pipeline blends with PREMULTIPLIED_ALPHA_BLENDING.
    return vec4<f32>(in.color.rgb * intensity * alpha, alpha);
}
