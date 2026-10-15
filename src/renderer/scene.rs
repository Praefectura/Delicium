//! Offscreen scene renderer.
//!
//! [`SceneRenderer`] draws one [`RenderFrame`] into a caller-provided texture
//! view. 3D skinned instances go through a depth-tested pass first; 2D sprite
//! instances then draw in input order in a second pass that loads the color
//! buffer, so 2D layers composite over the 3D result while 3D instances keep
//! depth sorting among themselves.
//!
//! Everything per-frame (deformed 2D vertices, model matrices, joint palettes)
//! is uploaded with [`wgpu::Queue::write_buffer`] into grow-only buffers. No
//! GPU buffer is created per frame; buffers grow by doubling (with a copy of
//! the old contents) only when a frame or asset needs more room.
//!
//! 2D masks are rasterized once at registration time in model-local
//! coordinates (see [`super::mask`]) and do not follow runtime deformation: the
//! shader maps the deformed local point through the recorded padded bounds.
//!
//! Base color images and the static artist UVs are also uploaded exactly once,
//! at registration time: a per-asset UV vertex buffer for 2D layers and the
//! 80-byte skinned vertex stride for 3D. Dynamic frames only update deformed 2D
//! positions and instance/palette uniforms; no texture is decoded or uploaded
//! per frame. Model images are shared with the CPU side through
//! [`std::sync::Arc`] and never copied wholesale into renderer state.
//!
//! The renderer is built for one target [`wgpu::TextureFormat`] and any target
//! view of that format; the scene projection comes from
//! [`scene_projection`] for the size passed to [`SceneRenderer::render_to`].

use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU64,
    sync::Arc,
};

use super::{
    error::RenderError,
    mask::{MASK_SIZE, rasterize_mask},
    types::{
        InstanceDraw, LEGACY, LayerAsset2d, RenderAsset, RenderFrame, SceneResourceDiagnostics,
        SceneView, SkinVertex, scene_projection_with,
    },
};
use crate::model::{AlphaMode, Material3d, ModelImage};

/// Joint slots in the per-instance palette uniform.
const MAX_JOINTS: usize = 128;

/// Palette uniform size: model matrix plus the full joint array.
const PALETTE_BYTES: u64 = 64 + (MAX_JOINTS as u64) * 64;

/// Per-instance model matrix size (2D pipeline dynamic uniform).
const INSTANCE_BYTES: u64 = 64;

/// Layer constants uniform size (color, mask bounds, flags: mask / texture).
const LAYER_BYTES: u64 = 48;

/// Per-asset 3D material uniform size (params + flags).
const MATERIAL_BYTES: u64 = 32;

/// Bound on instances processed per frame, protecting dynamic buffers.
const MAX_FRAME_INSTANCES: usize = 4096;

/// Bound on total dynamic upload bytes per frame.
const MAX_FRAME_DYNAMIC_BYTES: u64 = 64 << 20;

/// Bound per 2D layer: vertices and indices.
const MAX_LAYER_VERTICES: usize = 1 << 16;
const MAX_LAYER_INDICES: usize = 1 << 18;
const MAX_LAYERS_PER_ASSET: usize = 64;

/// Bound per 3D asset: vertices and indices.
const MAX_SKELETON_VERTICES: usize = 1 << 18;
const MAX_SKELETON_INDICES: usize = 1 << 20;

/// Total mask texture budget across all registered sprites.
const MAX_MASK_BYTES: u64 = 16 << 20;

/// Bytes of one mask texture (R8, `MASK_SIZE` squared).
const MASK_BYTES: u64 = (MASK_SIZE * MASK_SIZE) as u64;

/// Model image budgets, checked before any GPU resource is created.
const MAX_IMAGES_PER_ASSET: usize = 8;
const MAX_IMAGE_DIM: u32 = 2048;
const MAX_IMAGE_BYTES: u64 = 16 << 20;
const MAX_ASSET_IMAGE_BYTES: u64 = 32 << 20;

/// Logical model-image pool across all registered assets. A replacement is
/// checked against the currently registered bytes plus the candidate, because
/// the old entry stays live until the candidate has passed the GPU scopes.
const MAX_REGISTERED_IMAGE_BYTES: u64 = 128 << 20;

/// Static stride of [`SkinVertex`] in the vertex buffer.
const SKIN_VERTEX_BYTES: u64 = 80;

const _: () = assert!(std::mem::size_of::<SkinVertex>() as u64 == SKIN_VERTEX_BYTES);
const _: () = assert!(std::mem::offset_of!(SkinVertex, uv) == 72);
const _: () = assert!(std::mem::size_of::<MaterialUniform>() as u64 == MATERIAL_BYTES);

/// Per-layer constants, laid out exactly like the WGSL `Layer` struct.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerUniform {
    color: [f32; 4],
    mask_min: [f32; 2],
    mask_extent: [f32; 2],
    flags: [u32; 4],
}

/// The offscreen renderer: pipelines, shared buffers and the asset store.
pub struct SceneRenderer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,

    layer_layout: wgpu::BindGroupLayout,
    palette_layout: wgpu::BindGroupLayout,
    instance_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,

    sprite_pipeline: wgpu::RenderPipeline,
    skeleton_pipeline: wgpu::RenderPipeline,

    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,

    /// Fixed 1x1 white fallback for mask and base color bindings when the
    /// asset supplies none. Not a model image: it never counts toward the
    /// model image budgets and no counter observes it.
    white_view: wgpu::TextureView,
    /// Shared linear clamp sampler for masks and base color images.
    linear_sampler: wgpu::Sampler,

    sprite_vertices: GrowBuffer,
    instance_slots: GrowBuffer,
    instance_bind_group: wgpu::BindGroup,
    palette_slots: GrowBuffer,
    palette_bind_group: wgpu::BindGroup,

    /// Alignment required for dynamic uniform offsets.
    uniform_alignment: u64,
    /// Per-slot stride in the 2D instance buffer.
    instance_stride: u64,
    /// Per-slot stride in the 3D palette buffer.
    palette_stride: u64,

    /// Mask texture bytes currently registered.
    mask_bytes: u64,
    /// Number of model images currently registered across all assets.
    registered_image_count: usize,
    /// Logical RGBA bytes of the model images currently registered.
    image_bytes_total: u64,
    /// Monotonic count of committed model image texture creations.
    texture_creations: u64,
    /// Monotonic count of committed static UV uploads.
    uv_uploads: u64,
    /// Reused staging buffer for joint palette uploads.
    scratch: Vec<u8>,

    assets: HashMap<u64, AssetEntry>,
    depth: Option<DepthTarget>,
}

struct DepthTarget {
    width: u32,
    height: u32,
    view: wgpu::TextureView,
}

enum AssetEntry {
    Sprite(SpriteAsset),
    Skeleton(SkeletonAsset),
}

impl AssetEntry {
    fn mask_bytes(&self) -> u64 {
        match self {
            Self::Sprite(sprite) => sprite.mask_bytes,
            Self::Skeleton(_) => 0,
        }
    }

    fn image_bytes(&self) -> u64 {
        match self {
            Self::Sprite(sprite) => sprite.image_bytes,
            Self::Skeleton(skeleton) => skeleton.image_bytes,
        }
    }

    fn image_count(&self) -> usize {
        match self {
            Self::Sprite(sprite) => sprite.images.len(),
            Self::Skeleton(skeleton) => skeleton.images.len(),
        }
    }

    /// Model image textures created for this asset: one per image, shared by
    /// every referencing layer or the material, so it equals the image count.
    fn texture_creations(&self) -> u64 {
        self.image_count() as u64
    }

    /// Static UV uploads performed for this asset: the 2D UV buffer is one
    /// concatenated upload per asset and the 3D UVs ride along with the single
    /// skin vertex upload, so every asset performs exactly one.
    fn uv_uploads(&self) -> u64 {
        1
    }
}

struct SpriteAsset {
    layers: Vec<LayerGpu>,
    index_buffer: wgpu::Buffer,
    /// Static artist UVs of every layer, concatenated in layer order and
    /// uploaded exactly once at registration (zeros for legacy untextured
    /// layers; never rewritten by a frame).
    uv_buffer: wgpu::Buffer,
    mask_bytes: u64,
    image_bytes: u64,
    /// One view per model image, shared by every referencing layer bind group;
    /// each view keeps its texture alive exactly as long as the asset is
    /// registered. There is no cross-asset texture cache.
    images: Vec<wgpu::TextureView>,
}

struct LayerGpu {
    bind_group: wgpu::BindGroup,
    vertex_count: u32,
    /// Byte offset of this layer's static UVs inside [`SpriteAsset::uv_buffer`].
    uv_offset: u64,
    index_offset: u64,
    index_count: u32,
}

struct SkeletonAsset {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    joint_count: usize,
    /// Per-asset material bind group (uniform + base color texture + sampler).
    material_bind_group: wgpu::BindGroup,
    image_bytes: u64,
    /// The referenced base color image view, kept for the same ownership
    /// reason as [`SpriteAsset::images`].
    images: Vec<wgpu::TextureView>,
}

/// A grow-only GPU buffer with a compiled-in usage set.
struct GrowBuffer {
    label: &'static str,
    usage: wgpu::BufferUsages,
    buffer: Option<wgpu::Buffer>,
    capacity: u64,
}

impl GrowBuffer {
    fn new(label: &'static str, usage: wgpu::BufferUsages) -> Self {
        Self {
            label,
            usage,
            buffer: None,
            capacity: 0,
        }
    }

    /// Makes sure the buffer can hold `required` bytes, copying old contents
    /// when the buffer must grow. Returns `true` when a new buffer was
    /// created (callers holding bind groups must then recreate them).
    fn ensure(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, required: u64) -> bool {
        if self.buffer.is_some() && self.capacity >= required {
            return false;
        }
        let required = required.max(4);
        let new_capacity = self.capacity.saturating_mul(2).max(required);
        let new_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size: new_capacity,
            usage: self.usage | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        if let Some(old) = &self.buffer {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Delicium buffer growth copy"),
            });
            encoder.copy_buffer_to_buffer(old, 0, &new_buffer, 0, self.capacity);
            queue.submit([encoder.finish()]);
        }
        self.buffer = Some(new_buffer);
        self.capacity = new_capacity;
        true
    }

    fn require(&self) -> Result<&wgpu::Buffer, RenderError> {
        self.buffer.as_ref().ok_or(RenderError::Internal {
            details: format!("{} buffer has not been created yet", self.label),
        })
    }
}

/// One frame-validated 3D draw with its asset buffers.
struct SkeletonDraw {
    dynamic_offset: u32,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    material_bind_group: wgpu::BindGroup,
}

/// One frame-validated 2D layer draw.
struct SpriteDraw {
    bind_group: wgpu::BindGroup,
    index_buffer: wgpu::Buffer,
    model_offset: u32,
    vertex_offset: u64,
    vertex_count: u32,
    /// Static UV slice of this layer inside the asset's UV buffer.
    uv_buffer: wgpu::Buffer,
    uv_offset: u64,
    index_offset: u64,
    index_count: u32,
}

/// CPU-side result of validating one asset before any GPU work.
enum PreparedAsset {
    Sprite(PreparedSprite),
    Skeleton(PreparedSkeleton),
}

impl PreparedAsset {
    /// Logical RGBA bytes of the candidate's model images.
    fn image_bytes(&self) -> u64 {
        match self {
            Self::Sprite(sprite) => sprite.image_bytes,
            Self::Skeleton(skeleton) => skeleton.image_bytes,
        }
    }
}

struct PreparedSprite {
    layers: Vec<PreparedLayer>,
    /// Validated model images shared with the caller via `Arc`.
    images: Vec<Arc<ModelImage>>,
    /// Static artist UVs of every layer concatenated in layer order, one
    /// `[f32; 2]` per vertex (zeros for legacy untextured layers).
    uvs: Vec<[f32; 2]>,
    image_bytes: u64,
}

struct PreparedLayer {
    uniform: LayerUniform,
    vertex_count: u32,
    /// Validated image index used to select the layer's base color view; the
    /// uniform `flags.y` stays the authoritative "samples a texture" bit.
    texture: Option<usize>,
    indices: Vec<u32>,
    mask: Option<super::mask::MaskRaster>,
}

struct PreparedSkeleton {
    vertices: Vec<SkinVertex>,
    indices: Vec<u32>,
    joint_count: usize,
    images: Vec<Arc<ModelImage>>,
    texture: Option<usize>,
    /// Legacy assets keep `None`, which selects the original Lambert path.
    material: Option<MaterialUniform>,
    image_bytes: u64,
}

/// Per-asset material constants, laid out exactly like the WGSL `Material`.
///
/// `flags = [textured, cutout, use_material, 0]`: `use_material == 0` selects
/// the legacy Lambert path and makes the other lanes moot.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialUniform {
    /// `[roughness, metallic, alpha_cutoff, 0]`.
    params: [f32; 4],
    flags: [u32; 4],
}

impl MaterialUniform {
    fn legacy() -> Self {
        Self {
            params: [0.0; 4],
            flags: [0; 4],
        }
    }

    fn new(material: &Material3d, texture: Option<usize>) -> Self {
        let cutout = matches!(material.alpha_mode, AlphaMode::Cutout);
        Self {
            params: [
                material.roughness,
                material.metallic,
                if cutout { material.alpha_cutoff } else { 0.0 },
                0.0,
            ],
            flags: [texture.is_some().into(), cutout.into(), 1, 0],
        }
    }
}

impl SceneRenderer {
    /// Creates the pipelines and shared resources for one target format.
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        format: wgpu::TextureFormat,
    ) -> Result<Self, RenderError> {
        let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment).max(1);
        let instance_stride = align_up(INSTANCE_BYTES, alignment);
        let palette_stride = align_up(PALETTE_BYTES, alignment);

        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Delicium scene globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(64),
                },
                count: None,
            }],
        });
        let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Delicium layer layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(LAYER_BYTES),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Delicium material layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(MATERIAL_BYTES),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let palette_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Delicium palette layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(PALETTE_BYTES),
                },
                count: None,
            }],
        });
        let instance_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Delicium instance layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(INSTANCE_BYTES),
                },
                count: None,
            }],
        });

        let sprite_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Delicium sprite2d shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "shaders/sprite2d.wgsl"
            ))),
        });
        let skeleton_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Delicium skinned3d shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "shaders/skinned3d.wgsl"
            ))),
        });

        let sprite_attributes: [wgpu::VertexAttribute; 1] =
            wgpu::vertex_attr_array![0 => Float32x2];
        let sprite_uv_attributes: [wgpu::VertexAttribute; 1] =
            wgpu::vertex_attr_array![1 => Float32x2];
        let skeleton_attributes: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x3,
            2 => Float32x4,
            3 => Uint32x4,
            4 => Float32x4,
            5 => Float32x2
        ];

        let sprite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Delicium sprite2d layout"),
                bind_group_layouts: &[
                    Some(&globals_layout),
                    Some(&layer_layout),
                    Some(&instance_layout),
                ],
                immediate_size: 0,
            });
        let sprite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Delicium sprite2d pipeline"),
            layout: Some(&sprite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &sprite_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &sprite_attributes,
                    }),
                    // Static artist UVs, one upload per asset at registration.
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &sprite_uv_attributes,
                    }),
                ],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &sprite_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let skeleton_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Delicium skinned3d layout"),
                bind_group_layouts: &[
                    Some(&globals_layout),
                    Some(&palette_layout),
                    Some(&material_layout),
                ],
                immediate_size: 0,
            });
        let skeleton_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Delicium skinned3d pipeline"),
            layout: Some(&skeleton_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &skeleton_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: SKIN_VERTEX_BYTES,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &skeleton_attributes,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &skeleton_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Delicium scene globals"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Delicium scene globals bind group"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        // Fixed 1x1 white fallback for the mask, base color and material
        // texture bindings. It is a constant renderer resource: it is created
        // once, never counted as a registered model image, and its bytes stay
        // outside the model image budget.
        let white_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Delicium white fallback"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255u8, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let white_view = white_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Delicium linear clamp sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let mut sprite_vertices =
            GrowBuffer::new("Delicium 2D vertices", wgpu::BufferUsages::VERTEX);
        let mut instance_slots =
            GrowBuffer::new("Delicium 2D instance slots", wgpu::BufferUsages::UNIFORM);
        let mut palette_slots =
            GrowBuffer::new("Delicium 3D palette slots", wgpu::BufferUsages::UNIFORM);
        sprite_vertices.ensure(&device, &queue, 8);
        instance_slots.ensure(&device, &queue, instance_stride);
        palette_slots.ensure(&device, &queue, palette_stride);

        let instance_bind_group = create_dynamic_bind_group(
            &device,
            &instance_layout,
            "Delicium 2D instance bind group",
            instance_slots.require()?,
            INSTANCE_BYTES,
        );
        let palette_bind_group = create_dynamic_bind_group(
            &device,
            &palette_layout,
            "Delicium 3D palette bind group",
            palette_slots.require()?,
            PALETTE_BYTES,
        );

        let oom_error = pollster::block_on(oom.pop());
        let validation_error = pollster::block_on(scope.pop());
        if let Some(error) = oom_error {
            return Err(RenderError::OutOfMemory {
                details: error.to_string(),
            });
        }
        if let Some(error) = validation_error {
            return Err(RenderError::GpuResource {
                details: format!("scene renderer creation failed: {error}"),
            });
        }

        Ok(Self {
            device,
            queue,
            layer_layout,
            palette_layout,
            instance_layout,
            material_layout,
            sprite_pipeline,
            skeleton_pipeline,
            globals_buffer,
            globals_bind_group,
            white_view,
            linear_sampler,
            sprite_vertices,
            instance_slots,
            instance_bind_group,
            palette_slots,
            palette_bind_group,
            uniform_alignment: alignment,
            instance_stride,
            palette_stride,
            mask_bytes: 0,
            registered_image_count: 0,
            image_bytes_total: 0,
            texture_creations: 0,
            uv_uploads: 0,
            scratch: Vec::new(),
            assets: HashMap::new(),
            depth: None,
        })
    }

    /// Registers (or replaces) the GPU resources of one asset.
    ///
    /// All fallible CPU validation and all GPU resource creation happen before
    /// the asset store is touched; the GPU work runs inside error scopes, so a
    /// rejected asset never clobbers a previously registered one under the
    /// same id. Every image, UV, material and mask budget is checked before
    /// the first GPU resource is created, and a replacement keeps the old
    /// entry live (including its logical image bytes) until the candidate has
    /// passed both error scopes.
    pub fn register_asset(&mut self, id: u64, asset: &RenderAsset) -> Result<(), RenderError> {
        let prepared = match asset {
            RenderAsset::Sprite2d { layers, images } => self.prepare_sprite(layers, images),
            RenderAsset::Skeleton3d {
                vertices,
                indices,
                joint_count,
                images,
                texture,
                material,
            } => prepare_skeleton(
                &self.device,
                vertices,
                indices,
                *joint_count,
                images,
                *texture,
                material,
            ),
        }
        .map_err(|details| RenderError::InvalidAsset { asset: id, details })?;

        let new_mask_bytes = match &prepared {
            PreparedAsset::Sprite(sprite) => {
                sprite
                    .layers
                    .iter()
                    .filter(|layer| layer.mask.is_some())
                    .count() as u64
                    * MASK_BYTES
            }
            PreparedAsset::Skeleton(_) => 0,
        };
        let new_image_bytes = prepared.image_bytes();

        // Discount only the old entry's mask contribution for the mask budget
        // (existing mask semantics are unchanged). The model image pool is
        // stricter: every currently registered image byte still counts,
        // including the old entry of the very same id, because it stays live
        // until the replacement has passed both GPU error scopes.
        let old_mask_bytes = self.assets.get(&id).map_or(0, AssetEntry::mask_bytes);
        let old_image_bytes = self.assets.get(&id).map_or(0, AssetEntry::image_bytes);
        let old_image_count = self.assets.get(&id).map_or(0, AssetEntry::image_count);
        let retained_mask_bytes = self.mask_bytes - old_mask_bytes;
        if retained_mask_bytes + new_mask_bytes > MAX_MASK_BYTES {
            return Err(RenderError::InvalidAsset {
                asset: id,
                details: format!(
                    "mask budget exceeded: {new_mask_bytes} new bytes plus {retained_mask_bytes} already registered exceeds {MAX_MASK_BYTES}",
                ),
            });
        }
        let pool_during_replacement = self.image_bytes_total + new_image_bytes;
        if pool_during_replacement > MAX_REGISTERED_IMAGE_BYTES {
            return Err(RenderError::InvalidAsset {
                asset: id,
                details: format!(
                    "model image budget exceeded: {new_image_bytes} candidate bytes plus {} registered bytes exceeds {MAX_REGISTERED_IMAGE_BYTES}",
                    self.image_bytes_total,
                ),
            });
        }

        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let outcome = self.create_asset_resources(id, prepared);
        let oom_error = pollster::block_on(oom.pop());
        let validation_error = pollster::block_on(scope.pop());

        if let Some(error) = oom_error {
            return Err(RenderError::OutOfMemory {
                details: error.to_string(),
            });
        }
        if let Some(error) = validation_error {
            return Err(RenderError::GpuResource {
                details: format!("asset {id} resource creation failed: {error}"),
            });
        }
        let entry = outcome?;
        // Every counter below is updated only after the whole candidate
        // committed: a failed replacement leaves the old entry, the counters
        // and the logical budgets exactly as they were. A committed
        // replacement retires the old entry's bytes exactly once.
        self.mask_bytes = retained_mask_bytes + entry.mask_bytes();
        self.image_bytes_total = self.image_bytes_total - old_image_bytes + entry.image_bytes();
        self.registered_image_count =
            self.registered_image_count - old_image_count + entry.image_count();
        self.texture_creations += entry.texture_creations();
        self.uv_uploads += entry.uv_uploads();
        self.assets.insert(id, entry);
        Ok(())
    }

    /// Removes an asset; its dedicated buffers, textures and bind groups are
    /// dropped, including the sprite's static index and UV buffers. Its
    /// logical mask and model image bytes return to the budgets; the monotonic
    /// creation counters are not rewound.
    pub fn remove_asset(&mut self, id: u64) {
        if let Some(entry) = self.assets.remove(&id) {
            self.mask_bytes -= entry.mask_bytes();
            self.image_bytes_total -= entry.image_bytes();
            self.registered_image_count -= entry.image_count();
        }
    }

    /// Draws one frame into `target` (which must be `size` large and use the
    /// format given to [`SceneRenderer::new`]).
    ///
    /// A zero-sized target is skipped without touching the encoder. Any frame
    /// data that violates the registered assets is rejected as
    /// [`RenderError::InvalidFrame`]; nothing is drawn in that case.
    pub fn render_to(
        &mut self,
        frame: &RenderFrame,
        target: &wgpu::TextureView,
        size: [u32; 2],
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), RenderError> {
        self.render_to_with_view(frame, target, size, LEGACY, encoder)
    }

    /// [`SceneRenderer::render_to`] under an explicit scene view; the view
    /// decides the projection uploaded for this frame.
    pub fn render_to_with_view(
        &mut self,
        frame: &RenderFrame,
        target: &wgpu::TextureView,
        size: [u32; 2],
        view: SceneView,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), RenderError> {
        if size[0] == 0 || size[1] == 0 {
            return Ok(());
        }
        if frame.instances.len() > MAX_FRAME_INSTANCES {
            return Err(RenderError::InvalidFrame {
                instance: 0,
                details: format!(
                    "frame has {} instances, the limit is {MAX_FRAME_INSTANCES}",
                    frame.instances.len()
                ),
            });
        }

        // Validate the frame against the registered assets before any GPU
        // work: unknown assets, kind mismatches, wrong layer or matrix counts
        // and non-finite values are all rejected here.
        let mut skeleton_total = 0usize;
        let mut sprite_total = 0usize;
        let mut sprite_vertex_bytes = 0u64;
        for draw in &frame.instances {
            match draw {
                InstanceDraw::Skeleton3d {
                    asset,
                    instance,
                    transform,
                    skin_matrices,
                } => {
                    skeleton_total += 1;
                    let Some(AssetEntry::Skeleton(skeleton)) = self.assets.get(asset) else {
                        return Err(unknown_or_mismatched(
                            &self.assets,
                            *asset,
                            *instance,
                            "Skeleton3d",
                        ));
                    };
                    if skin_matrices.len() != skeleton.joint_count {
                        return Err(RenderError::InvalidFrame {
                            instance: *instance,
                            details: format!(
                                "asset {asset} has {} joints but the frame supplies {} skin matrices",
                                skeleton.joint_count,
                                skin_matrices.len()
                            ),
                        });
                    }
                    if !transform.is_finite() {
                        return Err(RenderError::InvalidFrame {
                            instance: *instance,
                            details: "instance transform is not finite".into(),
                        });
                    }
                    for (index, matrix) in skin_matrices.iter().enumerate() {
                        if !matrix.is_finite() {
                            return Err(RenderError::InvalidFrame {
                                instance: *instance,
                                details: format!("skin matrix {index} is not finite"),
                            });
                        }
                    }
                }
                InstanceDraw::Sprite2d {
                    asset,
                    instance,
                    transform,
                    layers,
                } => {
                    sprite_total += 1;
                    let Some(AssetEntry::Sprite(sprite)) = self.assets.get(asset) else {
                        return Err(unknown_or_mismatched(
                            &self.assets,
                            *asset,
                            *instance,
                            "Sprite2d",
                        ));
                    };
                    if layers.len() != sprite.layers.len() {
                        return Err(RenderError::InvalidFrame {
                            instance: *instance,
                            details: format!(
                                "asset {asset} has {} layers but the frame supplies {}",
                                sprite.layers.len(),
                                layers.len()
                            ),
                        });
                    }
                    if !transform.is_finite() {
                        return Err(RenderError::InvalidFrame {
                            instance: *instance,
                            details: "instance transform is not finite".into(),
                        });
                    }
                    for (layer_index, vertices) in layers.iter().enumerate() {
                        let layer = &sprite.layers[layer_index];
                        if vertices.len() != layer.vertex_count as usize {
                            return Err(RenderError::InvalidFrame {
                                instance: *instance,
                                details: format!(
                                    "layer {layer_index} of asset {asset} has {} vertices, the frame supplies {}",
                                    layer.vertex_count,
                                    vertices.len()
                                ),
                            });
                        }
                        for (vertex_index, vertex) in vertices.iter().enumerate() {
                            if !vertex[0].is_finite() || !vertex[1].is_finite() {
                                return Err(RenderError::InvalidFrame {
                                    instance: *instance,
                                    details: format!(
                                        "layer {layer_index} vertex {vertex_index} is not finite"
                                    ),
                                });
                            }
                        }
                        sprite_vertex_bytes += (vertices.len() as u64) * 8;
                    }
                }
            }
        }

        let palette_bytes = (skeleton_total as u64) * self.palette_stride;
        let instance_bytes = (sprite_total as u64) * self.instance_stride;
        let dynamic_bytes = palette_bytes + instance_bytes + sprite_vertex_bytes;
        if dynamic_bytes > MAX_FRAME_DYNAMIC_BYTES {
            return Err(RenderError::InvalidFrame {
                instance: 0,
                details: format!(
                    "frame needs {dynamic_bytes} dynamic upload bytes, the limit is {MAX_FRAME_DYNAMIC_BYTES}"
                ),
            });
        }

        let device = self.device.clone();
        let queue = self.queue.clone();

        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);

        let projection = scene_projection_with(size[0], size[1], view);
        queue.write_buffer(
            &self.globals_buffer,
            0,
            bytemuck::cast_slice(&projection.to_cols_array()),
        );

        // ---- 3D palettes ---------------------------------------------------
        let mut skeleton_draws: Vec<SkeletonDraw> = Vec::with_capacity(skeleton_total);
        if skeleton_total > 0 {
            if self.palette_slots.ensure(&device, &queue, palette_bytes) {
                self.palette_bind_group = create_dynamic_bind_group(
                    &device,
                    &self.palette_layout,
                    "Delicium 3D palette bind group",
                    self.palette_slots.require()?,
                    PALETTE_BYTES,
                );
            }
            let stride = self.palette_stride as usize;
            let mut scratch = std::mem::take(&mut self.scratch);
            scratch.resize(stride, 0);
            let used = PALETTE_BYTES as usize;
            let mut slot = 0u32;
            for draw in &frame.instances {
                let InstanceDraw::Skeleton3d {
                    asset,
                    transform,
                    skin_matrices,
                    ..
                } = draw
                else {
                    continue;
                };
                let Some(AssetEntry::Skeleton(skeleton)) = self.assets.get(asset) else {
                    continue; // validated above
                };
                scratch[..64].copy_from_slice(bytemuck::cast_slice(&transform.to_cols_array()));
                for (index, matrix) in skin_matrices.iter().enumerate() {
                    let start = 64 + index * 64;
                    scratch[start..start + 64]
                        .copy_from_slice(bytemuck::cast_slice(&matrix.to_cols_array()));
                }
                let offset = u64::from(slot) * self.palette_stride;
                queue.write_buffer(self.palette_slots.require()?, offset, &scratch[..used]);
                skeleton_draws.push(SkeletonDraw {
                    dynamic_offset: offset as u32,
                    vertex_buffer: skeleton.vertex_buffer.clone(),
                    index_buffer: skeleton.index_buffer.clone(),
                    index_count: skeleton.index_count,
                    material_bind_group: skeleton.material_bind_group.clone(),
                });
                slot += 1;
            }
            self.scratch = scratch;
        }

        // ---- 2D vertices and model matrices --------------------------------
        let mut sprite_draws: Vec<SpriteDraw> = Vec::new();
        if sprite_total > 0 {
            self.sprite_vertices
                .ensure(&device, &queue, sprite_vertex_bytes);
            if self.instance_slots.ensure(&device, &queue, instance_bytes) {
                self.instance_bind_group = create_dynamic_bind_group(
                    &device,
                    &self.instance_layout,
                    "Delicium 2D instance bind group",
                    self.instance_slots.require()?,
                    INSTANCE_BYTES,
                );
            }
            let mut vertex_cursor: u64 = 0;
            let mut slot = 0u32;
            for draw in &frame.instances {
                let InstanceDraw::Sprite2d {
                    asset,
                    transform,
                    layers,
                    ..
                } = draw
                else {
                    continue;
                };
                let Some(AssetEntry::Sprite(sprite)) = self.assets.get(asset) else {
                    continue; // validated above
                };
                let model_offset = u64::from(slot) * self.instance_stride;
                queue.write_buffer(
                    self.instance_slots.require()?,
                    model_offset,
                    bytemuck::cast_slice(&transform.to_cols_array()),
                );
                for (layer_index, vertices) in layers.iter().enumerate() {
                    let layer = &sprite.layers[layer_index];
                    let bytes = (vertices.len() as u64) * 8;
                    queue.write_buffer(
                        self.sprite_vertices.require()?,
                        vertex_cursor,
                        bytemuck::cast_slice(vertices.as_slice()),
                    );
                    sprite_draws.push(SpriteDraw {
                        bind_group: layer.bind_group.clone(),
                        index_buffer: sprite.index_buffer.clone(),
                        model_offset: model_offset as u32,
                        vertex_offset: vertex_cursor,
                        vertex_count: vertices.len() as u32,
                        // Static artist UVs: never rewritten by a frame.
                        uv_buffer: sprite.uv_buffer.clone(),
                        uv_offset: layer.uv_offset,
                        index_offset: layer.index_offset,
                        index_count: layer.index_count,
                    });
                    vertex_cursor += bytes;
                }
                slot += 1;
            }
        }

        // ---- depth target --------------------------------------------------
        let need_depth = match &self.depth {
            Some(depth) => depth.width != size[0] || depth.height != size[1],
            None => true,
        };
        if need_depth {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Delicium scene depth"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.depth = Some(DepthTarget {
                width: size[0],
                height: size[1],
                view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            });
        }

        let oom_error = pollster::block_on(oom.pop());
        let validation_error = pollster::block_on(scope.pop());
        if let Some(error) = oom_error {
            return Err(RenderError::OutOfMemory {
                details: error.to_string(),
            });
        }
        if let Some(error) = validation_error {
            return Err(RenderError::GpuResource {
                details: format!("frame upload failed validation: {error}"),
            });
        }

        // ---- pass 1: 3D, depth tested, color cleared -----------------------
        let Some(depth) = self.depth.as_ref() else {
            return Err(RenderError::Internal {
                details: "depth target missing after creation".into(),
            });
        };
        {
            let color_attachment = wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            };
            let depth_attachment = wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Delicium 3D pass"),
                color_attachments: &[Some(color_attachment)],
                depth_stencil_attachment: Some(depth_attachment),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if !skeleton_draws.is_empty() {
                pass.set_pipeline(&self.skeleton_pipeline);
                pass.set_bind_group(0, &self.globals_bind_group, &[]);
                for draw in &skeleton_draws {
                    pass.set_bind_group(1, &self.palette_bind_group, &[draw.dynamic_offset]);
                    pass.set_bind_group(2, &draw.material_bind_group, &[]);
                    pass.set_vertex_buffer(0, draw.vertex_buffer.slice(..));
                    pass.set_index_buffer(draw.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..draw.index_count, 0, 0..1);
                }
            }
        }

        // ---- pass 2: 2D layers in order, color loaded ----------------------
        if !sprite_draws.is_empty() {
            let color_attachment = wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            };
            let vertex_buffer = self.sprite_vertices.require()?.clone();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Delicium 2D pass"),
                color_attachments: &[Some(color_attachment)],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.sprite_pipeline);
            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            for draw in &sprite_draws {
                pass.set_bind_group(1, &draw.bind_group, &[]);
                pass.set_bind_group(2, &self.instance_bind_group, &[draw.model_offset]);
                pass.set_vertex_buffer(
                    0,
                    vertex_buffer.slice(
                        draw.vertex_offset..draw.vertex_offset + u64::from(draw.vertex_count) * 8,
                    ),
                );
                // The static UV buffer is uploaded once at registration and
                // only sliced here.
                pass.set_vertex_buffer(
                    1,
                    draw.uv_buffer
                        .slice(draw.uv_offset..draw.uv_offset + u64::from(draw.vertex_count) * 8),
                );
                pass.set_index_buffer(
                    draw.index_buffer.slice(
                        draw.index_offset..draw.index_offset + u64::from(draw.index_count) * 4,
                    ),
                    wgpu::IndexFormat::Uint32,
                );
                pass.draw_indexed(0..draw.index_count, 0, 0..1);
            }
        }

        Ok(())
    }

    // -- asset preparation ---------------------------------------------------

    fn prepare_sprite(
        &self,
        layers: &[LayerAsset2d],
        images: &[Arc<ModelImage>],
    ) -> Result<PreparedAsset, String> {
        if layers.is_empty() {
            return Err("sprite asset has no layers".into());
        }
        if layers.len() > MAX_LAYERS_PER_ASSET {
            return Err(format!(
                "sprite asset has {} layers, the limit is {MAX_LAYERS_PER_ASSET}",
                layers.len()
            ));
        }
        let image_bytes = validate_images(&self.device, images)?;
        let mut prepared = Vec::with_capacity(layers.len());
        let mut uvs: Vec<[f32; 2]> = Vec::new();
        for (index, layer) in layers.iter().enumerate() {
            if layer.vertices.is_empty() {
                return Err(format!("layer {index} has no vertices"));
            }
            if layer.vertices.len() > MAX_LAYER_VERTICES {
                return Err(format!(
                    "layer {index} has {} vertices, the limit is {MAX_LAYER_VERTICES}",
                    layer.vertices.len()
                ));
            }
            for (vertex_index, vertex) in layer.vertices.iter().enumerate() {
                if !vertex[0].is_finite() || !vertex[1].is_finite() {
                    return Err(format!("layer {index} vertex {vertex_index} is not finite"));
                }
            }
            if layer.indices.is_empty() {
                return Err(format!("layer {index} has no indices"));
            }
            if layer.indices.len() > MAX_LAYER_INDICES {
                return Err(format!(
                    "layer {index} has {} indices, the limit is {MAX_LAYER_INDICES}",
                    layer.indices.len()
                ));
            }
            for (index_index, value) in layer.indices.iter().enumerate() {
                if *value as usize >= layer.vertices.len() {
                    return Err(format!(
                        "layer {index} index {index_index} ({value}) is out of vertex range {}",
                        layer.vertices.len()
                    ));
                }
            }
            if !layer.color.iter().all(|value| value.is_finite()) {
                return Err(format!("layer {index} color is not finite"));
            }
            // Static artist UVs: either exactly one per vertex (finite, inside
            // [0, 1]) or empty, in which case per-vertex zeros are generated
            // here, exactly once at registration. The draw slice must cover
            // the vertex count regardless, and the layer's texture flag stays
            // 0 so the zeros are never sampled.
            match layer.uvs.len() {
                0 => uvs.extend(std::iter::repeat_n([0.0, 0.0], layer.vertices.len())),
                count if count == layer.vertices.len() => {
                    for (vertex_index, uv) in layer.uvs.iter().enumerate() {
                        if !uv[0].is_finite() || !uv[1].is_finite() {
                            return Err(format!("layer {index} UV {vertex_index} is not finite"));
                        }
                        if !(0.0..=1.0).contains(&uv[0]) || !(0.0..=1.0).contains(&uv[1]) {
                            return Err(format!(
                                "layer {index} UV {vertex_index} ({}, {}) is outside [0, 1]",
                                uv[0], uv[1]
                            ));
                        }
                    }
                    uvs.extend_from_slice(&layer.uvs);
                }
                count => {
                    return Err(format!(
                        "layer {index} has {} vertices but {} UVs; they must match",
                        layer.vertices.len(),
                        count
                    ));
                }
            }
            if let Some(texture) = layer.texture {
                if texture >= images.len() {
                    return Err(format!(
                        "layer {index} references image {texture}, but the asset has {} images",
                        images.len()
                    ));
                }
                if layer.uvs.is_empty() {
                    return Err(format!(
                        "layer {index} samples image {texture} but has no UVs"
                    ));
                }
            }
            let (mask, uniform_mask) = match &layer.mask {
                Some(points) => {
                    let raster = rasterize_mask(points)
                        .map_err(|details| format!("layer {index}: {details}"))?;
                    (Some(raster), ([0.0, 0.0], [1.0, 1.0], [1u32, 0, 0, 0]))
                }
                None => (None, ([0.0, 0.0], [1.0, 1.0], [0u32, 0, 0, 0])),
            };
            let (mask_min, mask_extent, mut flags) = match &mask {
                Some(raster) => (raster.min, raster.extent, [1u32, 0, 0, 0]),
                None => uniform_mask,
            };
            // flags.y: the layer samples a base color texture. Legacy layers
            // (no texture) keep it 0 and never sample, so the generated
            // per-vertex zero UVs are never observable.
            flags[1] = u32::from(layer.texture.is_some());
            prepared.push(PreparedLayer {
                uniform: LayerUniform {
                    color: layer.color,
                    mask_min,
                    mask_extent,
                    flags,
                },
                vertex_count: layer.vertices.len() as u32,
                texture: layer.texture,
                indices: layer.indices.clone(),
                mask,
            });
        }
        // Every image must be referenced by at least one layer; the model
        // layer rejects unused resources, and an unreferenced renderer texture
        // would only pad the logical budget.
        for (image_index, image) in images.iter().enumerate() {
            if !layers
                .iter()
                .any(|layer| layer.texture == Some(image_index))
            {
                return Err(format!(
                    "image {image_index} ({}) is not referenced by any layer",
                    image.id
                ));
            }
        }
        Ok(PreparedAsset::Sprite(PreparedSprite {
            layers: prepared,
            images: images.to_vec(),
            uvs,
            image_bytes,
        }))
    }

    fn create_asset_resources(
        &mut self,
        _id: u64,
        prepared: PreparedAsset,
    ) -> Result<AssetEntry, RenderError> {
        match prepared {
            PreparedAsset::Sprite(sprite) => {
                let PreparedSprite {
                    layers,
                    images,
                    uvs,
                    image_bytes,
                } = sprite;
                // One texture per distinct model image, created once and
                // shared by every layer that references it (multi-layer and
                // multi-instance sharing comes from the per-asset views).
                let image_views = self.create_image_views(&images);
                let total_index_bytes: u64 = layers
                    .iter()
                    .map(|layer| (layer.indices.len() as u64) * 4)
                    .sum();
                let index_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Delicium sprite indices"),
                    size: total_index_bytes,
                    usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                // Single static UV upload for the whole asset, covering every
                // layer's per-vertex UVs (zeros for legacy untextured layers).
                // Never rewritten while the asset is registered.
                let uv_bytes = (uvs.len() as u64) * 8;
                let uv_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Delicium sprite UVs"),
                    size: uv_bytes,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                queue_write(
                    &self.queue,
                    &uv_buffer,
                    0,
                    bytemuck::cast_slice(uvs.as_slice()),
                );
                let mut index_cursor = 0;
                let mut uv_cursor = 0u64;
                let mut gpu_layers = Vec::with_capacity(layers.len());
                let mut mask_bytes = 0u64;
                for layer in layers {
                    queue_write(
                        &self.queue,
                        &index_buffer,
                        index_cursor,
                        bytemuck::cast_slice(layer.indices.as_slice()),
                    );
                    let index_count = layer.indices.len() as u32;
                    let index_offset = index_cursor;
                    index_cursor += (layer.indices.len() as u64) * 4;

                    let mask_view = match &layer.mask {
                        Some(raster) => {
                            mask_bytes += MASK_BYTES;
                            self.upload_mask(raster)?
                        }
                        None => self.white_view.clone(),
                    };
                    // Untextured layers bind the shared white fallback; the
                    // uniform flag keeps them on the exact legacy path.
                    let texture_view = match layer.texture {
                        None => &self.white_view,
                        Some(image_index) => &image_views[image_index],
                    };
                    let uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("Delicium layer constants"),
                        size: 64,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    self.queue
                        .write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&layer.uniform));
                    let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Delicium layer bind group"),
                        layout: &self.layer_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: uniform_buffer.as_entire_binding(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&mask_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 3,
                                resource: wgpu::BindingResource::TextureView(texture_view),
                            },
                        ],
                    });
                    gpu_layers.push(LayerGpu {
                        bind_group,
                        vertex_count: layer.vertex_count,
                        uv_offset: uv_cursor,
                        index_offset,
                        index_count,
                    });
                    uv_cursor += (layer.vertex_count as u64) * 8;
                }
                Ok(AssetEntry::Sprite(SpriteAsset {
                    layers: gpu_layers,
                    index_buffer,
                    uv_buffer,
                    mask_bytes,
                    image_bytes,
                    images: image_views,
                }))
            }
            PreparedAsset::Skeleton(skeleton) => {
                let image_views = self.create_image_views(&skeleton.images);
                let vertex_bytes = (skeleton.vertices.len() as u64) * SKIN_VERTEX_BYTES;
                let vertex_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Delicium skeleton vertices"),
                    size: vertex_bytes,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                // The single skin-vertex upload carries the static per-vertex
                // UVs, so no second static buffer is needed for 3D.
                self.queue.write_buffer(
                    &vertex_buffer,
                    0,
                    &encode_skin_vertices(&skeleton.vertices),
                );
                let index_bytes = (skeleton.indices.len() as u64) * 4;
                let index_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Delicium skeleton indices"),
                    size: index_bytes,
                    usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.queue.write_buffer(
                    &index_buffer,
                    0,
                    bytemuck::cast_slice(skeleton.indices.as_slice()),
                );

                let uniform = skeleton.material.unwrap_or_else(MaterialUniform::legacy);
                let uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Delicium material constants"),
                    size: MATERIAL_BYTES,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.queue
                    .write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniform));
                let texture_view = match skeleton.texture {
                    Some(index) => &image_views[index],
                    None => &self.white_view,
                };
                let material_bind_group =
                    self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Delicium material bind group"),
                        layout: &self.material_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: uniform_buffer.as_entire_binding(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(texture_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                            },
                        ],
                    });
                Ok(AssetEntry::Skeleton(SkeletonAsset {
                    vertex_buffer,
                    index_buffer,
                    index_count: skeleton.indices.len() as u32,
                    joint_count: skeleton.joint_count,
                    material_bind_group,
                    image_bytes: skeleton.image_bytes,
                    images: image_views,
                }))
            }
        }
    }

    /// Uploads one `Rgba8UnormSrgb` texture per validated model image and
    /// returns their views. The views own the textures, there is no global
    /// texture cache, and shared images inside one asset are created once.
    fn create_image_views(&self, images: &[Arc<ModelImage>]) -> Vec<wgpu::TextureView> {
        let mut views = Vec::with_capacity(images.len());
        for image in images {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Delicium model image"),
                size: wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &image.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.width * 4),
                    rows_per_image: Some(image.height),
                },
                wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
            );
            views.push(texture.create_view(&wgpu::TextureViewDescriptor::default()));
        }
        views
    }

    fn upload_mask(
        &self,
        raster: &super::mask::MaskRaster,
    ) -> Result<wgpu::TextureView, RenderError> {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Delicium layer mask"),
            size: wgpu::Extent3d {
                width: raster.size,
                height: raster.size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &raster.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(raster.size),
                rows_per_image: Some(raster.size),
            },
            wgpu::Extent3d {
                width: raster.size,
                height: raster.size,
                depth_or_array_layers: 1,
            },
        );
        Ok(texture.create_view(&wgpu::TextureViewDescriptor::default()))
    }

    /// The dynamic offset alignment this renderer reserves per slot.
    pub fn uniform_alignment(&self) -> u64 {
        self.uniform_alignment
    }

    /// Logical resource accounting read directly from live state: the asset
    /// count, the registered mask and model image bytes, the monotonic
    /// creation counters and the three grow-only buffer capacities. The
    /// registered sizes are live state, not mirrored counters; only the two
    /// monotonic creation counters are accumulated.
    ///
    /// All values are logical: `texture_bytes` are the registered model image
    /// RGBA bytes the renderer tracks, not a driver VRAM measurement or a
    /// whole-process memory figure.
    pub fn resource_diagnostics(&self) -> SceneResourceDiagnostics {
        SceneResourceDiagnostics {
            asset_count: self.assets.len(),
            mask_bytes: self.mask_bytes,
            sprite_vertex_capacity_bytes: self.sprite_vertices.capacity,
            instance_capacity_bytes: self.instance_slots.capacity,
            palette_capacity_bytes: self.palette_slots.capacity,
            texture_count: self.registered_image_count,
            texture_bytes: self.image_bytes_total,
            texture_creations: self.texture_creations,
            uv_uploads: self.uv_uploads,
        }
    }
}

/// CPU validation for one 3D asset.
fn prepare_skeleton(
    device: &wgpu::Device,
    vertices: &[SkinVertex],
    indices: &[u32],
    joint_count: usize,
    images: &[Arc<ModelImage>],
    texture: Option<usize>,
    material: &Option<Material3d>,
) -> Result<PreparedAsset, String> {
    if vertices.is_empty() {
        return Err("skeleton asset has no vertices".into());
    }
    if vertices.len() > MAX_SKELETON_VERTICES {
        return Err(format!(
            "skeleton has {} vertices, the limit is {MAX_SKELETON_VERTICES}",
            vertices.len()
        ));
    }
    if indices.is_empty() {
        return Err("skeleton asset has no indices".into());
    }
    if indices.len() > MAX_SKELETON_INDICES {
        return Err(format!(
            "skeleton has {} indices, the limit is {MAX_SKELETON_INDICES}",
            indices.len()
        ));
    }
    for (index, value) in indices.iter().enumerate() {
        if *value as usize >= vertices.len() {
            return Err(format!(
                "index {index} ({value}) is out of vertex range {}",
                vertices.len()
            ));
        }
    }
    if joint_count > MAX_JOINTS {
        return Err(format!(
            "skeleton has {joint_count} joints, the limit is {MAX_JOINTS}"
        ));
    }
    let palette_bytes = 64 + (joint_count.max(1) as u64) * 64;
    let binding_limit = device.limits().max_uniform_buffer_binding_size;
    if palette_bytes > binding_limit {
        return Err(format!(
            "skeleton palette of {palette_bytes} bytes exceeds the device uniform binding limit of {binding_limit}"
        ));
    }
    let image_bytes = validate_images(device, images)?;
    for (index, vertex) in vertices.iter().enumerate() {
        // Untextured legacy vertices carry zeros here and are validated like
        // textured ones: the UV is a static attribute on every path.
        if !vertex.uv[0].is_finite() || !vertex.uv[1].is_finite() {
            return Err(format!("vertex {index} UV is not finite"));
        }
        if !(0.0..=1.0).contains(&vertex.uv[0]) || !(0.0..=1.0).contains(&vertex.uv[1]) {
            return Err(format!(
                "vertex {index} UV ({}, {}) is outside [0, 1]",
                vertex.uv[0], vertex.uv[1]
            ));
        }
        if !vertex.position.iter().all(|value| value.is_finite()) {
            return Err(format!("vertex {index} position is not finite"));
        }
        if !vertex.normal.iter().all(|value| value.is_finite()) {
            return Err(format!("vertex {index} normal is not finite"));
        }
        if !vertex.color.iter().all(|value| value.is_finite()) {
            return Err(format!("vertex {index} color is not finite"));
        }
        if !vertex.weights.iter().all(|value| value.is_finite()) {
            return Err(format!("vertex {index} weights are not finite"));
        }
        if joint_count == 0 {
            if vertex.weights.iter().any(|weight| *weight != 0.0) {
                return Err(format!(
                    "vertex {index} has skin weights but the skeleton has no joints"
                ));
            }
        } else {
            for (slot, joint) in vertex.joints.iter().enumerate() {
                if *joint as usize >= joint_count {
                    return Err(format!(
                        "vertex {index} joint slot {slot} references joint {joint}, but there are only {joint_count} joints"
                    ));
                }
            }
        }
    }
    // A 3D asset has at most one base color image and must reference it; the
    // model layer also rejects unused resources, and the renderer does not
    // silently upload something no draw can reach.
    let default_material = Material3d::default();
    let (texture, effective): (Option<usize>, Option<&Material3d>) = match texture {
        None if images.is_empty() => {
            // Legacy path: no image and no material keeps the output
            // unchanged. A declared material still works from the vertex
            // color.
            let effective = match material {
                Some(material) => Some(validate_material(material)?),
                None => None,
            };
            enforce_opaque_alpha(effective, None, vertices)?;
            (None, effective)
        }
        None => {
            return Err(format!(
                "skeleton declares {} images but references none",
                images.len()
            ));
        }
        Some(texture) => {
            if texture >= images.len() {
                return Err(format!(
                    "skeleton references image {texture}, but the asset has {} images",
                    images.len()
                ));
            }
            if images.len() != 1 {
                return Err(format!(
                    "skeleton declares {} images but can only reference the single base color image",
                    images.len()
                ));
            }
            // A textured asset without a declared material gets the default
            // PBR opaque material; its alpha contract is enforced exactly
            // like an explicitly declared one.
            let effective = match material {
                Some(material) => validate_material(material)?,
                None => &default_material,
            };
            enforce_opaque_alpha(
                Some(effective),
                Some((texture, images[texture].as_ref())),
                vertices,
            )?;
            (Some(texture), Some(effective))
        }
    };
    let prepared_material = effective.map(|material| MaterialUniform::new(material, texture));
    Ok(PreparedAsset::Skeleton(PreparedSkeleton {
        vertices: vertices.to_vec(),
        indices: indices.to_vec(),
        joint_count,
        images: images.to_vec(),
        texture,
        material: prepared_material,
        image_bytes,
    }))
}

/// Validates the material scalar ranges and the cutout threshold. Returns the
/// material so callers can keep using it after validation.
fn validate_material(material: &Material3d) -> Result<&Material3d, String> {
    if !material.roughness.is_finite() || !(0.0..=1.0).contains(&material.roughness) {
        return Err(format!(
            "material roughness {} is not a finite value in [0, 1]",
            material.roughness
        ));
    }
    if !material.metallic.is_finite() || !(0.0..=1.0).contains(&material.metallic) {
        return Err(format!(
            "material metallic {} is not a finite value in [0, 1]",
            material.metallic
        ));
    }
    if !material.alpha_cutoff.is_finite() {
        return Err("material alpha cutoff is not finite".into());
    }
    if matches!(material.alpha_mode, AlphaMode::Cutout)
        && !(material.alpha_cutoff > 0.0 && material.alpha_cutoff <= 1.0)
    {
        return Err(format!(
            "cutout alpha cutoff {} is outside (0, 1]",
            material.alpha_cutoff
        ));
    }
    Ok(material)
}

/// Enforces the opaque alpha contract: an opaque material requires a fully
/// opaque image (when textured) and every vertex alpha to be exactly 1.
fn enforce_opaque_alpha(
    material: Option<&Material3d>,
    image: Option<(usize, &ModelImage)>,
    vertices: &[SkinVertex],
) -> Result<(), String> {
    let Some(material) = material else {
        return Ok(());
    };
    if !matches!(material.alpha_mode, AlphaMode::Opaque) {
        return Ok(());
    }
    if let Some((index, image)) = image
        && !image.opaque
    {
        return Err(format!(
            "opaque material requires a fully opaque image, but image {index} ({}) has transparent texels",
            image.id
        ));
    }
    for (index, vertex) in vertices.iter().enumerate() {
        if vertex.color[3] != 1.0 {
            return Err(format!(
                "opaque material requires every vertex alpha to be 1.0, but vertex {index} has alpha {}",
                vertex.color[3]
            ));
        }
    }
    Ok(())
}

/// Independent preflight of the model images of one candidate asset. Nothing
/// here touches the GPU: duplicate and format problems are rejected before
/// any texture is created.
fn validate_images(device: &wgpu::Device, images: &[Arc<ModelImage>]) -> Result<u64, String> {
    if images.len() > MAX_IMAGES_PER_ASSET {
        return Err(format!(
            "asset has {} images, the limit is {MAX_IMAGES_PER_ASSET}",
            images.len()
        ));
    }
    let device_dimension = device.limits().max_texture_dimension_2d;
    let mut seen: HashSet<&str> = HashSet::with_capacity(images.len());
    let mut total_bytes = 0u64;
    for (index, image) in images.iter().enumerate() {
        if !seen.insert(image.id.as_str()) {
            return Err(format!(
                "image {index} repeats the id {:?} of an earlier image",
                image.id
            ));
        }
        if image.width == 0 || image.height == 0 {
            return Err(format!(
                "image {index} ({}) has a zero dimension ({}x{})",
                image.id, image.width, image.height
            ));
        }
        if image.width > MAX_IMAGE_DIM || image.height > MAX_IMAGE_DIM {
            return Err(format!(
                "image {index} ({}) is {}x{}, the limit is {MAX_IMAGE_DIM} per axis",
                image.id, image.width, image.height
            ));
        }
        if image.width > device_dimension || image.height > device_dimension {
            return Err(format!(
                "image {index} ({}) is {}x{}, above the device limit of {device_dimension} per axis",
                image.id, image.width, image.height
            ));
        }
        let expected = (image.width as u64)
            .checked_mul(image.height as u64)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| format!("image {index} ({}) dimensions overflow", image.id))?;
        if image.rgba.len() as u64 != expected {
            return Err(format!(
                "image {index} ({}) is {}x{} and needs {expected} RGBA bytes, but carries {}",
                image.id,
                image.width,
                image.height,
                image.rgba.len()
            ));
        }
        if expected > MAX_IMAGE_BYTES {
            return Err(format!(
                "image {index} ({}) needs {expected} RGBA bytes, the per-image limit is {MAX_IMAGE_BYTES}",
                image.id
            ));
        }
        total_bytes += expected;
        if total_bytes > MAX_ASSET_IMAGE_BYTES {
            return Err(format!(
                "images of the asset need {total_bytes} RGBA bytes, the per-asset limit is {MAX_ASSET_IMAGE_BYTES}"
            ));
        }
        // The declared visibility flags are re-derived from the actual pixels:
        // a caller-provided boolean is never trusted.
        let mut opaque = true;
        let mut fully_transparent = true;
        for (texel_index, texel) in image.rgba.as_chunks::<4>().0.iter().enumerate() {
            let alpha = texel[3];
            // The bytes are sRGB-encoded linear-premultiplied RGB, so an
            // alpha-zero texel must carry zero RGB: anything else would let a
            // sample emit color that the metadata calls fully transparent.
            // Reject instead of silently mutating caller data.
            if alpha == 0 && (texel[0] != 0 || texel[1] != 0 || texel[2] != 0) {
                return Err(format!(
                    "image {index} ({}) texel {texel_index} has alpha 0 but non-zero premultiplied RGB ({}, {}, {})",
                    image.id, texel[0], texel[1], texel[2]
                ));
            }
            opaque &= alpha == 255;
            fully_transparent &= alpha == 0;
        }
        if opaque != image.opaque {
            return Err(format!(
                "image {index} ({}) declares opaque={} but its pixels say {opaque}",
                image.id, image.opaque
            ));
        }
        if fully_transparent != image.fully_transparent {
            return Err(format!(
                "image {index} ({}) declares fully_transparent={} but its pixels say {fully_transparent}",
                image.id, image.fully_transparent
            ));
        }
    }
    Ok(total_bytes)
}

/// Encodes skin vertices into the exact 80-byte vertex buffer layout. The UV
/// is a static vertex attribute: it rides along with the single static skin
/// vertex upload and is never recomputed by a frame.
fn encode_skin_vertices(vertices: &[SkinVertex]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vertices.len() * SKIN_VERTEX_BYTES as usize);
    for vertex in vertices {
        for value in vertex.position {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        for value in vertex.normal {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        for value in vertex.color {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        for value in vertex.joints {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        for value in vertex.weights {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        for value in vertex.uv {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    bytes
}

fn queue_write(queue: &wgpu::Queue, buffer: &wgpu::Buffer, offset: u64, data: &[u8]) {
    if !data.is_empty() {
        queue.write_buffer(buffer, offset, data);
    }
}

fn create_dynamic_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &'static str,
    buffer: &wgpu::Buffer,
    size: u64,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: NonZeroU64::new(size),
            }),
        }],
    })
}

fn align_up(value: u64, alignment: u64) -> u64 {
    if alignment <= 1 {
        return value;
    }
    value.div_ceil(alignment) * alignment
}

fn unknown_or_mismatched(
    assets: &HashMap<u64, AssetEntry>,
    asset: u64,
    instance: u64,
    kind: &str,
) -> RenderError {
    match assets.get(&asset) {
        None => RenderError::UnknownAsset { asset },
        Some(_) => RenderError::InvalidFrame {
            instance,
            details: format!("asset {asset} is not a {kind} asset"),
        },
    }
}
