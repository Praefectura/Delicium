use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use delicium::renderer::{
    InstanceDraw, LayerAsset2d, RenderAsset, RenderFrame, SceneRenderer, SkinVertex,
};
use glam::Mat4;

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
        });
    gpu.renderer
        .register_asset(
            3,
            &RenderAsset::Skeleton3d {
                vertices: vertices.to_vec(),
                indices: vec![0, 1, 2],
                joint_count: 1,
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
                layers: layers.clone(),
            },
        )
        .unwrap();
    let frame = sprite_frame(1, &layers);
    let before = gpu.render(&frame, "before-rejected-gpu-replacement.png");
    let mut oversized = quad([0.0, 1.0, 0.0, 1.0], None);
    // The 256 KiB limit permits readback but rejects this 288,000-byte buffer.
    oversized.indices = oversized.indices.repeat(12000);
    let result = gpu.renderer.register_asset(
        1,
        &RenderAsset::Sprite2d {
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
    assert_eq!(
        after, before,
        "Rejected GPU resources must not replace the old asset or mask"
    );
    assert_pixel(pixel_at(&after, [0.0, 1.0]), [255, 0, 0, 255]);
    assert_pixel(pixel_at(&after, [0.7, 1.0]), [0, 0, 0, 0]);
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
        layers: layers.clone(),
    };
    for _ in 0..8 {
        gpu.renderer.register_asset(2, &asset).unwrap();
        gpu.renderer.register_asset(2, &asset).unwrap();
        gpu.renderer.register_asset(3, &asset).unwrap();
        gpu.renderer.remove_asset(2);
        gpu.renderer.remove_asset(3);
    }
    gpu.renderer.register_asset(2, &asset).unwrap();
    let pixels = gpu.render(
        &sprite_frame(2, &layers),
        "after-repeated-index-reloads.png",
    );
    assert_pixel(pixel_at(&pixels, [0.0, 1.0]), [0, 255, 0, 255]);
    gpu.renderer.remove_asset(2);
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
