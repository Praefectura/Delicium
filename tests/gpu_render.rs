use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use delicium::model::{AlphaMode, Material3d, ModelImage};
use delicium::renderer::{
    InstanceDraw, LayerAsset2d, RenderAsset, RenderFrame, SceneRenderer, SkinVertex,
};
use glam::{Mat4, Vec3};

const SIZE: u32 = 256;
const ROW_BYTES: u32 = SIZE * 4;

struct GpuFixture {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    renderer: SceneRenderer,
    texture: wgpu::Texture,
    directory: PathBuf,
}

impl GpuFixture {
    fn new() -> Self {
        Self::with_limits(wgpu::Limits::default())
    }

    fn with_limits(limits: wgpu::Limits) -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12 | wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("GPU validation blocked: no DX12/Vulkan adapter; this is not a skipped pass");
        let info = adapter.get_info();
        assert_ne!(
            info.device_type,
            wgpu::DeviceType::Cpu,
            "GPU validation blocked: software adapter does not establish hardware rendering"
        );
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Delicium offscreen test device"),
            required_limits: limits,
            ..Default::default()
        }))
        .expect("GPU validation blocked: device creation failed");
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let renderer = SceneRenderer::new(
            device.clone(),
            queue.clone(),
            wgpu::TextureFormat::Rgba8Unorm,
        )
        .expect("Actual scene pipelines and WGSL must compile");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Delicium offscreen test target"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".artifacts")
            .join(format!(
                "gpu-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("adapter.txt"), format!("{info:?}\n")).unwrap();
        println!("GPU adapter: {info:?}; artifacts: {}", directory.display());
        Self {
            device,
            queue,
            renderer,
            texture,
            directory,
        }
    }

    fn render(&mut self, frame: &RenderFrame, filename: &str) -> Vec<u8> {
        let view = self
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Delicium offscreen readback"),
            size: u64::from(ROW_BYTES) * u64::from(SIZE),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let error_scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Delicium offscreen test commands"),
            });
        self.renderer
            .render_to(frame, &view, [SIZE, SIZE], &mut encoder)
            .unwrap();
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ROW_BYTES),
                    rows_per_image: Some(SIZE),
                },
            },
            wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let (send, receive) = mpsc::sync_channel(1);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(15)),
            })
            .expect("GPU validation blocked: command completion timed out");
        receive
            .recv_timeout(Duration::from_secs(15))
            .expect("Readback callback timed out")
            .expect("Readback mapping failed");
        let pixels = readback
            .slice(..)
            .get_mapped_range()
            .expect("Readback mapped range is unavailable")
            .to_vec();
        readback.unmap();
        let error = pollster::block_on(error_scope.pop());
        assert!(error.is_none(), "GPU validation error: {error:?}");
        write_png(&self.directory.join(filename), &pixels);
        pixels
    }
}

fn write_png(path: &Path, pixels: &[u8]) {
    let mut encoder = png::Encoder::new(std::fs::File::create(path).unwrap(), SIZE, SIZE);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(pixels)
        .unwrap();
}

fn pixel_at(pixels: &[u8], point: [f32; 2]) -> [u8; 4] {
    let x = ((point[0] / 4.5 + 0.5) * SIZE as f32) as usize;
    let y = ((3.6 - point[1]) / 4.5 * SIZE as f32) as usize;
    let offset = (y * SIZE as usize + x) * 4;
    pixels[offset..offset + 4].try_into().unwrap()
}

fn assert_pixel(actual: [u8; 4], expected: [u8; 4]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!(
            actual.abs_diff(expected) <= 5,
            "channel {actual}, expected {expected}"
        );
    }
}

fn quad(color: [f32; 4], mask: Option<Vec<[f32; 2]>>) -> LayerAsset2d {
    LayerAsset2d {
        vertices: vec![[-1.0, 0.0], [1.0, 0.0], [1.0, 2.0], [-1.0, 2.0]],
        indices: vec![0, 1, 2, 0, 2, 3],
        color,
        mask,
        uvs: Vec::new(),
        texture: None,
    }
}

fn sprite_frame(asset: u64, layers: &[LayerAsset2d]) -> RenderFrame {
    RenderFrame {
        instances: vec![InstanceDraw::Sprite2d {
            asset,
            instance: 1,
            transform: Mat4::IDENTITY,
            layers: layers.iter().map(|layer| layer.vertices.clone()).collect(),
        }],
    }
}

fn skeletal_frame(translation: f32) -> RenderFrame {
    RenderFrame {
        instances: vec![InstanceDraw::Skeleton3d {
            asset: 3,
            instance: 3,
            transform: Mat4::IDENTITY,
            skin_matrices: vec![Mat4::from_translation(glam::Vec3::new(
                translation,
                0.0,
                0.0,
            ))],
        }],
    }
}

#[test]
fn actual_gpu_checks_alpha_masks_layer_order_skinning_and_clear() {
    let mut gpu = GpuFixture::new();
    let masked = vec![quad(
        [1.0, 0.2, 0.1, 0.5],
        Some(vec![
            [-0.25, 0.25],
            [0.25, 0.25],
            [0.25, 1.75],
            [-0.25, 1.75],
        ]),
    )];
    gpu.renderer
        .register_asset(
            1,
            &RenderAsset::Sprite2d {
                images: Vec::new(),
                layers: masked.clone(),
            },
        )
        .unwrap();
    let pixels = gpu.render(&sprite_frame(1, &masked), "masked-premultiplied.png");
    assert_pixel(pixel_at(&pixels, [0.0, 1.0]), [128, 26, 13, 128]);
    assert_pixel(pixel_at(&pixels, [0.7, 1.0]), [0, 0, 0, 0]);
    assert_pixel(pixel_at(&pixels, [-1.5, 1.0]), [0, 0, 0, 0]);

    let layers = vec![
        quad([1.0, 0.0, 0.0, 1.0], None),
        quad([0.0, 1.0, 0.0, 0.5], None),
    ];
    gpu.renderer
        .register_asset(
            2,
            &RenderAsset::Sprite2d {
                images: Vec::new(),
                layers: layers.clone(),
            },
        )
        .unwrap();
    let pixels = gpu.render(&sprite_frame(2, &layers), "ordered-layers.png");
    assert_pixel(pixel_at(&pixels, [0.0, 1.0]), [128, 128, 0, 255]);
    assert_pixel(pixel_at(&pixels, [0.7, 1.0]), [128, 128, 0, 255]);
    let mut deformed = sprite_frame(2, &layers);
    let InstanceDraw::Sprite2d {
        layers: dynamic_layers,
        ..
    } = &mut deformed.instances[0]
    else {
        unreachable!()
    };
    for vertex in dynamic_layers.iter_mut().flatten() {
        vertex[0] += 2.2;
    }
    let pixels = gpu.render(&deformed, "deformed-2d.png");
    assert_pixel(pixel_at(&pixels, [0.0, 1.0]), [0, 0, 0, 0]);
    assert_pixel(pixel_at(&pixels, [1.7, 1.0]), [128, 128, 0, 255]);

    let vertices =
        [[-0.4, 1.0, 0.0], [0.4, 1.0, 0.0], [0.0, 1.8, 0.0]].map(|position| SkinVertex {
            position,
            normal: [0.0, 0.0, 1.0],
            color: [0.1, 0.5, 1.0, 1.0],
            joints: [0; 4],
            weights: [1.0, 0.0, 0.0, 0.0],
            uv: [0.0, 0.0],
        });
    gpu.renderer
        .register_asset(
            3,
            &RenderAsset::Skeleton3d {
                vertices: vertices.to_vec(),
                indices: vec![0, 1, 2],
                joint_count: 1,
                images: Vec::new(),
                texture: None,
                material: None,
            },
        )
        .unwrap();
    let first = gpu.render(&skeletal_frame(0.0), "skinning-before.png");
    let after = gpu.render(&skeletal_frame(0.8), "skinning-after.png");
    assert_eq!(
        pixel_at(&first, [0.0, 1.3])[3],
        255,
        "Bind pose must cover the original point"
    );
    assert_eq!(
        pixel_at(&after, [0.0, 1.3])[3],
        0,
        "GPU skinning must move away from the old point"
    );
    assert_eq!(pixel_at(&first, [0.8, 1.3])[3], 0);
    assert_eq!(
        pixel_at(&after, [0.8, 1.3])[3],
        255,
        "The palette update must move actual pixels"
    );

    let mut mixed = sprite_frame(2, &layers);
    mixed.instances.extend(skeletal_frame(0.0).instances);
    let pixels = gpu.render(&mixed, "mixed-2d-over-3d.png");
    assert_pixel(pixel_at(&pixels, [0.0, 1.3]), [128, 128, 0, 255]);

    let empty = gpu.render(&RenderFrame::default(), "empty-after-actors.png");
    assert!(
        empty.iter().all(|byte| *byte == 0),
        "Removing all instances must not leave ghost pixels"
    );
    gpu.renderer.remove_asset(1);
    gpu.renderer.remove_asset(2);
    gpu.renderer.remove_asset(3);
}

#[test]
fn failed_gpu_registration_keeps_the_previous_asset_and_mask() {
    let mut gpu = GpuFixture::with_limits(wgpu::Limits {
        max_buffer_size: 256 * 1024,
        ..Default::default()
    });
    let layers = vec![quad(
        [1.0, 0.0, 0.0, 1.0],
        Some(vec![
            [-0.25, 0.25],
            [0.25, 0.25],
            [0.25, 1.75],
            [-0.25, 1.75],
        ]),
    )];
    gpu.renderer
        .register_asset(
            1,
            &RenderAsset::Sprite2d {
                images: Vec::new(),
                layers: layers.clone(),
            },
        )
        .unwrap();
    let registered = gpu.renderer.resource_diagnostics();
    assert_eq!(registered.asset_count, 1);
    assert!(registered.mask_bytes > 0);
    let frame = sprite_frame(1, &layers);
    let before = gpu.render(&frame, "before-rejected-gpu-replacement.png");
    let mut oversized = quad([0.0, 1.0, 0.0, 1.0], None);
    // The 256 KiB limit permits readback but rejects this 288,000-byte buffer.
    oversized.indices = oversized.indices.repeat(12000);
    let result = gpu.renderer.register_asset(
        1,
        &RenderAsset::Sprite2d {
            images: Vec::new(),
            layers: vec![oversized],
        },
    );
    assert!(
        matches!(
            result,
            Err(delicium::renderer::RenderError::GpuResource { .. })
        ),
        "The device buffer limit must reject GPU creation, not CPU model validation: {result:?}"
    );
    let after = gpu.render(&frame, "after-rejected-gpu-replacement.png");
    let preserved = gpu.renderer.resource_diagnostics();
    assert_eq!(preserved.asset_count, registered.asset_count);
    assert_eq!(preserved.mask_bytes, registered.mask_bytes);
    assert_eq!(
        after, before,
        "Rejected GPU resources must not replace the old asset or mask"
    );
    assert_pixel(pixel_at(&after, [0.0, 1.0]), [255, 0, 0, 255]);
    assert_pixel(pixel_at(&after, [0.7, 1.0]), [0, 0, 0, 0]);
}

fn image(id: &str, width: u32, height: u32, rgba: Vec<u8>) -> Arc<ModelImage> {
    let opaque = rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255);
    let fully_transparent = rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0);
    Arc::new(ModelImage {
        id: id.into(),
        width,
        height,
        rgba: rgba.into(),
        opaque,
        fully_transparent,
    })
}

fn textured_quad() -> LayerAsset2d {
    let mut layer = quad([1.0; 4], None);
    layer.texture = Some(0);
    layer.uvs = vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
    layer
}

fn skin_quad(z: f32, color: [f32; 4], normal: [f32; 3]) -> Vec<SkinVertex> {
    let layer = textured_quad();
    layer
        .vertices
        .iter()
        .zip(layer.uvs)
        .map(|(position, uv)| SkinVertex {
            position: [position[0], position[1], z],
            normal,
            color,
            joints: [0; 4],
            weights: [1.0, 0.0, 0.0, 0.0],
            uv,
        })
        .collect()
}

fn skin_draw(asset: u64, instance: u64, dx: f32) -> InstanceDraw {
    InstanceDraw::Skeleton3d {
        asset,
        instance,
        transform: Mat4::IDENTITY,
        skin_matrices: vec![Mat4::from_translation(Vec3::new(dx, 0.0, 0.0))],
    }
}

fn material_reference(base: [f32; 3], normal: [f32; 3], material: Material3d) -> [u8; 4] {
    // Independent f64 reference; no renderer helpers or WGSL execution.
    let normalize = |v: [f64; 3]| {
        let length = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        v.map(|x| x / length)
    };
    let dot = |a: [f64; 3], b: [f64; 3]| a.into_iter().zip(b).map(|(a, b)| a * b).sum::<f64>();
    let n = normalize(normal.map(f64::from));
    let l = normalize([0.35, 0.8, 0.45]);
    let v = [0.0, 0.0, 1.0];
    let h = normalize([l[0], l[1], l[2] + 1.0]);
    let nl = dot(n, l).max(0.0);
    let nv = dot(n, v).max(1e-4);
    let nh = dot(n, h).clamp(0.0, 1.0);
    let vh = dot(v, h).clamp(0.0, 1.0);
    let alpha = f64::from(material.roughness).powi(2).max(1e-4);
    let alpha_squared = alpha * alpha;
    let denominator = (1.0 - nh * nh) + nh * nh * alpha_squared;
    let distribution = alpha_squared / (std::f64::consts::PI * denominator * denominator);
    let k = alpha / 2.0;
    let geometry = (nl / (nl * (1.0 - k) + k)) * (nv / (nv * (1.0 - k) + k));
    let metallic = f64::from(material.metallic);
    let rgb: [u8; 3] = base.map(|channel| {
        let channel = f64::from(channel);
        let f0 = 0.04 * (1.0 - metallic) + channel * metallic;
        let fresnel = f0 + (1.0 - f0) * (1.0 - vh).powi(5);
        let specular = distribution * geometry * fresnel / (4.0 * nl * nv).max(1e-4);
        let diffuse = (1.0 - fresnel) * (1.0 - metallic) * channel / std::f64::consts::PI;
        let linear = channel * (1.0 - metallic) * 0.25 + (diffuse + specular) * nl * 0.75;
        assert!(linear.is_finite());
        (linear.clamp(0.0, 1.0) * 255.0).round() as u8
    });
    [rgb[0], rgb[1], rgb[2], 255]
}

#[test]
fn roughness_and_metallic_match_numeric_reference_not_legacy_lambert() {
    let mut gpu = GpuFixture::new();
    let base = [0.32, 0.16, 0.08];
    let light = Vec3::new(0.35, 0.8, 0.45).normalize();
    let normal = (light + Vec3::Z).normalize().to_array();
    let vertices = skin_quad(0.2, [base[0], base[1], base[2], 1.0], normal);
    let materials = [
        Material3d {
            roughness: 0.4,
            ..Material3d::default()
        },
        Material3d {
            roughness: 0.8,
            ..Material3d::default()
        },
        Material3d {
            roughness: 0.65,
            ..Material3d::default()
        },
        Material3d {
            roughness: 0.65,
            metallic: 0.8,
            ..Material3d::default()
        },
        Material3d {
            roughness: 0.0,
            ..Material3d::default()
        },
    ];
    let frame = RenderFrame {
        instances: vec![skin_draw(51, 1, 0.0)],
    };
    let mut observed = Vec::new();
    for (index, material) in materials.into_iter().enumerate() {
        gpu.renderer
            .register_asset(
                51,
                &RenderAsset::Skeleton3d {
                    vertices: vertices.clone(),
                    indices: vec![0, 1, 2, 0, 2, 3],
                    joint_count: 1,
                    images: Vec::new(),
                    texture: None,
                    material: Some(material),
                },
            )
            .unwrap();
        let pixels = gpu.render(&frame, &format!("material-reference-{index}.png"));
        let actual = pixel_at(&pixels, [0.0, 1.0]);
        let expected = material_reference(base, normal, material);
        println!(
            "material case={index} roughness={} metallic={} actual={actual:?} reference={expected:?}",
            material.roughness, material.metallic
        );
        assert_pixel(actual, expected);
        observed.push(actual);
    }
    assert!(observed[0][0].abs_diff(observed[1][0]) > 10);
    assert!((0..3).any(|channel| observed[2][channel].abs_diff(observed[3][channel]) >= 8));
    gpu.renderer
        .register_asset(
            51,
            &RenderAsset::Skeleton3d {
                vertices,
                indices: vec![0, 1, 2, 0, 2, 3],
                joint_count: 1,
                images: Vec::new(),
                texture: None,
                material: None,
            },
        )
        .unwrap();
    let pixels = gpu.render(&frame, "legacy-lambert-still-independent.png");
    let intensity = 0.25 + 0.75 * Vec3::from_array(normal).dot(light).max(0.0);
    let expected = base.map(|channel| (channel * intensity * 255.0).round() as u8);
    assert_pixel(
        pixel_at(&pixels, [0.0, 1.0]),
        [expected[0], expected[1], expected[2], 255],
    );
}

#[test]
fn textured_sprite_checks_uv_srgb_premultiplication_mask_and_static_uploads() {
    let mut gpu = GpuFixture::new();
    let layer = textured_quad();
    let colors = image(
        "quadrants",
        2,
        2,
        vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ],
    );
    gpu.renderer
        .register_asset(
            11,
            &RenderAsset::Sprite2d {
                layers: vec![layer.clone()],
                images: vec![colors.clone()],
            },
        )
        .unwrap();
    let frame = sprite_frame(11, std::slice::from_ref(&layer));
    let before = gpu.renderer.resource_diagnostics();
    assert_eq!(before.texture_creations, 1);
    assert_eq!(before.uv_uploads, 1);
    let pixels = gpu.render(&frame, "artist-uv-quadrants.png");
    for (point, color) in [
        ([-0.75, 1.75], [255, 0, 0, 255]),
        ([0.75, 1.75], [0, 255, 0, 255]),
        ([-0.75, 0.25], [0, 0, 255, 255]),
        ([0.75, 0.25], [255, 255, 0, 255]),
    ] {
        assert_pixel(pixel_at(&pixels, point), color);
    }
    let mut deformed = frame.clone();
    let InstanceDraw::Sprite2d { layers, .. } = &mut deformed.instances[0] else {
        unreachable!()
    };
    for vertex in &mut layers[0] {
        vertex[0] += 0.5;
    }
    let pixels = gpu.render(&deformed, "artist-uv-deformed.png");
    assert_pixel(pixel_at(&pixels, [-0.25, 1.75]), [255, 0, 0, 255]);
    assert_pixel(pixel_at(&pixels, [-0.9, 1.75]), [0, 0, 0, 0]);
    let after = gpu.renderer.resource_diagnostics();
    assert_eq!(before.texture_creations, after.texture_creations);
    assert_eq!(before.uv_uploads, after.uv_uploads);
    assert_eq!(after.texture_count, 1);
    assert_eq!(after.texture_bytes, 16);

    let mut masked = layer.clone();
    masked.mask = Some(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 2.0], [0.0, 2.0]]);
    gpu.renderer
        .register_asset(
            11,
            &RenderAsset::Sprite2d {
                layers: vec![masked.clone()],
                images: vec![colors],
            },
        )
        .unwrap();
    let pixels = gpu.render(
        &sprite_frame(11, &[masked]),
        "artist-uv-independent-mask.png",
    );
    assert_pixel(pixel_at(&pixels, [-0.75, 1.75]), [0, 0, 0, 0]);
    assert_pixel(pixel_at(&pixels, [0.75, 1.75]), [0, 255, 0, 255]);

    gpu.renderer
        .register_asset(
            11,
            &RenderAsset::Sprite2d {
                layers: vec![layer.clone()],
                images: vec![image("gray", 1, 1, vec![128, 128, 128, 255])],
            },
        )
        .unwrap();
    let pixels = gpu.render(&frame, "srgb-decoded-to-linear-target.png");
    assert_pixel(pixel_at(&pixels, [0.0, 1.0]), [55, 55, 55, 255]);
    let mut tinted = layer;
    tinted.color[3] = 0.5;
    gpu.renderer
        .register_asset(
            11,
            &RenderAsset::Sprite2d {
                layers: vec![tinted],
                images: vec![image("premultiplied", 1, 1, vec![188, 0, 0, 128])],
            },
        )
        .unwrap();
    let pixels = gpu.render(&frame, "linear-premultiplied-no-double-alpha.png");
    assert_pixel(pixel_at(&pixels, [0.0, 1.0]), [64, 0, 0, 64]);
}

#[test]
fn textured_skin_keeps_uv_with_skinning_and_cutout_does_not_write_hole_depth() {
    let mut gpu = GpuFixture::new();
    let colors = image(
        "quadrants",
        2,
        2,
        vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ],
    );
    let opaque_asset = RenderAsset::Skeleton3d {
        vertices: skin_quad(0.2, [1.0; 4], [0.0, 0.0, 1.0]),
        indices: vec![0, 1, 2, 0, 2, 3],
        joint_count: 1,
        images: vec![colors],
        texture: Some(0),
        material: Some(Material3d::default()),
    };
    gpu.renderer.register_asset(21, &opaque_asset).unwrap();
    let frame = RenderFrame {
        instances: vec![skin_draw(21, 1, 0.0)],
    };
    let pixels = gpu.render(&frame, "skinned-artist-uv.png");
    let red = pixel_at(&pixels, [-0.75, 1.75]);
    let green = pixel_at(&pixels, [0.75, 1.75]);
    assert!(red[0] > red[1] + 30 && green[1] > green[0] + 30);
    let pixels = gpu.render(
        &RenderFrame {
            instances: vec![skin_draw(21, 1, 0.8)],
        },
        "skinned-artist-uv-moved.png",
    );
    assert_pixel(pixel_at(&pixels, [0.05, 1.75]), red);
    assert_eq!(pixel_at(&pixels, [-0.9, 1.75])[3], 0);
    let before = gpu.renderer.resource_diagnostics();
    let oversized_width = gpu.device.limits().max_texture_dimension_2d + 1;
    for case in 0..6 {
        let mut invalid = opaque_asset.clone();
        let RenderAsset::Skeleton3d {
            vertices,
            images,
            material,
            ..
        } = &mut invalid
        else {
            unreachable!()
        };
        match case {
            0 => material.as_mut().unwrap().roughness = f32::NAN,
            1 => material.as_mut().unwrap().metallic = 1.1,
            2 => {
                let material = material.as_mut().unwrap();
                material.alpha_mode = AlphaMode::Cutout;
                material.alpha_cutoff = 0.0;
            }
            3 => vertices[0].color[3] = 0.5,
            4 => *images = vec![image("translucent", 1, 1, vec![188, 0, 0, 128])],
            5 => {
                *images = vec![image(
                    "oversized",
                    oversized_width,
                    1,
                    vec![255; oversized_width as usize * 4],
                )]
            }
            _ => unreachable!(),
        }
        assert!(
            gpu.renderer.register_asset(21, &invalid).is_err(),
            "invalid case {case} accepted"
        );
    }
    let after = gpu.renderer.resource_diagnostics();
    assert_eq!(before.texture_count, after.texture_count);
    assert_eq!(before.texture_bytes, after.texture_bytes);
    assert_eq!(before.texture_creations, after.texture_creations);
    assert_eq!(before.uv_uploads, after.uv_uploads);
    let preserved = gpu.render(
        &RenderFrame {
            instances: vec![skin_draw(21, 1, 0.8)],
        },
        "invalid-material-keeps-skinned-image.png",
    );
    assert!(
        preserved == pixels,
        "failed registration changed the old skinned image"
    );

    gpu.renderer
        .register_asset(
            21,
            &RenderAsset::Skeleton3d {
                vertices: skin_quad(0.2, [1.0; 4], [0.0, 0.0, 1.0]),
                indices: vec![0, 1, 2, 0, 2, 3],
                joint_count: 1,
                images: vec![image("hole", 2, 1, vec![0, 0, 0, 0, 255, 0, 0, 255])],
                texture: Some(0),
                material: Some(Material3d {
                    alpha_mode: AlphaMode::Cutout,
                    ..Material3d::default()
                }),
            },
        )
        .unwrap();
    gpu.renderer
        .register_asset(
            22,
            &RenderAsset::Skeleton3d {
                vertices: skin_quad(0.0, [0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 1.0]),
                indices: vec![0, 1, 2, 0, 2, 3],
                joint_count: 1,
                images: Vec::new(),
                texture: None,
                material: None,
            },
        )
        .unwrap();
    // Draw the background second: an alpha-zero depth write would block it.
    let frame = RenderFrame {
        instances: vec![skin_draw(21, 1, 0.0), skin_draw(22, 2, 0.0)],
    };
    let pixels = gpu.render(&frame, "cutout-hole-leaves-depth-open.png");
    let hole = pixel_at(&pixels, [-0.75, 1.0]);
    let solid = pixel_at(&pixels, [0.75, 1.0]);
    assert_eq!(hole[3], 255);
    assert!(hole[2] > 60 && hole[0] < 5 && hole[1] < 5);
    assert!(solid[0] > solid[2] + 30 && solid[3] == 255);
}

#[test]
fn texture_lifecycle_shares_images_and_failed_replacements_preserve_pixels() {
    let mut gpu = GpuFixture::new();
    let layers = vec![textured_quad(), textured_quad()];
    let paint = image("shared", 1, 1, vec![255, 255, 255, 255]);
    let asset = RenderAsset::Sprite2d {
        layers: layers.clone(),
        images: vec![paint.clone()],
    };
    let baseline = gpu.renderer.resource_diagnostics();
    for _ in 0..100 {
        gpu.renderer.register_asset(31, &asset).unwrap();
        gpu.renderer.register_asset(31, &asset).unwrap();
        let state = gpu.renderer.resource_diagnostics();
        assert_eq!(state.texture_count, 1);
        assert_eq!(state.texture_bytes, 4);
        gpu.renderer.remove_asset(31);
        let state = gpu.renderer.resource_diagnostics();
        assert_eq!(state.texture_count, baseline.texture_count);
        assert_eq!(state.texture_bytes, baseline.texture_bytes);
    }
    gpu.renderer.register_asset(31, &asset).unwrap();
    let mut frame = sprite_frame(31, &layers);
    frame.instances.push(InstanceDraw::Sprite2d {
        asset: 31,
        instance: 2,
        transform: Mat4::from_translation(Vec3::new(0.2, 0.0, 0.0)),
        layers: layers.iter().map(|layer| layer.vertices.clone()).collect(),
    });
    let pixels = gpu.render(&frame, "shared-image-two-layers-two-instances.png");
    let old = gpu.renderer.resource_diagnostics();
    assert_eq!(old.texture_count, 1);
    let mut invalid = layers[0].clone();
    invalid.texture = Some(1);
    let bad_reference = RenderAsset::Sprite2d {
        layers: vec![invalid],
        images: vec![paint],
    };
    let bad_pixels = RenderAsset::Sprite2d {
        layers: vec![layers[0].clone()],
        images: vec![image("short", 2, 2, vec![0; 4])],
    };
    let mut short_uv = layers[0].clone();
    short_uv.uvs.pop();
    let bad_uv = RenderAsset::Sprite2d {
        layers: vec![short_uv],
        images: vec![image("white", 1, 1, vec![255; 4])],
    };
    let bad_flags = RenderAsset::Sprite2d {
        layers: vec![layers[0].clone()],
        images: vec![Arc::new(ModelImage {
            id: "false-flags".into(),
            width: 1,
            height: 1,
            rgba: vec![0; 4].into(),
            opaque: true,
            fully_transparent: false,
        })],
    };
    let bad_transparent_rgb = RenderAsset::Sprite2d {
        layers: vec![layers[0].clone()],
        images: vec![image("non-premultiplied", 1, 1, vec![255, 0, 0, 0])],
    };
    for bad in [
        bad_reference,
        bad_pixels,
        bad_uv,
        bad_flags,
        bad_transparent_rgb,
    ] {
        assert!(gpu.renderer.register_asset(31, &bad).is_err());
        let state = gpu.renderer.resource_diagnostics();
        assert_eq!(state.texture_bytes, old.texture_bytes);
        assert_eq!(state.texture_count, old.texture_count);
        assert_eq!(state.asset_count, old.asset_count);
        assert_eq!(
            gpu.render(&frame, "failed-texture-replacement-preserves-old.png"),
            pixels
        );
    }
    let stable = gpu.renderer.resource_diagnostics();
    assert_eq!(stable.texture_creations, old.texture_creations);
    assert_eq!(stable.uv_uploads, old.uv_uploads);
    gpu.renderer.remove_asset(31);
    assert_eq!(
        gpu.renderer.resource_diagnostics().texture_bytes,
        baseline.texture_bytes
    );
}

#[test]
fn texture_budget_includes_old_resources_during_same_id_replacement() {
    let mut gpu = GpuFixture::new();
    let rgba: Arc<[u8]> = vec![255; 2048 * 2048 * 4].into();
    let images = ["bulk-a", "bulk-b"]
        .map(|id| {
            Arc::new(ModelImage {
                id: id.into(),
                width: 2048,
                height: 2048,
                rgba: rgba.clone(),
                opaque: true,
                fully_transparent: false,
            })
        })
        .to_vec();
    let mut second = textured_quad();
    second.texture = Some(1);
    let layers = vec![textured_quad(), second];
    let asset = RenderAsset::Sprite2d {
        layers: layers.clone(),
        images,
    };
    for id in 41..45 {
        gpu.renderer.register_asset(id, &asset).unwrap();
    }
    let full = gpu.renderer.resource_diagnostics();
    assert_eq!(full.texture_bytes, 128 * 1024 * 1024);
    assert_eq!(full.texture_count, 8);
    let frame = sprite_frame(41, &layers);
    let before = gpu.render(&frame, "full-texture-budget-before.png");
    let mut candidate_layer = textured_quad();
    candidate_layer.color = [1.0, 0.0, 0.0, 1.0];
    let candidate = RenderAsset::Sprite2d {
        layers: vec![candidate_layer],
        images: vec![image("candidate", 1, 1, vec![255; 4])],
    };
    assert!(gpu.renderer.register_asset(41, &candidate).is_err());
    assert_eq!(
        gpu.renderer.resource_diagnostics().texture_creations,
        full.texture_creations
    );
    assert_eq!(
        gpu.renderer.resource_diagnostics().texture_bytes,
        full.texture_bytes
    );
    assert_eq!(
        gpu.render(&frame, "full-texture-budget-failure-keeps-old.png"),
        before
    );
    gpu.renderer.remove_asset(44);
    gpu.renderer.register_asset(41, &candidate).unwrap();
    for id in 41..45 {
        gpu.renderer.remove_asset(id);
    }
    assert_eq!(gpu.renderer.resource_diagnostics().texture_bytes, 0);
    assert_eq!(gpu.renderer.resource_diagnostics().texture_count, 0);
}

#[test]
fn repeated_sprite_reload_and_removal_do_not_grow_an_index_arena() {
    let mut gpu = GpuFixture::with_limits(wgpu::Limits {
        max_buffer_size: 256 * 1024,
        ..Default::default()
    });
    let mut layer = quad([0.0, 1.0, 0.0, 1.0], None);
    layer.indices = layer.indices.repeat(6000);
    let layers = vec![layer];
    let asset = RenderAsset::Sprite2d {
        images: Vec::new(),
        layers: layers.clone(),
    };
    let baseline = gpu.renderer.resource_diagnostics();
    assert_eq!(baseline.asset_count, 0);
    for _ in 0..8 {
        gpu.renderer.register_asset(2, &asset).unwrap();
        assert_eq!(gpu.renderer.resource_diagnostics().asset_count, 1);
        gpu.renderer.register_asset(2, &asset).unwrap();
        assert_eq!(gpu.renderer.resource_diagnostics().asset_count, 1);
        gpu.renderer.register_asset(3, &asset).unwrap();
        assert_eq!(gpu.renderer.resource_diagnostics().asset_count, 2);
        gpu.renderer.remove_asset(2);
        gpu.renderer.remove_asset(3);
        assert_eq!(gpu.renderer.resource_diagnostics().asset_count, 0);
        assert_eq!(
            gpu.renderer.resource_diagnostics().mask_bytes,
            baseline.mask_bytes
        );
    }
    gpu.renderer.register_asset(2, &asset).unwrap();
    let pixels = gpu.render(
        &sprite_frame(2, &layers),
        "after-repeated-index-reloads.png",
    );
    assert_pixel(pixel_at(&pixels, [0.0, 1.0]), [0, 255, 0, 255]);
    let rendered = gpu.renderer.resource_diagnostics();
    assert_eq!(rendered.asset_count, 1);
    assert!(rendered.sprite_vertex_capacity_bytes > 0);
    gpu.renderer.remove_asset(2);
    let removed = gpu.renderer.resource_diagnostics();
    assert_eq!(removed.asset_count, 0);
    assert_eq!(
        removed.sprite_vertex_capacity_bytes,
        rendered.sprite_vertex_capacity_bytes
    );
    let view = gpu
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    assert!(matches!(
        gpu.renderer
            .render_to(&sprite_frame(2, &layers), &view, [SIZE, SIZE], &mut encoder),
        Err(delicium::renderer::RenderError::UnknownAsset { asset: 2 })
    ));
}
