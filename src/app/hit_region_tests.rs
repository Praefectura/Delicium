use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use glam::{Mat4, Vec3};
use serde_json::json;

use super::{
    HitGeometry, InstanceOutline, clip_depth, clip_to_client, pad_polygon, push_triangle,
    region_polygon,
};
use crate::platform::{ProbedPlan, probe_region_build, probe_region_equivalence};
use crate::renderer::InstanceDraw;

use crate::{
    model::{LoadLimits, ModelKind, load_model_bytes},
    renderer::{RenderFrame, SceneRenderer},
    simulation::{Simulation, render_asset},
};

fn sprite_scene(vertices: &[[f32; 2]], indices: &[u32], alpha: f32) -> (Simulation, RenderFrame) {
    let document = json!({
        "format": "delicium.model", "format_version": "0.1", "name": "Region fixture",
        "kind": "sprite2d", "sprite2d": {
            "parameters": [], "layers": [{
                "name": "outline", "color": [1.0, 0.5, 0.2, alpha],
                "vertices": vertices, "indices": indices
            }]
        }
    });
    let model = load_model_bytes(
        &serde_json::to_vec(&document).unwrap(),
        &LoadLimits::default(),
    )
    .unwrap();
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(model).unwrap();
    simulation.spawn(asset, Vec3::ZERO).unwrap();
    let frame = simulation.render_frame().unwrap();
    (simulation, frame)
}

fn translated(frame: &RenderFrame, dx_pixels: f32) -> RenderFrame {
    let mut frame = frame.clone();
    for instance in &mut frame.instances {
        let transform = match instance {
            InstanceDraw::Sprite2d { transform, .. }
            | InstanceDraw::Skeleton3d { transform, .. } => transform,
        };
        *transform =
            Mat4::from_translation(Vec3::new(dx_pixels * 4.5 / 640.0, 0.0, 0.0)) * *transform;
    }
    frame
}

#[test]
fn triangle_union_preserves_nonconvex_gap() {
    let screen = [
        [100.0, 100.0],
        [120.0, 100.0],
        [120.0, 220.0],
        [100.0, 220.0],
        [200.0, 100.0],
        [220.0, 100.0],
        [220.0, 220.0],
        [200.0, 220.0],
        [120.0, 200.0],
        [200.0, 200.0],
        [200.0, 220.0],
        [120.0, 220.0],
    ];
    let vertices: Vec<_> = screen
        .iter()
        .map(|p| crate::renderer::screen_to_world(*p, CoverageGpu::SIZE))
        .collect();
    let indices = [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11];
    let (simulation, frame) = sprite_scene(&vertices, &indices, 1.0);
    let geometry = HitGeometry::from_frame(&simulation, &frame, CoverageGpu::SIZE).unwrap();
    assert_eq!(
        crate::platform::hit_region_contains(
            geometry.polygons(),
            &[[110, 150], [160, 150], [210, 150], [160, 210]]
        )
        .unwrap(),
        [true, false, true, true]
    );
}

#[test]
fn thin_triangle_has_uniform_padding_and_zero_area_is_ignored() {
    let thin = [[100.0, 100.0], [800.0, 100.0], [100.0, 100.02]];
    let padded = region_polygon(&pad_polygon(&thin, super::HIT_PADDING_PX), 960.0, 640.0);
    assert_eq!(
        crate::platform::hit_region_contains(&[padded], &[[450, 107], [450, 93], [450, 112]])
            .unwrap(),
        [true, true, false]
    );
    let mut polygons = Vec::new();
    push_triangle(
        &mut polygons,
        [[-1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
        CoverageGpu::SIZE,
        crate::renderer::LEGACY,
        960.0,
        640.0,
    )
    .unwrap();
    assert!(polygons.is_empty());
}

#[test]
fn cache_measures_rotation_and_accumulated_motion_from_successful_baseline() {
    let (simulation, frame) = sprite_scene(&[[-0.8, 0.0], [0.8, 0.0], [0.0, 1.2]], &[0, 1, 2], 1.0);
    let baseline = HitGeometry::from_frame(&simulation, &frame, CoverageGpu::SIZE).unwrap();
    for dx in [1.0, 2.0, 3.9] {
        let current =
            HitGeometry::from_frame(&simulation, &translated(&frame, dx), CoverageGpu::SIZE)
                .unwrap();
        assert!(
            !current.changed_since(&baseline),
            "{dx}px should reuse the successful baseline"
        );
    }
    let moved =
        HitGeometry::from_frame(&simulation, &translated(&frame, 4.2), CoverageGpu::SIZE).unwrap();
    assert!(moved.changed_since(&baseline));
    let next =
        HitGeometry::from_frame(&simulation, &translated(&frame, 7.8), CoverageGpu::SIZE).unwrap();
    assert!(!next.changed_since(&moved));
    let mut rotated = frame.clone();
    if let InstanceDraw::Sprite2d { transform, .. } = &mut rotated.instances[0] {
        *transform = Mat4::from_translation(Vec3::new(0.0, 0.4, 0.0))
            * Mat4::from_rotation_z(0.2)
            * Mat4::from_translation(Vec3::new(0.0, -0.4, 0.0));
    }
    let rotated = HitGeometry::from_frame(&simulation, &rotated, CoverageGpu::SIZE).unwrap();
    assert!(
        rotated.changed_since(&baseline),
        "Rotation around the centroid must rebuild"
    );
}

#[test]
fn cache_invalidates_identity_size_and_topology_changes() {
    let outline = || InstanceOutline {
        asset: 1,
        instance: 1,
        polys: vec![vec![[1.0, 1.0], [20.0, 1.0], [1.0, 20.0]]],
    };
    let baseline = outline();
    for field in 0..4 {
        let mut changed = outline();
        match field {
            0 => changed.asset = 2,
            1 => changed.instance = 2,
            2 => changed.polys.push(changed.polys[0].clone()),
            _ => changed.polys[0].push([2.0, 2.0]),
        }
        assert!(changed.changed_since(&baseline));
    }
    let (simulation, frame) = sprite_scene(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], &[0, 1, 2], 1.0);
    let original = HitGeometry::from_frame(&simulation, &frame, CoverageGpu::SIZE).unwrap();
    assert!(
        HitGeometry::from_frame(&simulation, &frame, [960, 641])
            .unwrap()
            .changed_since(&original)
    );
    assert!(
        HitGeometry::from_frame(&simulation, &RenderFrame::default(), CoverageGpu::SIZE)
            .unwrap()
            .changed_since(&original)
    );
}

#[test]
fn cache_invalidates_scene_view_changes_and_projects_with_the_given_view() {
    let (simulation, frame) = sprite_scene(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], &[0, 1, 2], 1.0);
    let legacy = HitGeometry::from_frame(&simulation, &frame, CoverageGpu::SIZE).unwrap();
    let desktop = HitGeometry::from_frame_with_view(
        &simulation,
        &frame,
        CoverageGpu::SIZE,
        crate::renderer::desktop_view(960, 1.0),
    )
    .unwrap();
    assert!(
        desktop.changed_since(&legacy),
        "A different scene view must rebuild the native region"
    );
    // The two views project the same instance differently: with the desktop
    // view (span 6.75 at 960 physical height) the unit square sits lower and
    // narrower on screen than under the legacy span 4.5.
    assert_ne!(legacy.instances[0].polys, desktop.instances[0].polys);
    // The same view on both sides reuses the cache.
    let repeat = HitGeometry::from_frame_with_view(
        &simulation,
        &frame,
        CoverageGpu::SIZE,
        crate::renderer::desktop_view(960, 1.0),
    )
    .unwrap();
    assert!(!repeat.changed_since(&desktop));
    // A scale change on the same physical height rebuilds: the view differs.
    let scaled = HitGeometry::from_frame_with_view(
        &simulation,
        &frame,
        CoverageGpu::SIZE,
        crate::renderer::desktop_view(960, 1.5),
    )
    .unwrap();
    assert!(scaled.changed_since(&desktop));
}

#[test]
fn depth_and_viewport_clipping_preserve_true_intersections() {
    for z in [10.0, -91.0] {
        assert!(clip_depth(&[[-1.0, 0.0, z], [1.0, 0.0, z], [0.0, 1.0, z]]).is_none());
    }
    let clipped = clip_depth(&[[-1.0, 0.0, 9.0], [1.0, 0.0, 11.0], [0.0, 1.0, 9.0]]).unwrap();
    assert_eq!(clipped.len(), 4);
    assert!(
        clipped
            .iter()
            .all(|point| point[2] <= super::DEPTH_NEAR_Z && point[2] >= super::DEPTH_FAR_Z)
    );
    let projected = clip_to_client(&[[-20.0, 0.0], [20.0, -20.0], [20.0, 20.0]], 100.0, 100.0);
    let region = region_polygon(&projected, 100.0, 100.0);
    assert_eq!(
        crate::platform::hit_region_contains(&[region], &[[2, 8], [2, 15], [22, 5]]).unwrap(),
        [true, false, false]
    );
}

#[test]
fn invisible_empty_and_malformed_frames_do_not_silently_intercept() {
    let (simulation, frame) = sprite_scene(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], &[0, 1, 2], 0.0);
    assert!(
        HitGeometry::from_frame(&simulation, &frame, CoverageGpu::SIZE)
            .unwrap()
            .polygons()
            .is_empty()
    );
    for size in [[0, 640], [960, 0], CoverageGpu::SIZE] {
        assert!(
            HitGeometry::from_frame(&simulation, &RenderFrame::default(), size)
                .unwrap()
                .polygons()
                .is_empty()
        );
    }
    let (simulation, frame) = sprite_scene(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], &[0, 1, 2], 1.0);
    for case in 0..4 {
        let mut broken = frame.clone();
        if let InstanceDraw::Sprite2d { asset, layers, .. } = &mut broken.instances[0] {
            match case {
                0 => *asset = 99,
                1 => layers.clear(),
                2 => {
                    layers[0].pop();
                }
                _ => layers[0][0][0] = f32::NAN,
            }
        }
        assert!(HitGeometry::from_frame(&simulation, &broken, CoverageGpu::SIZE).is_err());
    }
    let simulation = animated_scene();
    let mut broken = simulation.render_frame().unwrap();
    if let InstanceDraw::Skeleton3d { skin_matrices, .. } = &mut broken.instances[2] {
        skin_matrices.clear();
    }
    assert!(HitGeometry::from_frame(&simulation, &broken, CoverageGpu::SIZE).is_err());
}

#[test]
fn wholly_transparent_textures_and_discarded_materials_do_not_intercept() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255, 0, 0, 0]).unwrap();
        writer.finish().unwrap();
    }
    let sprite = json!({
        "format": "delicium.model", "format_version": "0.2", "name": "Transparent paint",
        "kind": "sprite2d", "images": [{"id": "hidden", "png_base64": STANDARD.encode(bytes)}],
        "sprite2d": {"parameters": [], "layers": [{
            "name": "outline", "color": [1, 1, 1, 1],
            "vertices": [[0, 0], [1, 0], [0, 1]], "indices": [0, 1, 2],
            "texture": "hidden", "uvs": [[0, 1], [1, 1], [0, 0]]
        }]}
    });
    let mut skeleton: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../assets/examples/robot3d.dmodel.json"))
            .unwrap();
    skeleton["format_version"] = json!("0.2");
    skeleton["skeleton3d"]["material"] = json!({"alpha_mode": "cutout", "alpha_cutoff": 0.5});
    for vertex in skeleton["skeleton3d"]["vertices"].as_array_mut().unwrap() {
        vertex["color"][3] = json!(0.25);
    }
    for document in [sprite, skeleton] {
        let model = load_model_bytes(
            &serde_json::to_vec(&document).unwrap(),
            &LoadLimits::default(),
        )
        .unwrap();
        let mut simulation = Simulation::new();
        let asset = simulation.insert_asset(model).unwrap();
        simulation.spawn(asset, Vec3::ZERO).unwrap();
        let frame = simulation.render_frame().unwrap();
        assert!(
            HitGeometry::from_frame(&simulation, &frame, CoverageGpu::SIZE)
                .unwrap()
                .polygons()
                .is_empty()
        );
    }
}

#[test]
fn frozen_gpu_frames_are_fully_covered_by_current_and_cached_native_regions() {
    let mut simulation = animated_scene();
    let mut gpu = CoverageGpu::new(&simulation);
    let mut installed: Option<HitGeometry> = None;
    let mut reused = 0;
    let mut rebuilt = 0;
    let mut samples = 0;
    for pose in 0..=180 {
        if pose != 0 {
            simulation
                .advance(Duration::from_nanos(16_666_667))
                .unwrap();
        }
        if pose > 12 && pose % 15 != 0 {
            continue;
        }
        let frame = simulation.render_frame().unwrap();
        let current = HitGeometry::from_frame(&simulation, &frame, CoverageGpu::SIZE).unwrap();
        if installed
            .as_ref()
            .is_none_or(|previous| current.changed_since(previous))
        {
            // Validate the real native region before treating this build as installed.
            crate::platform::hit_region_contains(current.polygons(), &[]).unwrap();
            assert!(
                crate::platform::hit_region_matches_sequential_oracle(current.polygons()).unwrap(),
                "Native region differs from the sequential oracle at pose {pose}"
            );
            installed = Some(current);
            rebuilt += 1;
        } else {
            reused += 1;
        }
        assert_gpu_coverage(
            &mut gpu,
            &frame,
            installed.as_ref().unwrap().polygons(),
            pose,
        );
        samples += 1;
    }
    assert!(
        reused > 0 && rebuilt > 1,
        "Coverage must exercise both cache paths"
    );
    let mut frame = simulation.render_frame().unwrap();
    let baseline = HitGeometry::from_frame(&simulation, &frame, CoverageGpu::SIZE).unwrap();
    let edge_frame = translated(&frame, 3.9);
    let edge_geometry =
        HitGeometry::from_frame(&simulation, &edge_frame, CoverageGpu::SIZE).unwrap();
    assert!(
        edge_geometry.changed_since(&baseline),
        "Crossing the client boundary changes clipped topology and must rebuild"
    );
    assert_gpu_coverage(&mut gpu, &edge_frame, edge_geometry.polygons(), 181);
    // Keep the rightmost actor away from the client edge for the cache-only
    // drift probes; a clipping topology change legitimately forces a rebuild.
    if let InstanceDraw::Skeleton3d { transform, .. } = frame.instances.last_mut().unwrap() {
        *transform = Mat4::from_translation(Vec3::new(-0.25, 0.0, 0.0)) * *transform;
    }
    let baseline = HitGeometry::from_frame(&simulation, &frame, CoverageGpu::SIZE).unwrap();
    for (index, dx) in [1.0, 2.5, 3.9].into_iter().enumerate() {
        let frame = translated(&frame, dx);
        let current = HitGeometry::from_frame(&simulation, &frame, CoverageGpu::SIZE).unwrap();
        assert!(
            !current.changed_since(&baseline),
            "Drift {dx}px must reuse the successful baseline when clipping topology is stable"
        );
        assert_gpu_coverage(&mut gpu, &frame, baseline.polygons(), 182 + index);
    }

    // Exercise the negative-scale path on one 2D and one 3D instance across
    // several animation poses. The GPU and hit-region paths must consume the
    // same mirrored transforms, including the reversed triangle winding.
    for pose in 0..7 {
        if pose != 0 {
            simulation
                .advance(Duration::from_nanos(16_666_667))
                .unwrap();
        }
        let mut mirrored = simulation.render_frame().unwrap();
        let mut mirrored_2d = false;
        let mut mirrored_3d = false;
        for instance in &mut mirrored.instances {
            match instance {
                InstanceDraw::Sprite2d { transform, .. } if !mirrored_2d => {
                    *transform *= Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
                    mirrored_2d = true;
                }
                InstanceDraw::Skeleton3d { transform, .. } if !mirrored_3d => {
                    *transform *= Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
                    mirrored_3d = true;
                }
                _ => {}
            }
        }
        assert!(
            mirrored_2d && mirrored_3d,
            "animated scene must contain both model kinds"
        );
        let geometry = HitGeometry::from_frame(&simulation, &mirrored, CoverageGpu::SIZE).unwrap();
        assert_gpu_coverage(&mut gpu, &mirrored, geometry.polygons(), 185 + pose);
    }
    println!(
        "REGION_COVERAGE pass=true animated_samples={samples} reused={reused} rebuilt={rebuilt} cached_drift_samples=3 mirrored_gpu_samples=7"
    );
}

fn animated_scene() -> Simulation {
    let mut simulation = Simulation::new();
    for (bytes, positions) in [
        (
            include_bytes!("../../assets/examples/puppet2d.dmodel.json").as_slice(),
            [-2.4, -0.8],
        ),
        (
            include_bytes!("../../assets/examples/robot3d.dmodel.json").as_slice(),
            [0.8, 2.4],
        ),
    ] {
        let model = load_model_bytes(bytes, &LoadLimits::default()).unwrap();
        let asset = simulation.insert_asset(model).unwrap();
        for x in positions {
            simulation.spawn(asset, Vec3::new(x, 0.15, 0.0)).unwrap();
        }
    }
    simulation
}

fn assert_gpu_coverage(
    gpu: &mut CoverageGpu,
    frame: &RenderFrame,
    polygons: &[Vec<[i32; 2]>],
    pose: usize,
) {
    let points = gpu.visible_pixels(frame);
    assert!(
        points.len() > 1000,
        "Pose {pose} must contain substantial rendered character pixels"
    );
    let covered = crate::platform::hit_region_contains(polygons, &points).unwrap();
    if let Some(index) = covered.iter().position(|value| !value) {
        panic!(
            "Pose {pose}: GPU-visible pixel {:?} is outside the cached native region",
            points[index]
        );
    }
}

struct CoverageGpu {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    renderer: SceneRenderer,
    texture: wgpu::Texture,
}

impl CoverageGpu {
    const SIZE: [u32; 2] = [960, 640];
    const ROW_BYTES: u32 = Self::SIZE[0] * 4;

    fn new(simulation: &Simulation) -> Self {
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
        .expect("Region coverage requires a hardware GPU adapter");
        let info = adapter.get_info();
        assert_ne!(
            info.device_type,
            wgpu::DeviceType::Cpu,
            "Software rendering is not a hardware coverage pass"
        );
        println!("Frozen-frame region coverage adapter: {info:?}");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Frozen-frame region coverage device"),
            ..Default::default()
        }))
        .unwrap();
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let mut renderer = SceneRenderer::new(
            device.clone(),
            queue.clone(),
            wgpu::TextureFormat::Rgba8Unorm,
        )
        .unwrap();
        for asset in [1, 2] {
            renderer
                .register_asset(asset, &render_asset(simulation.asset(asset).unwrap()))
                .unwrap();
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Frozen-frame region coverage target"),
            size: wgpu::Extent3d {
                width: Self::SIZE[0],
                height: Self::SIZE[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Self {
            device,
            queue,
            renderer,
            texture,
        }
    }

    fn visible_pixels(&mut self, frame: &RenderFrame) -> Vec<[i32; 2]> {
        let view = self
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Frozen-frame region coverage readback"),
            size: u64::from(Self::ROW_BYTES) * u64::from(Self::SIZE[1]),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.renderer
            .render_to(frame, &view, Self::SIZE, &mut encoder)
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
                    bytes_per_row: Some(Self::ROW_BYTES),
                    rows_per_image: Some(Self::SIZE[1]),
                },
            },
            wgpu::Extent3d {
                width: Self::SIZE[0],
                height: Self::SIZE[1],
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
            .unwrap();
        receive
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap();
        let points = {
            let bytes = readback.slice(..).get_mapped_range().unwrap();
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .filter_map(|(pixel, rgba)| {
                    (rgba[3] > 0).then_some([
                        (pixel as u32 % Self::SIZE[0]) as i32,
                        (pixel as u32 / Self::SIZE[0]) as i32,
                    ])
                })
                .collect()
        };
        readback.unmap();
        assert!(
            pollster::block_on(scope.pop()).is_none(),
            "Coverage render produced a GPU validation error"
        );
        points
    }
}

fn mirror_frame(frame: &mut RenderFrame) {
    for instance in &mut frame.instances {
        let transform = match instance {
            InstanceDraw::Sprite2d { transform, .. }
            | InstanceDraw::Skeleton3d { transform, .. } => transform,
        };
        *transform *= Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
    }
}

/// A mirror flip must invalidate the region cache and project the outline as
/// the horizontal mirror image of the baseline around the instance's own
/// vertical centre line, so the native region still covers exactly what is
/// drawn.
#[test]
fn mirror_flip_rebuilds_the_region_and_projects_the_mirrored_outline() {
    // An asymmetric fixture: the mirrored outline cannot coincide with the
    // original, so a mirrored region proves the flip was really projected.
    let (simulation, frame) = sprite_scene(
        &[[-0.6, 0.0], [0.6, 0.0], [0.0, 1.0], [-0.2, 0.4]],
        &[0, 1, 2, 1, 3, 2],
        1.0,
    );
    let size = [640u32, 640u32];
    let baseline = HitGeometry::from_frame(&simulation, &frame, size).unwrap();
    let baseline_polygons = baseline.polygons().to_vec();
    assert!(
        !baseline_polygons.is_empty(),
        "baseline region has no polygons"
    );
    let mut mirrored = frame.clone();
    mirror_frame(&mut mirrored);
    let geometry = HitGeometry::from_frame(&simulation, &mirrored, size).unwrap();
    assert!(
        geometry.changed_since(&baseline),
        "mirrored frame reused the baseline region"
    );
    let polygons = geometry.polygons();
    assert_eq!(
        polygons.len(),
        baseline_polygons.len(),
        "mirroring must not change the polygon count"
    );
    // The fixture spawns at the world origin, so its mirror axis is the
    // projected origin; every baseline vertex must reappear reflected about
    // that axis, and nothing else may appear.
    let axis_x =
        crate::renderer::world_to_screen_with([0.0, 0.0], size, crate::renderer::LEGACY)[0];
    let mut unmatched: Vec<[i32; 2]> = polygons
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    for point in baseline_polygons.iter().flatten() {
        let reflected = [2.0 * axis_x - point[0] as f32, point[1] as f32];
        let Some(index) = unmatched.iter().position(|candidate| {
            (candidate[0] as f32 - reflected[0]).abs() <= 1.5
                && (candidate[1] as f32 - reflected[1]).abs() <= 1.5
        }) else {
            panic!("baseline vertex {point:?} has no mirrored counterpart near {reflected:?}");
        };
        unmatched.swap_remove(index);
    }
    assert!(
        unmatched.is_empty(),
        "mirrored region has vertices with no baseline counterpart: {unmatched:?}"
    );
}

/// The benchmark launcher's scene: half the instances drawn from the 2D
/// puppet and half from the 3D robot, on the bench grid at the launcher's
/// positions (instance ids ascending, phases 0.23s apart, stationary).
fn bench_scene(count: usize) -> Simulation {
    let mut simulation = Simulation::new();
    for (bytes, kind) in [
        (
            include_bytes!("../../assets/examples/puppet2d.dmodel.json").as_slice(),
            ModelKind::Sprite2d,
        ),
        (
            include_bytes!("../../assets/examples/robot3d.dmodel.json").as_slice(),
            ModelKind::Skeleton3d,
        ),
    ] {
        let model = load_model_bytes(bytes, &LoadLimits::default()).unwrap();
        let asset = simulation.insert_asset(model).unwrap();
        for position in crate::app::benchmark::positions(count, kind) {
            simulation.spawn(asset, position).unwrap();
        }
    }
    simulation
}

/// A winding rectangle corner case for the adversarial equivalence matrix.
fn probe_rectangle(x: i32, y: i32, width: i32, height: i32) -> Vec<[i32; 2]> {
    vec![
        [x, y],
        [x + width, y],
        [x + width, y + height],
        [x, y + height],
    ]
}

/// The GDI coordinate range: coordinates are signed 27-bit values. A
/// multi-polygon batch additionally rejects a y spread >= 2^24 (measured by
/// the platform batch probe), which is why the two corners stay in separate
/// adversarial cases.
const PROBE_GDI_MIN_COORD: i32 = -(1 << 26);
const PROBE_GDI_MAX_COORD: i32 = (1 << 26) - 1;

/// Phase-0 decision-gate probe for batched region installs: fixed chunks and
/// per-instance groups through `CreatePolyPolygonRgn`, streamed into the
/// same carry-slot fold as the previous per-polygon production path.
///
/// Outcome (2026-10-16, evidence `.artifacts/perf-perbatch-20261016-015856/`):
/// ADOPT — 41-polygon chunks measured 0.332x the per-polygon fold's paired
/// median (50.6ms -> 16.8ms p50 at 10512 polygons) with exact equivalence,
/// beating the 82/164/328 arms (0.370x/0.473x/0.694x) and per-instance
/// (0.599x), and became the production `REGION_CHUNK_POLYGONS`; this probe
/// stays as the real-geometry reproduction harness. See also the synthetic
/// `poly_poly_batch_probe` in `src/platform/mod.rs`.
///
/// Fixture: the benchmark launcher's 64-instance scene (32 sprite2d + 32
/// skeleton3d on the bench grid) projected onto the archived c64 physical
/// size (1440x960) under the frozen LEGACY view; pure CPU, no window and no
/// GPU. The decision frame is the sampled frame with the most polygons.
///
/// Run explicitly, in release, with output:
/// `cargo test --release --locked --offline --lib region_batch_gate_probe -- --ignored --nocapture --test-threads=1`
///
/// Hard assertions: exact region equivalence against the independent
/// sequential oracle (decision frame under every arm, adversarial matrix,
/// empty input, invalid polygons), fail-closed errors, exact native-call
/// counts per plan, and region-ownership accounting (no leaks on any path).
/// Timing is printed for the gate decision made outside the test; evidence
/// lives in `.artifacts/perf-perbatch-20261016-015856/`.
#[test]
#[ignore = "phase-0 decision gate; run explicitly with --ignored"]
fn region_batch_gate_probe() {
    const SIZE: [u32; 2] = [1440, 960];
    const ROUNDS: usize = 21;
    const SECONDARY_ROUNDS: usize = 7;

    fn ms(elapsed: Duration) -> f64 {
        elapsed.as_secs_f64() * 1000.0
    }

    fn timed_build(polygons: &[Vec<[i32; 2]>], plan: ProbedPlan<'_>) -> f64 {
        let start = Instant::now();
        probe_region_build(polygons, plan).unwrap();
        ms(start.elapsed())
    }

    fn percentiles(samples: &mut [f64]) -> (f64, f64) {
        samples.sort_by(f64::total_cmp);
        let p50 = samples[samples.len() / 2];
        let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
        (p50, p95)
    }

    fn median(samples: &mut [f64]) -> f64 {
        samples.sort_by(f64::total_cmp);
        let middle = samples.len() / 2;
        if samples.len().is_multiple_of(2) {
            (samples[middle - 1] + samples[middle]) / 2.0
        } else {
            samples[middle]
        }
    }

    /// Frames `count` geometries `interval` frames apart, oldest first.
    fn sample_geometries(
        simulation: &mut Simulation,
        label: &str,
        count: usize,
        interval: usize,
        size: [u32; 2],
    ) -> Vec<HitGeometry> {
        let mut samples = Vec::with_capacity(count);
        for step in 0..count {
            if step != 0 {
                for _ in 0..interval {
                    simulation
                        .advance(Duration::from_nanos(16_666_667))
                        .unwrap();
                }
            }
            let frame = simulation.render_frame().unwrap();
            let geometry = HitGeometry::from_frame(simulation, &frame, size).unwrap();
            println!(
                "PROBE {label} sample={step} polygons={} points={}",
                geometry.polygons().len(),
                geometry.polygons().iter().map(Vec::len).sum::<usize>()
            );
            samples.push(geometry);
        }
        samples
    }

    fn decision_index(samples: &[HitGeometry]) -> usize {
        samples
            .iter()
            .enumerate()
            .max_by_key(|(_, geometry)| geometry.polygons().len())
            .map(|(index, _)| index)
            .unwrap()
    }

    /// Polygons contributed by each instance, from the per-instance ends.
    fn instance_polygon_counts(geometry: &HitGeometry) -> Vec<usize> {
        let mut counts = Vec::new();
        let mut previous = 0;
        for &end in geometry.instance_polygon_ends() {
            counts.push(end - previous);
            previous = end;
        }
        counts
    }

    /// `rounds` timing rounds over the arms, with the arm order rotated by
    /// the round (each round starts at the next arm) and one warmup per arm;
    /// returns each arm's samples and the per-round paired ratios against
    /// arm 0.
    fn run_rounds(
        label: &str,
        polygons: &[Vec<[i32; 2]>],
        arms: &[(&str, ProbedPlan<'_>)],
        rounds: usize,
    ) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
        for (name, plan) in arms {
            timed_build(polygons, *plan);
            println!("PROBE {label} warmup arm={name} ok");
        }
        let mut arm_samples: Vec<Vec<f64>> = vec![Vec::with_capacity(rounds); arms.len()];
        let mut arm_ratios: Vec<Vec<f64>> = vec![Vec::with_capacity(rounds); arms.len()];
        for round in 0..rounds {
            let mut round_times = vec![0.0_f64; arms.len()];
            for position in 0..arms.len() {
                let arm = (round + position) % arms.len();
                round_times[arm] = timed_build(polygons, arms[arm].1);
            }
            for (slot, time) in arm_samples.iter_mut().zip(&round_times) {
                slot.push(*time);
            }
            for (slot, time) in arm_ratios.iter_mut().zip(&round_times).skip(1) {
                slot.push(time / round_times[0]);
            }
        }
        (arm_samples, arm_ratios)
    }

    fn report_arms(
        label: &str,
        arms: &[(&str, ProbedPlan<'_>)],
        arm_samples: &[Vec<f64>],
        arm_ratios: &[Vec<f64>],
    ) {
        let mut base = arm_samples[0].clone();
        let (base_p50, _) = percentiles(&mut base);
        for (index, (name, _)) in arms.iter().enumerate() {
            let mut times = arm_samples[index].clone();
            let (p50, p95) = percentiles(&mut times);
            let (paired, independent, drift) = if index == 0 {
                (1.0, 1.0, 0.0)
            } else {
                let paired = median(&mut arm_ratios[index].clone());
                let independent = p50 / base_p50;
                (
                    paired,
                    independent,
                    (paired - independent) / independent * 100.0,
                )
            };
            let marker = if drift.abs() > 5.0 { " DRIFT" } else { "" };
            println!(
                "PROBE {label} arm={name} samples={} p50_ms={p50:.3} p95_ms={p95:.3} paired_median={paired:.4} indep_ratio={independent:.4} drift={drift:+.1}%{marker}",
                arm_samples[index].len()
            );
        }
    }

    // -- primary fixture: the benchmark's 64-instance scene ---------------
    let mut simulation = bench_scene(64);
    let samples = sample_geometries(&mut simulation, "primary", 13, 30, SIZE);
    let decision_at = decision_index(&samples);
    let decision = &samples[decision_at];
    let polygons = decision.polygons();
    let polygon_count = polygons.len();
    let ends = decision.instance_polygon_ends();
    assert!(
        polygon_count >= 9800,
        "the decision frame must carry the full 64-instance load, got {polygon_count} polygons"
    );
    assert_eq!(ends.len(), 64, "one end offset per frame instance");
    assert!(
        ends.windows(2).all(|pair| pair[0] <= pair[1]),
        "per-instance end offsets must be non-decreasing"
    );
    assert_eq!(
        ends.last().copied().unwrap_or(0),
        polygon_count,
        "the last instance must end at the flattened polygon count"
    );
    println!(
        "PROBE primary decision sample={decision_at} polygons={polygon_count} points={} instances={}",
        polygons.iter().map(Vec::len).sum::<usize>(),
        ends.len()
    );
    {
        let mut counts = instance_polygon_counts(decision);
        let empty = counts.iter().filter(|&&count| count == 0).count();
        counts.sort_unstable();
        println!(
            "PROBE primary per_instance min={} p50={} max={} empty={empty}",
            counts.first().copied().unwrap_or(0),
            counts[counts.len() / 2],
            counts.last().copied().unwrap_or(0),
        );
    }
    {
        let mut groups = 0;
        let mut previous = 0;
        for &end in ends {
            if end > previous {
                groups += 1;
                previous = end;
            }
        }
        println!(
            "PROBE primary batches unified={polygon_count} chunk-41={} chunk-82={} chunk-164={} chunk-328={} per-instance={groups}",
            polygon_count.div_ceil(41),
            polygon_count.div_ceil(82),
            polygon_count.div_ceil(164),
            polygon_count.div_ceil(328),
        );
    }

    let arms: Vec<(&str, ProbedPlan<'_>)> = vec![
        ("unified", ProbedPlan::Unified),
        ("chunk-41", ProbedPlan::Chunked(41)),
        ("chunk-82", ProbedPlan::Chunked(82)),
        ("chunk-164", ProbedPlan::Chunked(164)),
        ("chunk-328", ProbedPlan::Chunked(328)),
        ("per-instance", ProbedPlan::PerInstance(ends)),
    ];
    let (arm_samples, arm_ratios) = run_rounds("primary", polygons, &arms, ROUNDS);
    report_arms("primary", &arms, &arm_samples, &arm_ratios);
    {
        // Diagnostic only: the batched path's per-batch leaf cost with no
        // unions, i.e. the floor the chunked arms build on.
        let plan = ProbedPlan::BuildOnlyChunked(164);
        timed_build(polygons, plan);
        let mut times: Vec<f64> = (0..9).map(|_| timed_build(polygons, plan)).collect();
        let (p50, p95) = percentiles(&mut times);
        println!(
            "PROBE primary diagnostic=build-only-chunk-164 batches={} samples={} p50_ms={p50:.3} p95_ms={p95:.3}",
            polygon_count.div_ceil(164),
            times.len()
        );
    }

    // -- secondary fixture: the 16-instance scene (print-only) ------------
    {
        let mut simulation = bench_scene(16);
        let samples = sample_geometries(&mut simulation, "secondary", 7, 30, SIZE);
        let decision_at = decision_index(&samples);
        let decision = &samples[decision_at];
        let polygons = decision.polygons();
        let ends = decision.instance_polygon_ends();
        println!(
            "PROBE secondary decision sample={decision_at} polygons={} instances={}",
            polygons.len(),
            ends.len()
        );
        let arms: Vec<(&str, ProbedPlan<'_>)> = vec![
            ("unified", ProbedPlan::Unified),
            ("chunk-41", ProbedPlan::Chunked(41)),
            ("chunk-82", ProbedPlan::Chunked(82)),
            ("chunk-164", ProbedPlan::Chunked(164)),
            ("chunk-328", ProbedPlan::Chunked(328)),
            ("per-instance", ProbedPlan::PerInstance(ends)),
        ];
        let (arm_samples, arm_ratios) = run_rounds("secondary", polygons, &arms, SECONDARY_ROUNDS);
        report_arms("secondary", &arms, &arm_samples, &arm_ratios);
    }

    // -- hard equivalence --------------------------------------------------
    for (name, plan) in &arms {
        assert!(
            probe_region_equivalence(polygons, *plan).unwrap(),
            "decision frame differs from the sequential oracle under arm {name}"
        );
    }
    println!("PROBE equivalence decision_frame=true arms={}", arms.len());

    let square = probe_rectangle(0, 0, 10, 10);
    let concave = vec![
        [0, 0],
        [10, 0],
        [10, 30],
        [30, 30],
        [30, 0],
        [40, 0],
        [40, 40],
        [0, 40],
    ];
    let mut mixed_winding = probe_rectangle(5, 5, 10, 10);
    mixed_winding.reverse();
    let mut cases: Vec<Vec<Vec<[i32; 2]>>> = vec![
        vec![],
        vec![square.clone()],
        vec![
            square.clone(),
            probe_rectangle(5, 5, 10, 10),
            probe_rectangle(10, 0, 10, 10),
        ],
        vec![concave, probe_rectangle(50, 50, 10, 10)],
        vec![
            vec![[0, 0], [1, 30], [0, 30]],
            vec![[1, 0], [2, 30], [1, 30]],
        ],
        vec![vec![[0, 0], [1, 1], [2, 2]], vec![[0, 0], [0, 0], [0, 0]]],
        vec![
            probe_rectangle(PROBE_GDI_MIN_COORD, PROBE_GDI_MIN_COORD, 16, 16),
            probe_rectangle(PROBE_GDI_MIN_COORD + 32, PROBE_GDI_MIN_COORD + 32, 16, 16),
        ],
        vec![
            probe_rectangle(PROBE_GDI_MAX_COORD - 16, PROBE_GDI_MAX_COORD - 16, 16, 16),
            probe_rectangle(PROBE_GDI_MAX_COORD - 32, PROBE_GDI_MAX_COORD - 32, 16, 16),
        ],
        vec![square.clone(), mixed_winding],
    ];
    let chunk_plans = [
        ("chunk-1", ProbedPlan::Chunked(1)),
        ("chunk-2", ProbedPlan::Chunked(2)),
        ("chunk-3", ProbedPlan::Chunked(3)),
        ("chunk-164", ProbedPlan::Chunked(164)),
    ];
    let arrangements = ["raw", "rotated", "reversed"];
    let mut failures: Vec<String> = Vec::new();
    for (case, polygons) in cases.iter_mut().enumerate() {
        for (arrangement, arrangement_name) in arrangements.iter().enumerate() {
            match arrangement {
                1 => {
                    if polygons.is_empty() {
                        continue;
                    }
                    polygons.rotate_left(1);
                }
                2 => {
                    polygons.reverse();
                    for polygon in polygons.iter_mut() {
                        polygon.reverse();
                    }
                }
                _ => {}
            }
            for (plan_name, plan) in &chunk_plans {
                match probe_region_equivalence(polygons, *plan) {
                    Ok(true) => {}
                    Ok(false) => failures.push(format!(
                        "case={case} {arrangement_name} {plan_name}: regions differ"
                    )),
                    Err(error) => failures.push(format!(
                        "case={case} {arrangement_name} {plan_name}: {error}"
                    )),
                }
            }
            let ends = [polygons.len()];
            match probe_region_equivalence(polygons, ProbedPlan::PerInstance(&ends)) {
                Ok(true) => {}
                Ok(false) => failures.push(format!(
                    "case={case} {arrangement_name} per-instance: regions differ"
                )),
                Err(error) => failures.push(format!(
                    "case={case} {arrangement_name} per-instance: {error}"
                )),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "PROBE adversarial equivalence failures: {failures:#?}"
    );
    println!(
        "PROBE equivalence adversarial=true cases={} arrangements=3 plans=5",
        cases.len()
    );

    let three = vec![
        square.clone(),
        probe_rectangle(5, 5, 10, 10),
        probe_rectangle(10, 0, 10, 10),
    ];
    for ends in [&[0, 3, 3][..], &[1, 1, 3][..], &[3][..]] {
        assert!(probe_region_equivalence(&three, ProbedPlan::PerInstance(ends)).unwrap());
    }
    for bad in [&[2][..], &[3, 2][..], &[4][..], &[][..]] {
        assert!(probe_region_build(&three, ProbedPlan::PerInstance(bad)).is_err());
    }
    println!("PROBE equivalence empty_groups=true boundary_errors=true");

    for plan in [
        ProbedPlan::Unified,
        ProbedPlan::Chunked(1),
        ProbedPlan::Chunked(164),
    ] {
        assert!(
            probe_region_equivalence(&[], plan).unwrap(),
            "empty input under {plan:?}"
        );
    }
    assert!(probe_region_equivalence(&[], ProbedPlan::PerInstance(&[0, 0, 0])).unwrap());
    assert!(probe_region_build(&[], ProbedPlan::PerInstance(&[])).is_ok());
    assert!(probe_region_build(&[], ProbedPlan::PerInstance(ends)).is_err());
    println!("PROBE equivalence empty_input=true");

    let invalid_plans = [
        ("unified", ProbedPlan::Unified),
        ("chunk-1", ProbedPlan::Chunked(1)),
        ("chunk-2", ProbedPlan::Chunked(2)),
        ("chunk-164", ProbedPlan::Chunked(164)),
        ("per-instance", ProbedPlan::PerInstance(&[3])),
    ];
    for (case, invalid) in [
        vec![[0, 0], [1, 1]],
        vec![[0, 0], [PROBE_GDI_MAX_COORD + 1, 0], [0, 10]],
        vec![[0, 0], [PROBE_GDI_MIN_COORD - 1, 0], [0, 10]],
    ]
    .into_iter()
    .enumerate()
    {
        let mut polygons = vec![square.clone(), probe_rectangle(20, 0, 10, 10)];
        polygons.push(invalid);
        for (plan_name, plan) in &invalid_plans {
            assert!(
                probe_region_build(&polygons, *plan).is_err(),
                "invalid polygon case {case} must fail closed under {plan_name}"
            );
        }
    }
    println!("PROBE equivalence invalid_polygons=Err live=0");
    println!("PROBE done");
}
