use super::*;
use crate::app::{HitRegionCache, RegionMode, benchmark::RegionDecision};
use crate::renderer::{SceneView, desktop_view_at};

fn cache(geometry: HitGeometry) -> HitRegionCache {
    HitRegionCache {
        mode: RegionMode::Pixel,
        geometry: Some(geometry),
        click_through: false,
        failed: false,
        dirty_frames: 0,
        force_install: false,
    }
}

fn deliver(cache: &mut HitRegionCache, mut geometry: HitGeometry) -> RegionDecision {
    let decision = cache.decision(&geometry);
    if matches!(decision, RegionDecision::Rebuilt) {
        geometry.materialize().unwrap();
        cache.geometry = Some(geometry);
        cache.note_installed();
    }
    decision
}

fn moved(frame: &RenderFrame, pixels: f32, size: [u32; 2], view: SceneView) -> RenderFrame {
    let mut frame = frame.clone();
    for draw in &mut frame.instances {
        let transform = match draw {
            InstanceDraw::Sprite2d { transform, .. }
            | InstanceDraw::Skeleton3d { transform, .. } => transform,
        };
        *transform =
            Mat4::from_translation(Vec3::X * pixels * view.span / size[1] as f32) * *transform;
    }
    frame
}

pub(super) fn eous_scene() -> Simulation {
    let model = load_model_bytes(
        include_bytes!("../../assets/examples/bangboo-eous.dmodel.json"),
        &LoadLimits::default(),
    )
    .unwrap();
    let mut simulation = Simulation::new();
    let asset = simulation.insert_asset(model).unwrap();
    for x in [-0.65, 0.65] {
        simulation.spawn(asset, Vec3::new(x, 0.15, 0.0)).unwrap();
    }
    simulation
        .set_wander_bounds(Some(crate::simulation::WorldXBounds {
            left: -3.5,
            right: 3.5,
        }))
        .unwrap();
    simulation
}

#[test]
fn installed_region_covers_ten_pixel_jump_under_k4() {
    let size = [1280, 720];
    let view = desktop_view_at(720, 1.5, 0.0, 0.0);
    let vertices: Vec<_> = [
        [200.0, 200.0],
        [240.0, 200.0],
        [240.0, 300.0],
        [200.0, 300.0],
    ]
    .map(|p| crate::renderer::screen_to_world_with(p, size, view))
    .into();
    let indices: Vec<_> = (0..4096).flat_map(|_| [0, 1, 2, 0, 2, 3]).collect();
    let (simulation, frame) = sprite_scene(&vertices, &indices, 1.0);
    let baseline = HitGeometry::from_frame_with_view(&simulation, &frame, size, view).unwrap();
    assert_eq!(baseline.polygons().len(), 8192);
    let mut cache = cache(baseline);
    let frame = moved(&frame, 10.0, size, view);
    let geometry = HitGeometry::collect_frame_with_view(&simulation, &frame, size, view).unwrap();
    let decision = deliver(&mut cache, geometry);
    let mut gpu = CoverageGpu::with_view(&simulation, size, view);
    assert_gpu_coverage(
        &mut gpu,
        &frame,
        cache.geometry.as_ref().unwrap().polygons(),
        1,
    );
    assert!(
        matches!(decision, RegionDecision::Rebuilt),
        "unsafe movement must bypass K4"
    );
    // After a successful installation, small movement may reuse its margin.
    // The second deferred frame must still compare with the installed pose,
    // not with the preceding candidate; the cumulative jump bypasses K4.
    for (step, pixels) in [5.0, 6.8, 7.2].into_iter().enumerate() {
        let current = moved(&frame, pixels, size, view);
        let geometry =
            HitGeometry::collect_frame_with_view(&simulation, &current, size, view).unwrap();
        let decision = deliver(&mut cache, geometry);
        if step < 2 {
            assert!(matches!(decision, RegionDecision::Deferred));
        } else {
            assert!(matches!(decision, RegionDecision::Rebuilt));
        }
        assert_gpu_coverage(
            &mut gpu,
            &current,
            cache.geometry.as_ref().unwrap().polygons(),
            step + 2,
        );
    }
}

#[test]
fn eous_desktop_installed_region_covers_current_animation() {
    let size = [2560, 1440];
    let view = desktop_view_at(1440, 1.5, 0.0, 0.0);
    let mut simulation = eous_scene();
    let first = simulation.render_frame().unwrap();
    let mut cache =
        cache(HitGeometry::from_frame_with_view(&simulation, &first, size, view).unwrap());
    let mut gpu = CoverageGpu::with_view(&simulation, size, view);
    let mut counts = [0usize; 3];
    let capture = std::env::var_os("DELICIUM_EOUS_CAPTURE_DIR").map(std::path::PathBuf::from);
    for step in 0..240 {
        simulation
            .advance(Duration::from_millis(if step % 3 == 0 { 33 } else { 50 }))
            .unwrap();
        if step == 90 {
            for id in [1, 2] {
                let position = simulation.instance(id).unwrap().position;
                simulation.poke_reaction(id, position + Vec3::X).unwrap();
            }
        }
        let frame = simulation.render_frame().unwrap();
        let geometry =
            HitGeometry::collect_frame_with_view(&simulation, &frame, size, view).unwrap();
        assert!(
            geometry.polygons().is_empty(),
            "collection must not materialize integer polygons"
        );
        let decision = deliver(&mut cache, geometry);
        counts[match decision {
            RegionDecision::Reused => 0,
            RegionDecision::Deferred => 1,
            _ => 2,
        }] += 1;
        if step < 12 || step % 6 == 0 || [89, 90, 91, 92, 93, 94, 95, 96, 119, 120].contains(&step)
        {
            gpu.capture = capture
                .as_ref()
                .filter(|_| step % 24 == 0)
                .map(|dir| dir.join(format!("desktop-{step:03}.png")));
            assert_gpu_coverage(
                &mut gpu,
                &frame,
                cache.geometry.as_ref().unwrap().polygons(),
                step,
            );
        }
    }
    println!(
        "EOUS_TEMPORAL_COVERAGE size={size:?} reused={} deferred={} rebuilt={}",
        counts[0], counts[1], counts[2]
    );
}

/// The same animation at a 1600-tall view. The 1440 case is the primary
/// monitor's own size; this one proves the installed region still covers
/// every rendered pixel when the vertical span grows, which is the geometry
/// the fast path sees on a taller display rather than a second monitor.
///
/// Selected frames only: the ear sweep, the turn, the reaction and the
/// crossfade edges are where a too-tight region would show first, and a full
/// 240-frame GPU sweep at this size costs minutes for little extra signal
/// over the 1440 case that already runs every frame.
#[test]
fn eous_tall_view_installed_region_covers_selected_frames() {
    let size = [2560, 1600];
    let view = desktop_view_at(1600, 1.5, 0.0, 0.0);
    let mut simulation = eous_scene();
    let first = simulation.render_frame().unwrap();
    let mut cache =
        cache(HitGeometry::from_frame_with_view(&simulation, &first, size, view).unwrap());
    let mut gpu = CoverageGpu::with_view(&simulation, size, view);
    let mut poses = 0_usize;
    // The same step script as the 1440 case so the two runs compare frame to
    // frame: early idle, the ear sweep and the turn before the reaction, the
    // reaction itself and its crossfades, and the walk away afterwards.
    for step in 0..240 {
        simulation
            .advance(Duration::from_millis(if step % 3 == 0 { 33 } else { 50 }))
            .unwrap();
        if step == 90 {
            for id in [1, 2] {
                let position = simulation.instance(id).unwrap().position;
                simulation.poke_reaction(id, position + Vec3::X).unwrap();
            }
        }
        let frame = simulation.render_frame().unwrap();
        let geometry =
            HitGeometry::collect_frame_with_view(&simulation, &frame, size, view).unwrap();
        let _decision = deliver(&mut cache, geometry);
        if [0, 6, 18, 30, 60, 89, 90, 91, 92, 96, 120, 180, 239].contains(&step) {
            assert_gpu_coverage(
                &mut gpu,
                &frame,
                cache.geometry.as_ref().unwrap().polygons(),
                step,
            );
            poses += 1;
        }
    }
    println!("EOUS_TALL_COVERAGE size={size:?} poses={poses}");
}

#[test]
#[ignore = "explicit release-mode Eous CPU/region profiling"]
fn eous_region_cpu_probe() {
    let mut simulation = eous_scene();
    let size = [2560, 1440];
    let view = desktop_view_at(1440, 1.5, 0.0, 0.0);
    let mut baseline = None;
    let mut times = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut raw_ms = Vec::new();
    let mut materialize_ms = Vec::new();
    let mut reasons = [0usize; 4]; // unchanged, safe margin, large motion, topology/first
    let mut polygons = 0;
    for step in 0..240 {
        let t = Instant::now();
        simulation
            .advance(Duration::from_millis(if step % 3 == 0 { 33 } else { 50 }))
            .unwrap();
        times[0].push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let frame = simulation.render_frame().unwrap();
        times[1].push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let mut geometry =
            HitGeometry::collect_frame_with_view(&simulation, &frame, size, view).unwrap();
        raw_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let materialize_start = Instant::now();
        geometry.materialize().unwrap();
        materialize_ms.push(materialize_start.elapsed().as_secs_f64() * 1000.0);
        times[2].push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let _changed = baseline
            .as_ref()
            .is_none_or(|old| geometry.changed_since(old));
        times[3].push(t.elapsed().as_secs_f64() * 1000.0);
        let reason = baseline.as_ref().map_or(3, |old| {
            let topology = geometry.instances.len() != old.instances.len()
                || geometry.instances.iter().zip(&old.instances).any(|(a, b)| {
                    a.asset != b.asset
                        || a.instance != b.instance
                        || a.polys.len() != b.polys.len()
                        || a.polys
                            .iter()
                            .zip(&b.polys)
                            .any(|(a, b)| a.len() != b.len())
                });
            if topology {
                return 3;
            }
            match geometry.change_since(old) {
                super::super::RegionChange::Unchanged => 0,
                super::super::RegionChange::WithinMargin => 1,
                super::super::RegionChange::Immediate => 2,
            }
        });
        reasons[reason] += 1;
        polygons = geometry.polygons().len();
        baseline = Some(geometry);
    }
    for (name, mut values) in ["advance", "render_frame", "geometry", "compare"]
        .into_iter()
        .zip(times)
    {
        values.sort_by(f64::total_cmp);
        println!(
            "EOUS_CPU name={name} p50_ms={} p95_ms={} max_ms={} polygons={polygons}",
            values[120], values[228], values[239]
        );
    }
    raw_ms.sort_by(f64::total_cmp);
    materialize_ms.sort_by(f64::total_cmp);
    println!(
        "EOUS_CPU_SPLIT raw_p50_ms={} materialize_p50_ms={} unchanged={} margin={} large_motion={} topology_or_first={}",
        raw_ms[120], materialize_ms[120], reasons[0], reasons[1], reasons[2], reasons[3]
    );
}

/// The preview window's own coverage question.
///
/// The acceptance capture runs `--bangboo` in a 960x640 bounded window, i.e.
/// [`crate::renderer::LEGACY`] with 35k+ polygons per frame, where the install
/// throttle is `region_cadence` 4. Every other GPU coverage test here runs a
/// desktop view at a different cadence, so the exact combination a real
/// preview window uses was never checked against rendered pixels.
///
/// This delivers geometry through the production cache exactly as the app does
/// and, on every sampled pose, asks whether every pixel the GPU actually
/// painted lies inside the region that is installed at that moment. A miss is
/// the same failure the plan calls a missed visible pixel.
#[test]
#[ignore = "preview-view GPU coverage probe; run release explicitly with --ignored"]
fn eous_preview_view_gpu_coverage_probe() {
    const SIZE: [u32; 2] = [960, 640];
    let view = crate::renderer::LEGACY;
    let mut simulation = eous_scene();
    let first = simulation.render_frame().unwrap();
    let mut cache =
        cache(HitGeometry::from_frame_with_view(&simulation, &first, SIZE, view).unwrap());
    let mut gpu = CoverageGpu::with_view(&simulation, SIZE, view);
    let mut poses = 0_usize;
    let mut rebuilt = 0_usize;
    let mut deferred = 0_usize;
    for step in 0..540 {
        simulation
            .advance(Duration::from_millis(if step % 3 == 0 { 33 } else { 50 }))
            .unwrap();
        if step == 90 {
            for id in [1, 2] {
                let position = simulation.instance(id).unwrap().position;
                simulation.poke_reaction(id, position + Vec3::X).unwrap();
            }
        }
        let frame = simulation.render_frame().unwrap();
        let geometry =
            HitGeometry::collect_frame_with_view(&simulation, &frame, SIZE, view).unwrap();
        match deliver(&mut cache, geometry) {
            RegionDecision::Rebuilt => rebuilt += 1,
            RegionDecision::Deferred => deferred += 1,
            _ => {}
        }
        // The capture takes a screenshot every 900ms, so a sample every ~30
        // animation steps lands across idle, greet, play, the reaction and the
        // walk rather than inside one action only.
        if step % 30 != 0 {
            continue;
        }
        let delivered = cache.geometry.as_ref().map(|geometry| geometry.polygons());
        let Some(polygons) = delivered else {
            continue;
        };
        assert_gpu_coverage(&mut gpu, &frame, polygons, step);
        poses += 1;
    }
    println!(
        "PROBE preview gpu view=legacy size={SIZE:?} poses={poses} rebuilt={rebuilt} deferred={deferred} status=covered"
    );
    assert!(poses >= 15, "probe must sample enough poses, got {poses}");
}
