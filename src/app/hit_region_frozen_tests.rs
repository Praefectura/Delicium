use super::super::*;
use super::temporal;
use std::time::{Duration, Instant};

fn bits(polys: &[Vec<[f32; 2]>]) -> Vec<Vec<[u32; 2]>> {
    polys
        .iter()
        .map(|poly| poly.iter().map(|p| p.map(f32::to_bits)).collect())
        .collect()
}

fn compare_frame(simulation: &Simulation, frame: &RenderFrame, size: [u32; 2], view: SceneView) {
    let mut current = HitGeometry::collect_frame_with_view(simulation, frame, size, view).unwrap();
    let mut frozen =
        oracle::HitGeometry::collect_frame_with_view(simulation, frame, size, view).unwrap();
    let current_bits: Vec<_> = current
        .instances
        .iter()
        .map(|instance| bits(&instance.polys))
        .collect();
    assert_eq!(
        current_bits,
        oracle::raw_bits(&frozen),
        "raw geometry bits, size={size:?} view={view:?}"
    );
    current.materialize().unwrap();
    frozen.materialize();
    assert_eq!(
        current.polygons(),
        frozen.polygons(),
        "integer polygons, size={size:?} view={view:?}"
    );
    assert_eq!(
        current.instance_polygon_ends(),
        frozen.instance_polygon_ends()
    );
    let expected = current.polygons().to_vec();
    current.materialize().unwrap();
    assert_eq!(
        current.polygons(),
        expected,
        "materialize must remain idempotent"
    );
}

#[test]
fn scratch_materialize_matches_frozen_boundary_cases() {
    let mut cases = vec![
        vec![],
        vec![[0.0, 0.0]],
        vec![[0.0, 0.0], [10.0, 10.0]],
        vec![[0.0, 0.0], [10.0, 10.0], [20.0, 20.0]],
        vec![[8.0, 8.0], [92.0, 8.0], [8.0, 72.0]],
        vec![[0.0, 0.0], [100.0, 0.0], [100.0, 80.0], [0.0, 80.0]],
        vec![[-20.0, -10.0], [120.0, 15.0], [50.0, 100.0]],
        vec![[-100.0, -100.0], [-90.0, -100.0], [-95.0, -90.0]],
        vec![[10.0, 10.0], [90.0, 10.0], [10.0, 10.00001]],
        vec![[-0.0, 0.0], [0.0, -0.0], [80.0, 1.0], [1.0, 70.0]],
        vec![[0.0, -0.0], [-0.0, 0.0], [1.0, 70.0], [80.0, 1.0]],
        vec![[100.0, 80.0]; 4],
    ];
    for n in 3..=9 {
        let polygon: Vec<_> = (0..n)
            .map(|i| {
                let angle = i as f32 * std::f32::consts::TAU / n as f32;
                [50.0 + angle.cos() * 55.0, 40.0 + angle.sin() * 45.0]
            })
            .collect();
        cases.push(polygon.clone());
        cases.push(polygon.into_iter().rev().collect());
    }
    for (case, poly) in cases.iter().enumerate() {
        for size in [[100, 80], [0, 0]] {
            let expected = oracle::materialize_polys(std::slice::from_ref(poly), size);
            let mut current = HitGeometry {
                size,
                view: crate::renderer::LEGACY,
                instances: vec![InstanceOutline {
                    asset: 1,
                    instance: 1,
                    polys: vec![poly.clone()],
                }],
                polygons: Vec::new(),
                instance_polygon_ends: Vec::new(),
            };
            current.materialize().unwrap();
            assert_eq!(current.polygons(), expected, "case={case} size={size:?}");
        }
    }
}

fn keyed_candidates(points: &[[f32; 2]]) -> Vec<Candidate> {
    let mut candidates = Vec::with_capacity(points.len());
    for &point in points {
        push_candidate(&mut candidates, point).unwrap();
    }
    candidates
}

#[test]
fn candidate_keys_match_finite_numeric_order_and_order_laws() {
    let values = [
        -f32::MAX,
        -1.0,
        -f32::MIN_POSITIVE,
        -f32::from_bits(1),
        -0.0,
        0.0,
        f32::from_bits(1),
        f32::MIN_POSITIVE,
        1.0,
        f32::MAX,
    ];
    let points: Vec<_> = values
        .iter()
        .flat_map(|&x| values.map(|y| [x, y]))
        .collect();
    let candidates = keyed_candidates(&points);
    for a in &candidates {
        for b in &candidates {
            let expected = a
                .point
                .partial_cmp(&b.point)
                .unwrap()
                .then_with(|| a.ordinal.cmp(&b.ordinal));
            let order = compare_candidates(a, b);
            assert_eq!(order, expected, "{a:?} {b:?}");
            assert_eq!(order, compare_candidates(b, a).reverse());
        }
    }
    let subset: Vec<_> = candidates.iter().step_by(7).collect();
    for a in &subset {
        for b in &subset {
            for c in &subset {
                if compare_candidates(a, b).is_le() && compare_candidates(b, c).is_le() {
                    assert!(compare_candidates(a, c).is_le());
                }
            }
        }
    }
}

#[test]
fn keyed_sort_and_hull_preserve_stable_finite_bits() {
    let edge_bits = [
        0,
        0x8000_0000,
        1,
        0x8000_0001,
        0x007f_ffff,
        0x0080_0000,
        0x7f7f_ffff,
        0xff7f_ffff,
        0x3f80_0000,
        0xbf80_0000,
    ];
    let mut seed = 0x582a_3179u32;
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let value = f32::from_bits(seed);
        if value.is_finite() { value } else { 0.0 }
    };
    for round in 0..4 {
        for len in [0, 1, 2, 3, 7, 12, 16, 24, 31, 32, 63, 64, 65, 128, 257] {
            let points: Vec<_> = (0..len)
                .map(|i| {
                    if i % 3 == 0 {
                        [
                            f32::from_bits(edge_bits[(i + round) % edge_bits.len()]),
                            f32::from_bits(edge_bits[(i / 3 + round) % edge_bits.len()]),
                        ]
                    } else if i % 3 == 1 {
                        [next(), next()]
                    } else {
                        [1.0, -0.0]
                    }
                })
                .collect();
            let mut expected = keyed_candidates(&points);
            expected.sort_by(|a, b| a.point.partial_cmp(&b.point).unwrap());
            let mut actual = keyed_candidates(&points);
            actual.sort_unstable_by(compare_candidates);
            let identity = |p: &Candidate| (p.point.map(f32::to_bits), p.ordinal);
            assert_eq!(
                actual.iter().map(identity).collect::<Vec<_>>(),
                expected.iter().map(identity).collect::<Vec<_>>(),
                "round={round} len={len}"
            );
            actual.dedup_by(|a, b| a.point == b.point);
            expected.dedup_by(|a, b| a.point == b.point);
            assert_eq!(
                actual.iter().map(identity).collect::<Vec<_>>(),
                expected.iter().map(identity).collect::<Vec<_>>()
            );
            let hull: Vec<_> = convex_hull(&mut points.clone())
                .iter()
                .map(|p| p.map(f32::to_bits))
                .collect();
            assert_eq!(hull, oracle::hull_bits(points), "round={round} len={len}");
        }
    }
}

#[test]
fn candidate_keys_never_accept_nonfinite_coordinates() {
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for point in [[invalid, 1.0], [1.0, invalid]] {
            let mut candidates = keyed_candidates(&[[1.0, 2.0]]);
            let before = candidates[0];
            assert!(
                push_candidate(&mut candidates, point)
                    .unwrap_err()
                    .contains("not finite")
            );
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].point, before.point);
            assert_eq!(candidates[0].key, before.key);
            assert_eq!(candidates[0].ordinal, before.ordinal);
            assert!(convex_hull(&mut vec![point, [1.0, 2.0], [2.0, 1.0]]).is_empty());
        }
    }
}

#[test]
fn scratch_hull_preserves_stable_signed_zero_representatives() {
    for points in [
        vec![[-0.0, 0.0], [0.0, -0.0], [8.0, 0.0], [0.0, 8.0]],
        vec![[0.0, -0.0], [-0.0, 0.0], [8.0, 0.0], [0.0, 8.0]],
        vec![[-0.0, -0.0], [0.0, 0.0], [-0.0, 0.0]],
        vec![[0.0, -0.0], [0.0, 0.0]],
    ] {
        let expected = oracle::hull_bits(points.clone());
        let actual = convex_hull(&mut points.clone());
        let actual: Vec<_> = actual.iter().map(|p| p.map(f32::to_bits)).collect();
        assert_eq!(actual, expected);
        for size in [[8.0, 8.0], [0.0, 0.0]] {
            assert_eq!(
                bits(&[clip_to_client(&points, size[0], size[1])]),
                bits(&[oracle::client_clip(&points, size[0], size[1])])
            );
        }
    }
}

#[test]
fn scratch_triangle_matches_frozen_depth_and_client_boundaries() {
    for z in [-91.0, -90.0, -89.9, 0.0, 9.8, 9.9, 10.0] {
        for scale in [0.001, 1.0, 10.0] {
            for mirror in [-1.0, 1.0] {
                let triangle = [
                    [-scale * mirror, -scale, z],
                    [scale * mirror, -scale, z + 0.25],
                    [-0.0, scale, z - 0.25],
                ];
                for (size, view) in [
                    ([960, 640], crate::renderer::LEGACY),
                    (
                        [2560, 1600],
                        crate::renderer::desktop_view_at(1600, 1.5, 0.0, 0.0),
                    ),
                ] {
                    let mut current = Vec::new();
                    let actual = push_triangle(
                        &mut current,
                        triangle,
                        size,
                        view,
                        size[0] as f32,
                        size[1] as f32,
                    );
                    let expected = oracle::triangle(triangle, size, view);
                    match (actual, expected) {
                        (Ok(()), Ok(expected)) => assert_eq!(bits(&current), bits(&expected)),
                        (Err(a), Err(b)) => assert_eq!(a, b),
                        (a, b) => panic!("triangle result mismatch: {a:?} vs {b:?}"),
                    }
                }
            }
        }
    }
}

#[test]
fn finite_client_clip_overflow_is_rejected_before_publishing() {
    let size = [960, 640];
    let view = crate::renderer::LEGACY;
    let triangle = [[-2.0e38, 200.0], [2.0e38, 200.0], [480.0, 400.0]].map(|screen| {
        let world = crate::renderer::screen_to_world_with(screen, size, view);
        assert!(world.iter().all(|v| v.is_finite()));
        assert!(
            world_to_screen_with(world, size, view)
                .iter()
                .all(|v| v.is_finite())
        );
        [world[0], world[1], 0.0]
    });
    let old = oracle::triangle(triangle, size, view).unwrap();
    assert!(
        old.iter().flatten().flatten().any(|v| !v.is_finite()),
        "fixture must reach the frozen clip-overflow route"
    );
    let sentinel = vec![[10.0, 10.0], [20.0, 10.0], [10.0, 20.0]];
    let mut outlines = vec![sentinel.clone()];
    let error = push_triangle(&mut outlines, triangle, size, view, 960.0, 640.0).unwrap_err();
    assert!(
        error.contains("clipped outline vertex is not finite"),
        "{error}"
    );
    assert_eq!(outlines, vec![sentinel]);
}

#[test]
fn scratch_mixed_animated_models_match_frozen_including_mirrors() {
    let mut simulation = super::animated_scene();
    for _ in 0..8 {
        simulation.advance(Duration::from_millis(137)).unwrap();
        let mut frame = simulation.render_frame().unwrap();
        compare_frame(&simulation, &frame, [960, 640], crate::renderer::LEGACY);
        for draw in &mut frame.instances {
            let transform = match draw {
                InstanceDraw::Sprite2d { transform, .. }
                | InstanceDraw::Skeleton3d { transform, .. } => transform,
            };
            *transform = Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)) * *transform;
        }
        compare_frame(&simulation, &frame, [960, 640], crate::renderer::LEGACY);
    }
}

#[test]
fn scratch_geometry_matches_frozen_representative_frames() {
    for (size, view) in [
        ([960, 640], crate::renderer::LEGACY),
        (
            [2560, 1600],
            crate::renderer::desktop_view_at(1600, 1.5, 0.0, 0.0),
        ),
        (
            [4160, 2560],
            crate::renderer::desktop_view_at(2560, 1.5, 0.0, 0.0),
        ),
    ] {
        let mut simulation = temporal::eous_scene();
        for step in 0..2 {
            simulation
                .advance(Duration::from_millis(33 + 17 * step))
                .unwrap();
            let frame = simulation.render_frame().unwrap();
            compare_frame(&simulation, &frame, size, view);
        }
    }
}

#[test]
#[ignore = "full frozen geometry equivalence; run release explicitly with --ignored"]
fn materialize_frozen_oracle_probe() {
    let start = Instant::now();
    let mut compared = 0;
    for (size, view, steps) in [
        (
            [2560, 1600],
            crate::renderer::desktop_view_at(1600, 1.5, 0.0, 0.0),
            240,
        ),
        ([960, 640], crate::renderer::LEGACY, 240),
        (
            [4160, 2560],
            crate::renderer::desktop_view_at(2560, 1.5, 0.0, 0.0),
            12,
        ),
    ] {
        let mut simulation = temporal::eous_scene();
        for step in 0..steps {
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
            compare_frame(&simulation, &frame, size, view);
            compared += 1;
        }
        println!(
            "MATERIALIZE_EQUIVALENCE size={size:?} frames={steps} raw_bits=equal integer_sequences=equal"
        );
    }
    println!(
        "MATERIALIZE_EQUIVALENCE total={compared} elapsed_s={:.3} status=passed",
        start.elapsed().as_secs_f64()
    );
}

pub(super) fn expected_materialize(polys: &[Vec<[f32; 2]>], size: [u32; 2]) -> Vec<Vec<[i32; 2]>> {
    oracle::materialize_polys(polys, size)
}

fn allocation_geometry() -> HitGeometry {
    let mut geometry = HitGeometry {
        size: [100, 80],
        view: crate::renderer::LEGACY,
        instances: vec![InstanceOutline {
            asset: 1,
            instance: 1,
            polys: vec![
                vec![[10.0, 10.0], [90.0, 10.0], [50.0, 70.0]],
                vec![[-5.0, 40.0], [50.0, -5.0], [105.0, 85.0]],
            ],
        }],
        polygons: Vec::new(),
        instance_polygon_ends: Vec::new(),
    };
    geometry.instances.push(InstanceOutline {
        asset: 1,
        instance: 2,
        polys: vec![vec![[20.0, 20.0], [80.0, 20.0], [50.0, 60.0]]],
    });
    geometry
}

struct ReserveReset;
impl Drop for ReserveReset {
    fn drop(&mut self) {
        set_reserve_failure(None);
    }
}

#[test]
fn materialize_allocation_failures_clear_partial_output() {
    let _reset = ReserveReset;
    set_reserve_failure(None);
    let mut successful = allocation_geometry();
    successful.materialize().unwrap();
    let attempts = reserve_attempts();
    assert!(
        attempts >= 5,
        "must exercise scratch, outer and final outputs"
    );
    for after in 0..attempts {
        let mut candidate = allocation_geometry();
        set_reserve_failure(Some(after));
        let result = candidate.materialize();
        assert!(
            result.is_err(),
            "reserve failure {after}/{attempts} not propagated"
        );
        assert!(
            candidate.polygons().is_empty(),
            "partial polygons after failure {after}"
        );
        assert!(
            candidate.instance_polygon_ends().is_empty(),
            "partial metadata after failure {after}"
        );
        set_reserve_failure(None);
        candidate.materialize().unwrap();
        assert_eq!(candidate.polygons(), successful.polygons());
        assert_eq!(
            candidate.instance_polygon_ends(),
            successful.instance_polygon_ends()
        );
    }
    set_reserve_failure(None);
    let mut retried = allocation_geometry();
    retried.materialize().unwrap();
    assert_eq!(retried.polygons(), successful.polygons());
}

#[test]
fn materialization_rejects_nonfinite_internal_points_without_partial_output() {
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut geometry = allocation_geometry();
        geometry.instances[0].polys[1][0][0] = value;
        assert!(geometry.materialize().is_err());
        assert!(geometry.polygons().is_empty());
        assert!(geometry.instance_polygon_ends().is_empty());
    }
}

#[test]
fn collection_allocation_failures_return_errors_without_poisoning_next_frame() {
    let _reset = ReserveReset;
    let sprite = super::sprite_scene(&[[-0.5, -0.5], [0.5, -0.5], [0.0, 0.5]], &[0, 1, 2], 1.0);
    let model = super::load_model_bytes(
        include_bytes!("../../assets/examples/robot3d.dmodel.json"),
        &super::LoadLimits::default(),
    )
    .unwrap();
    let mut skeleton = Simulation::new();
    let asset = skeleton.insert_asset(model).unwrap();
    skeleton.spawn(asset, Vec3::ZERO).unwrap();
    let frame = skeleton.render_frame().unwrap();
    for (simulation, frame) in [sprite, (skeleton, frame)] {
        set_reserve_failure(None);
        let successful = HitGeometry::collect_frame_with_view(
            &simulation,
            &frame,
            [960, 640],
            crate::renderer::LEGACY,
        )
        .unwrap();
        let attempts = reserve_attempts();
        assert!(attempts >= 3);
        for after in 0..attempts {
            set_reserve_failure(Some(after));
            assert!(
                HitGeometry::collect_frame_with_view(
                    &simulation,
                    &frame,
                    [960, 640],
                    crate::renderer::LEGACY
                )
                .is_err(),
                "collect reserve failure {after}/{attempts}"
            );
        }
        set_reserve_failure(None);
        let retry = HitGeometry::collect_frame_with_view(
            &simulation,
            &frame,
            [960, 640],
            crate::renderer::LEGACY,
        )
        .unwrap();
        assert_eq!(
            bits(&retry.instances[0].polys),
            bits(&successful.instances[0].polys)
        );
    }
}

#[test]
#[ignore = "diagnostic per-polygon timers; attribution only, not a performance gate"]
fn materialize_substage_diagnostic_probe() {
    let _reset = ReserveReset;
    let mut simulation = temporal::eous_scene();
    let size = [2560, 1600];
    let view = crate::renderer::desktop_view_at(1600, 1.5, 0.0, 0.0);
    let [width, height] = size.map(|n| n as f32);
    for step in 0..120 {
        simulation
            .advance(Duration::from_millis(if step % 3 == 0 { 33 } else { 50 }))
            .unwrap();
        if step == 90 {
            for id in [1, 2] {
                let position = simulation.instance(id).unwrap().position;
                simulation.poke_reaction(id, position + Vec3::X).unwrap();
            }
        }
        if ![0, 20, 40, 60, 90, 118].contains(&step) {
            continue;
        }
        let frame = simulation.render_frame().unwrap();
        let mut geometry =
            HitGeometry::collect_frame_with_view(&simulation, &frame, size, view).unwrap();
        let raw_count = geometry.instances.iter().map(|i| i.polys.len()).sum();
        let mut result = Vec::with_capacity(raw_count);
        let mut scratch = MaterializeScratch::default();
        let mut times = [Duration::ZERO; 4];
        let mut inside_count = 0usize;
        let mut clip_count = 0usize;
        set_reserve_failure(None);
        for poly in geometry.instances.iter().flat_map(|i| &i.polys) {
            let start = Instant::now();
            pad_polygon_into(
                poly,
                HIT_PADDING_PX,
                &mut scratch.candidates,
                &mut scratch.hull,
            )
            .unwrap();
            times[0] += start.elapsed();
            let start = Instant::now();
            let inside = client_fully_inside(&scratch.hull, width, height);
            if inside {
                inside_count += 1;
            } else {
                clip_count += 1;
                clip_to_client_into(
                    &scratch.hull,
                    &mut scratch.clip_a,
                    &mut scratch.clip_b,
                    width,
                    height,
                )
                .unwrap();
            }
            times[1] += start.elapsed();
            let start = Instant::now();
            let clipped = if inside {
                &scratch.hull
            } else {
                &scratch.clip_b
            };
            let region = region_polygon_into(
                clipped,
                width,
                height,
                &mut scratch.candidates,
                &mut scratch.region_hull,
            )
            .unwrap();
            times[2] += start.elapsed();
            let start = Instant::now();
            if region.len() >= 3 {
                result.push(region);
            }
            times[3] += start.elapsed();
        }
        let reserves = reserve_attempts();
        geometry.materialize().unwrap();
        assert_eq!(result, geometry.polygons());
        let ms = times.map(|d| d.as_secs_f64() * 1000.0);
        println!(
            "MATERIALIZE_DIAGNOSTIC step={step} raw={raw_count} inside={inside_count} clipped={clip_count} reserves={reserves} pad_ms={:.4} client_ms={:.4} integer_ms={:.4} publish_ms={:.4} sequences=equal instrumentation=per_polygon_not_gate",
            ms[0], ms[1], ms[2], ms[3]
        );
    }
}

#[test]
#[ignore = "diagnostic comparator counts only; not a performance gate"]
fn materialize_comparator_count_probe() {
    fn counts(points: &[[f32; 2]], candidates: &mut Vec<Candidate>) -> [usize; 3] {
        candidates.clear();
        candidates.extend(
            points
                .iter()
                .enumerate()
                .map(|(ordinal, &point)| Candidate {
                    point,
                    key: candidate_sort_key(point),
                    ordinal,
                }),
        );
        let mut counts = [0; 3];
        candidates.sort_unstable_by(|a, b| {
            counts[0] += 1;
            let order = a.point.partial_cmp(&b.point).unwrap_or(Ordering::Equal);
            if order == Ordering::Equal {
                counts[1] += 1;
                if a.point[0] != 0.0 && a.point[1] != 0.0 {
                    counts[2] += 1;
                }
            }
            order.then_with(|| a.ordinal.cmp(&b.ordinal))
        });
        counts
    }
    let mut simulation = temporal::eous_scene();
    let size = [2560, 1600];
    let view = crate::renderer::desktop_view_at(1600, 1.5, 0.0, 0.0);
    let mut points = Vec::new();
    let mut candidates = Vec::new();
    for step in 0..120 {
        simulation
            .advance(Duration::from_millis(if step % 3 == 0 { 33 } else { 50 }))
            .unwrap();
        if step == 90 {
            for id in [1, 2] {
                let position = simulation.instance(id).unwrap().position;
                simulation.poke_reaction(id, position + Vec3::X).unwrap();
            }
        }
        if ![0, 20, 40, 60, 90, 118].contains(&step) {
            continue;
        }
        let frame = simulation.render_frame().unwrap();
        let geometry =
            HitGeometry::collect_frame_with_view(&simulation, &frame, size, view).unwrap();
        let mut totals = [[0usize; 3]; 2];
        for poly in geometry.instances.iter().flat_map(|i| &i.polys) {
            points.clear();
            for &[x, y] in poly {
                let p = HIT_PADDING_PX;
                points.extend([
                    [x - p, y - p],
                    [x + p, y - p],
                    [x - p, y + p],
                    [x + p, y + p],
                ]);
            }
            for (total, value) in totals[0].iter_mut().zip(counts(&points, &mut candidates)) {
                *total += value;
            }
            let padded = pad_polygon(poly, HIT_PADDING_PX);
            let clipped = clip_to_client(&padded, size[0] as f32, size[1] as f32);
            points.clear();
            for [x, y] in clipped {
                points.extend([
                    [x.floor(), y.floor()],
                    [x.ceil(), y.floor()],
                    [x.floor(), y.ceil()],
                    [x.ceil(), y.ceil()],
                ]);
            }
            for (total, value) in totals[1].iter_mut().zip(counts(&points, &mut candidates)) {
                *total += value;
            }
        }
        println!(
            "COMPARATOR_COUNTS step={step} pad={:?} integer={:?} columns=compares,equal,zero_free_equal timing=not_measured",
            totals[0], totals[1]
        );
    }
}

#[test]
#[ignore = "three-round independent old/new stage gate; run release explicitly with --ignored"]
fn materialize_scratch_stage_gate_probe() {
    let mut simulation = temporal::eous_scene();
    let size = [2560, 1600];
    let view = crate::renderer::desktop_view_at(1600, 1.5, 0.0, 0.0);
    let mut frames = Vec::new();
    for step in 0..120 {
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
        if step % 2 == 0 {
            frames.push(frame);
        }
    }
    // Warm both implementations without retaining geometry between samples.
    for frame in frames.iter().take(2) {
        let mut old =
            oracle::HitGeometry::collect_frame_with_view(&simulation, frame, size, view).unwrap();
        old.materialize();
        let mut new = HitGeometry::collect_frame_with_view(&simulation, frame, size, view).unwrap();
        new.materialize().unwrap();
        std::hint::black_box((old.polygons(), new.polygons()));
    }
    let mut passed = true;
    for round in 0..3 {
        let mut timings: [[Vec<f64>; 3]; 2] =
            std::array::from_fn(|_| std::array::from_fn(|_| Vec::new()));
        for (index, frame) in frames.iter().enumerate() {
            for arm in if (index + round) % 2 == 0 {
                [0, 1]
            } else {
                [1, 0]
            } {
                let start = Instant::now();
                if arm == 0 {
                    let mut geometry = oracle::HitGeometry::collect_frame_with_view(
                        &simulation,
                        frame,
                        size,
                        view,
                    )
                    .unwrap();
                    let collected = start.elapsed();
                    let materialize = Instant::now();
                    geometry.materialize();
                    let materialized = materialize.elapsed();
                    let combined = start.elapsed();
                    std::hint::black_box(geometry.polygons());
                    for (values, elapsed) in
                        timings[arm]
                            .iter_mut()
                            .zip([collected, materialized, combined])
                    {
                        values.push(elapsed.as_secs_f64() * 1000.0);
                    }
                } else {
                    let mut geometry =
                        HitGeometry::collect_frame_with_view(&simulation, frame, size, view)
                            .unwrap();
                    let collected = start.elapsed();
                    let materialize = Instant::now();
                    geometry.materialize().unwrap();
                    let materialized = materialize.elapsed();
                    let combined = start.elapsed();
                    std::hint::black_box(geometry.polygons());
                    for (values, elapsed) in
                        timings[arm]
                            .iter_mut()
                            .zip([collected, materialized, combined])
                    {
                        values.push(elapsed.as_secs_f64() * 1000.0);
                    }
                }
            }
        }
        for (arm, values) in timings.iter_mut().enumerate() {
            for samples in values.iter_mut() {
                samples.sort_by(f64::total_cmp);
            }
            let p50 = values.each_ref().map(|samples| samples[samples.len() / 2]);
            let p95 = values
                .each_ref()
                .map(|samples| samples[(samples.len() * 95 / 100).min(samples.len() - 1)]);
            println!(
                "MATERIALIZE_STAGE round={} arm={} frames={} collect_p50_ms={:.4} materialize_p50_ms={:.4} combined_p50_ms={:.4} collect_p95_ms={:.4} materialize_p95_ms={:.4} combined_p95_ms={:.4}",
                round + 1,
                if arm == 0 { "frozen" } else { "scratch" },
                frames.len(),
                p50[0],
                p50[1],
                p50[2],
                p95[0],
                p95[1],
                p95[2]
            );
            if arm == 1 {
                passed &= p50[0] <= 5.0 && p50[1] <= 16.0 && p50[2] <= 20.0;
            }
        }
    }
    println!(
        "MATERIALIZE_STAGE gate={}",
        if passed { "passed" } else { "failed" }
    );
    assert!(
        passed,
        "preregistered stage performance gate failed; stop without changing thresholds"
    );
}

// Independent snapshot of the complete pre-scratch geometry pipeline.
#[allow(dead_code)]
mod oracle {
    //! Screen-space character outlines that back the native window hit region.
    //!
    //! The region is the union of every visible triangle of a frame, projected to
    //! physical window pixels and padded by [`HIT_PADDING_PX`] along each axis. It
    //! is a geometric outline of the rendered silhouette, not a copy of its
    //! pixels: a fully transparent 2D layer contributes nothing, masks only shape
    //! the shader's alpha, and anti-aliased coverage is not reproduced. Triangles
    //! are built exactly as the renderer builds them (deformed 2D layers, skinned
    //! 3D vertices) and projected with [`world_to_screen_with`], the inverse of
    //! the picking transform.
    //!
    //! Clipping never moves vertices onto the window border: triangles are first
    //! clipped against the fixed camera's near/far depth slab (world z in
    //! `-90.0 ..= 9.9` maps to NDC z in `0 ..= 1`), then against the client
    //! rectangle as polygons. Geometry outside the depth slab cannot intercept
    //! input, and partially visible triangles keep their true silhouette.

    use std::cmp::Ordering;

    use glam::{Mat4, Vec3};

    use crate::{
        model::{CompiledModel, CompiledSkeleton3d, CompiledSprite2d, ModelData},
        renderer::{
            InstanceDraw, RenderAssetId, RenderFrame, RenderInstanceId, SceneView,
            world_to_screen_with,
        },
        simulation::Simulation,
    };

    /// Padding added around every projected triangle, in physical pixels.
    ///
    /// The padding is the Minkowski sum with an axis-aligned square, so the
    /// margin is exactly this many pixels along each axis (at most
    /// `sqrt(2) * HIT_PADDING_PX` along a diagonal) and never an unbounded spike
    /// on a sharp corner.
    pub(super) const HIT_PADDING_PX: f32 = 8.0;

    /// Movement dead zone, measured against the last successful installation.
    pub(super) const REGION_MOVE_EPSILON_PX: f32 = 4.0;

    /// Deferral must leave at least one pixel inside the padded native region.
    /// Unlike the dead zone, this is a hard limit even under install throttling.
    pub(super) const REGION_SAFE_LAG_PX: f32 = 7.0;
    const _: () = assert!(REGION_MOVE_EPSILON_PX < REGION_SAFE_LAG_PX);
    const _: () = assert!(REGION_SAFE_LAG_PX < HIT_PADDING_PX);

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum RegionChange {
        Unchanged,
        WithinMargin,
        Immediate,
    }

    /// World-space z of the camera's near plane (NDC z = 0); see
    /// [`crate::renderer::scene_projection`], an orthographic view at `z = 10`
    /// looking down -Z with near/far depths `0.1`/`100`.
    const DEPTH_NEAR_Z: f32 = 9.9;

    /// World-space z of the camera's far plane (NDC z = 1).
    const DEPTH_FAR_Z: f32 = -90.0;

    /// One instance's contribution to the outline: the projected screen-space
    /// triangles of the last successful build, after clipping and before padding.
    #[derive(Debug)]
    struct InstanceOutline {
        asset: RenderAssetId,
        instance: RenderInstanceId,
        polys: Vec<Vec<[f32; 2]>>,
    }

    impl InstanceOutline {
        fn change_since(&self, previous: &Self) -> RegionChange {
            if self.asset != previous.asset
                || self.instance != previous.instance
                || self.polys.len() != previous.polys.len()
            {
                return RegionChange::Immediate;
            }
            let mut change = RegionChange::Unchanged;
            for (current, previous) in self.polys.iter().zip(&previous.polys) {
                if current.len() != previous.len() {
                    return RegionChange::Immediate;
                }
                for (current, previous) in current.iter().zip(previous) {
                    let dx = current[0] - previous[0];
                    let dy = current[1] - previous[1];
                    let distance_sq = dx * dx + dy * dy;
                    if distance_sq >= REGION_SAFE_LAG_PX * REGION_SAFE_LAG_PX {
                        return RegionChange::Immediate;
                    }
                    if distance_sq > REGION_MOVE_EPSILON_PX * REGION_MOVE_EPSILON_PX {
                        change = RegionChange::WithinMargin;
                    }
                }
            }
            change
        }
    }

    /// The geometry a pixel region was last built from.
    ///
    /// The raw screen-space polygons and the padded integer polygons are kept
    /// apart: [`HitGeometry::change_since`] compares the raw geometry, so a
    /// region is rebuilt on real movement instead of on rounding noise, and the
    /// stored integer polygons are the ones actually installed.
    #[derive(Debug)]
    pub(super) struct HitGeometry {
        size: [u32; 2],
        view: SceneView,
        instances: Vec<InstanceOutline>,
        polygons: Vec<Vec<[i32; 2]>>,
        /// Exclusive end of every instance's polygons inside `polygons`, in
        /// frame order; probe-only metadata for the batched-install decision
        /// gate (see `.artifacts/perf-perbatch-20261016-015856/`).
        #[cfg(all(test, target_os = "windows"))]
        instance_polygon_ends: Vec<usize>,
    }

    impl HitGeometry {
        /// Collects the screen-space outline of every instance in `frame`.
        ///
        /// `size` is the physical client size the projection maps to. Broken
        /// assets (layer count mismatches, out-of-range indices, missing skin
        /// matrices) and non-finite deformations are reported as errors; the
        /// caller falls back instead of silently leaving holes in the region.
        /// [`HitGeometry::from_frame_with_view`] under the frozen [`LEGACY`] view,
        /// kept as the entry point of the region tests.
        #[cfg(all(test, target_os = "windows"))]
        pub(super) fn from_frame(
            simulation: &Simulation,
            frame: &RenderFrame,
            size: [u32; 2],
        ) -> Result<Self, String> {
            use crate::renderer::LEGACY;
            Self::from_frame_with_view(simulation, frame, size, LEGACY)
        }

        /// [`HitGeometry::from_frame`] under an explicit [`SceneView`]; the view
        /// decides the world-to-screen mapping the outlines are projected with.
        #[cfg(all(test, target_os = "windows"))]
        pub(super) fn from_frame_with_view(
            simulation: &Simulation,
            frame: &RenderFrame,
            size: [u32; 2],
            view: SceneView,
        ) -> Result<Self, String> {
            let mut geometry = Self::collect_frame_with_view(simulation, frame, size, view)?;
            geometry.materialize();
            Ok(geometry)
        }

        /// Collects only raw comparison geometry. Deferred and reused frames do
        /// not need padded integer polygons; only an installation materializes them.
        pub(super) fn collect_frame_with_view(
            simulation: &Simulation,
            frame: &RenderFrame,
            size: [u32; 2],
            view: SceneView,
        ) -> Result<Self, String> {
            let mut instances = Vec::with_capacity(frame.instances.len());
            let polygons = Vec::new();
            #[cfg(all(test, target_os = "windows"))]
            let instance_polygon_ends = Vec::with_capacity(frame.instances.len());
            if size[0] == 0 || size[1] == 0 {
                // A zero-sized client shows nothing and must not intercept input.
                return Ok(Self {
                    size,
                    view,
                    instances,
                    polygons,
                    #[cfg(all(test, target_os = "windows"))]
                    instance_polygon_ends,
                });
            }
            for draw in &frame.instances {
                let (asset, polys) = match draw {
                    InstanceDraw::Sprite2d {
                        asset,
                        transform,
                        layers,
                        ..
                    } => {
                        let model = asset_model(simulation, *asset)?;
                        let ModelData::Sprite2d(sprite) = &model.data else {
                            return Err(format!(
                                "asset {asset} is drawn as a 2D sprite but is a {} model",
                                model.data.kind()
                            ));
                        };
                        (
                            *asset,
                            sprite_outline(*asset, sprite, transform, layers, size, view)?,
                        )
                    }
                    InstanceDraw::Skeleton3d {
                        asset,
                        transform,
                        skin_matrices,
                        ..
                    } => {
                        let model = asset_model(simulation, *asset)?;
                        let ModelData::Skeleton3d(skeleton) = &model.data else {
                            return Err(format!(
                                "asset {asset} is drawn as a 3D skeleton but is a {} model",
                                model.data.kind()
                            ));
                        };
                        (
                            *asset,
                            skeleton_outline(
                                *asset,
                                skeleton,
                                transform,
                                skin_matrices,
                                size,
                                view,
                            )?,
                        )
                    }
                };
                instances.push(InstanceOutline {
                    asset,
                    instance: draw.instance(),
                    polys,
                });
            }
            Ok(Self {
                size,
                view,
                instances,
                polygons,
                #[cfg(all(test, target_os = "windows"))]
                instance_polygon_ends,
            })
        }

        /// Pads and integerizes only a candidate selected for installation.
        /// This is idempotent, including empty/zero-sized client geometry.
        pub(super) fn materialize(&mut self) {
            self.polygons.clear();
            #[cfg(all(test, target_os = "windows"))]
            self.instance_polygon_ends.clear();
            let [width, height] = self.size.map(|n| n as f32);
            for instance in &self.instances {
                for poly in &instance.polys {
                    // Pad before clipping, preserving the margin at window edges.
                    let padded = pad_polygon(poly, HIT_PADDING_PX);
                    let clipped = clip_to_client(&padded, width, height);
                    let region = region_polygon(&clipped, width, height);
                    if region.len() >= 3 {
                        self.polygons.push(region);
                    }
                }
                #[cfg(all(test, target_os = "windows"))]
                self.instance_polygon_ends.push(self.polygons.len());
            }
        }

        /// Classifies movement against the last successfully installed geometry.
        /// Structural changes have no correspondence proof and cannot be deferred.
        pub(super) fn change_since(&self, previous: &Self) -> RegionChange {
            if self.size != previous.size
                || self.view != previous.view
                || self.instances.len() != previous.instances.len()
            {
                return RegionChange::Immediate;
            }
            let mut change = RegionChange::Unchanged;
            for (current, previous) in self.instances.iter().zip(&previous.instances) {
                match current.change_since(previous) {
                    RegionChange::Immediate => return RegionChange::Immediate,
                    RegionChange::WithinMargin => change = RegionChange::WithinMargin,
                    RegionChange::Unchanged => {}
                }
            }
            change
        }

        #[cfg(all(test, target_os = "windows"))]
        pub(super) fn changed_since(&self, previous: &Self) -> bool {
            self.change_since(previous) != RegionChange::Unchanged
        }

        /// The padded integer polygons to install as the window region, in
        /// physical client coordinates.
        pub(super) fn polygons(&self) -> &[Vec<[i32; 2]>] {
            &self.polygons
        }

        /// Exclusive end of every instance's polygons in [`HitGeometry::polygons`],
        /// in frame order (probe-only; see the `instance_polygon_ends` field).
        #[cfg(all(test, target_os = "windows"))]
        pub(super) fn instance_polygon_ends(&self) -> &[usize] {
            &self.instance_polygon_ends
        }
    }

    fn asset_model(
        simulation: &Simulation,
        asset: RenderAssetId,
    ) -> Result<&CompiledModel, String> {
        simulation
            .asset(asset)
            .map(|model| model.as_ref())
            .ok_or_else(|| format!("asset {asset} is drawn but not registered in the simulation"))
    }

    /// Screen-space triangles of a 2D instance: every non-transparent layer's
    /// deformed vertices grouped by the compiled triangle list.
    fn sprite_outline(
        asset: RenderAssetId,
        sprite: &CompiledSprite2d,
        transform: &Mat4,
        layers: &[Vec<[f32; 2]>],
        size: [u32; 2],
        view: SceneView,
    ) -> Result<Vec<Vec<[f32; 2]>>, String> {
        if sprite.layers.len() != layers.len() {
            return Err(format!(
                "asset {asset} declares {} layers but the frame deformed {}",
                sprite.layers.len(),
                layers.len()
            ));
        }
        let width = size[0].max(1) as f32;
        let height = size[1].max(1) as f32;
        let mut polys = Vec::new();
        for (layer, vertices) in sprite.layers.iter().zip(layers) {
            if layer.color[3] <= 0.0 || !layer.texture_visible {
                // A fully transparent layer draws nothing and must not intercept.
                continue;
            }
            if layer.indices.len() % 3 != 0 {
                return Err(format!(
                    "asset {asset} layer '{}' has {} indices, not a triangle list",
                    layer.name,
                    layer.indices.len()
                ));
            }
            for triangle in layer.indices.as_chunks::<3>().0 {
                let mut corners = [[0.0f32; 3]; 3];
                for (corner, index) in triangle.iter().enumerate() {
                    let vertex = vertices.get(*index as usize).ok_or_else(|| {
                    format!(
                        "asset {asset} layer '{}' references vertex {index} but only {} were deformed",
                        layer.name,
                        vertices.len()
                    )
                })?;
                    let world = transform.transform_point3(Vec3::new(vertex[0], vertex[1], 0.0));
                    if !world.is_finite() {
                        return Err(format!(
                            "asset {asset} layer '{}' deforms to a non-finite position",
                            layer.name
                        ));
                    }
                    corners[corner] = [world.x, world.y, world.z];
                }
                push_triangle(&mut polys, corners, size, view, width, height)
                    .map_err(|error| format!("asset {asset} layer '{}': {error}", layer.name))?;
            }
        }
        Ok(polys)
    }

    /// Screen-space triangles of a 3D instance: bind-space vertices skinned with
    /// the frame's matrices (exactly as picking does), grouped by the compiled
    /// triangle list and transformed by the instance root.
    fn skeleton_outline(
        asset: RenderAssetId,
        skeleton: &CompiledSkeleton3d,
        transform: &Mat4,
        skin_matrices: &[Mat4],
        size: [u32; 2],
        view: SceneView,
    ) -> Result<Vec<Vec<[f32; 2]>>, String> {
        if !skeleton.visible {
            return Ok(Vec::new());
        }
        let width = size[0].max(1) as f32;
        let height = size[1].max(1) as f32;
        if !skeleton.indices.len().is_multiple_of(3) {
            return Err(format!(
                "asset {asset} has {} indices, not a triangle list",
                skeleton.indices.len()
            ));
        }
        let mut polys = Vec::new();
        // Per-instance, per-frame: shared triangle corners reuse the exact same
        // skinning sum. Lazy population preserves first-error order and never
        // evaluates an unreferenced vertex.
        let mut world_vertices = vec![None; skeleton.vertices.len()];
        for triangle in skeleton.indices.as_chunks::<3>().0 {
            let mut corners = [[0.0f32; 3]; 3];
            for (corner, index) in triangle.iter().enumerate() {
                let cached = world_vertices.get_mut(*index as usize).ok_or_else(|| {
                    format!(
                        "asset {asset} references vertex {index} but only {} exist",
                        skeleton.vertices.len()
                    )
                })?;
                let world = if let Some(world) = *cached {
                    world
                } else {
                    let vertex = &skeleton.vertices[*index as usize];
                    let bind = Vec3::from_array(vertex.position);
                    let mut position = Vec3::ZERO;
                    for (joint, weight) in vertex.joints.iter().zip(vertex.weights) {
                        let skin = skin_matrices.get(*joint as usize).ok_or_else(|| {
                        format!(
                            "asset {asset} vertex {index} references joint {joint} but only {} skin matrices are ready",
                            skin_matrices.len()
                        )
                    })?;
                        position += skin.transform_point3(bind) * weight;
                    }
                    let world = transform.transform_point3(position);
                    if !world.is_finite() {
                        return Err(format!(
                            "asset {asset} vertex {index} skins to a non-finite position"
                        ));
                    }
                    let world = world.to_array();
                    *cached = Some(world);
                    world
                };
                corners[corner] = world;
            }
            push_triangle(&mut polys, corners, size, view, width, height)
                .map_err(|error| format!("asset {asset}: {error}"))?;
        }
        Ok(polys)
    }

    /// Clips one world-space triangle to the camera's depth slab, projects it and
    /// appends the visible, non-degenerate screen-space polygon to `outlines`.
    fn push_triangle(
        outlines: &mut Vec<Vec<[f32; 2]>>,
        triangle: [[f32; 3]; 3],
        size: [u32; 2],
        view: SceneView,
        width: f32,
        height: f32,
    ) -> Result<(), String> {
        let Some(visible) = clip_depth(&triangle) else {
            return Ok(());
        };
        let mut projected = Vec::with_capacity(visible.len());
        for point in &visible {
            let screen = world_to_screen_with([point[0], point[1]], size, view);
            if !screen[0].is_finite() || !screen[1].is_finite() {
                return Err("a vertex projects to a non-finite screen position".to_owned());
            }
            projected.push(screen);
        }
        let projected = clip_to_client(&projected, width, height);
        if projected.len() < 3 {
            return Ok(());
        }
        if doubled_area(&projected) == 0.0 {
            // Exactly zero-area projections are invisible. Thin but non-zero
            // slivers are kept on purpose.
            return Ok(());
        }
        outlines.push(projected);
        Ok(())
    }

    /// Clips a triangle against the camera's near and far planes in world space,
    /// returning `None` when nothing of it is visible.
    fn clip_depth(triangle: &[[f32; 3]; 3]) -> Option<Vec<[f32; 3]>> {
        let poly = clip_polygon3(triangle, |point| DEPTH_NEAR_Z - point[2]);
        if poly.len() < 3 {
            return None;
        }
        let poly = clip_polygon3(&poly, |point| point[2] - DEPTH_FAR_Z);
        if poly.len() < 3 {
            return None;
        }
        Some(poly)
    }

    /// Sutherland-Hodgman clip of a convex polygon against one 3D half-plane,
    /// keeping the side where `side(point) >= 0` and interpolating crossing edges.
    fn clip_polygon3(poly: &[[f32; 3]], side: impl Fn(&[f32; 3]) -> f32) -> Vec<[f32; 3]> {
        let mut out = Vec::with_capacity(poly.len() + 1);
        for (index, current) in poly.iter().enumerate() {
            let previous = &poly[(index + poly.len() - 1) % poly.len()];
            let current_side = side(current);
            let previous_side = side(previous);
            if current_side >= 0.0 {
                if previous_side < 0.0 {
                    out.push(lerp3(
                        previous,
                        current,
                        previous_side / (previous_side - current_side),
                    ));
                }
                out.push(*current);
            } else if previous_side >= 0.0 {
                out.push(lerp3(
                    previous,
                    current,
                    previous_side / (previous_side - current_side),
                ));
            }
        }
        out
    }

    /// Sutherland-Hodgman clip of a convex polygon against the client rectangle.
    /// Vertices are never clamped here: clipping keeps the true silhouette.
    fn clip_to_client(poly: &[[f32; 2]], width: f32, height: f32) -> Vec<[f32; 2]> {
        let poly = clip_polygon2(poly, |point| point[0]);
        let poly = clip_polygon2(&poly, |point| width - point[0]);
        let poly = clip_polygon2(&poly, |point| point[1]);
        clip_polygon2(&poly, |point| height - point[1])
    }

    /// Sutherland-Hodgman clip of a convex polygon against one 2D half-plane,
    /// keeping the side where `side(point) >= 0` and interpolating crossing edges.
    fn clip_polygon2(poly: &[[f32; 2]], side: impl Fn([f32; 2]) -> f32) -> Vec<[f32; 2]> {
        let mut out = Vec::with_capacity(poly.len() + 1);
        for (index, &current) in poly.iter().enumerate() {
            let previous = poly[(index + poly.len() - 1) % poly.len()];
            let current_side = side(current);
            let previous_side = side(previous);
            if current_side >= 0.0 {
                if previous_side < 0.0 {
                    out.push(lerp2(
                        previous,
                        current,
                        previous_side / (previous_side - current_side),
                    ));
                }
                out.push(current);
            } else if previous_side >= 0.0 {
                out.push(lerp2(
                    previous,
                    current,
                    previous_side / (previous_side - current_side),
                ));
            }
        }
        out
    }

    fn lerp2(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
    }

    fn lerp3(a: &[f32; 3], b: &[f32; 3], t: f32) -> [f32; 3] {
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    }

    /// Twice the signed area of a polygon (shoelace); the sign follows winding.
    fn doubled_area(poly: &[[f32; 2]]) -> f32 {
        let mut sum = 0.0;
        for (index, &[x, y]) in poly.iter().enumerate() {
            let [next_x, next_y] = poly[(index + 1) % poly.len()];
            sum += x * next_y - next_x * y;
        }
        sum
    }

    /// Minkowski sum of a convex polygon with the axis-aligned square
    /// `[-pad, pad]^2`, as the convex hull of every vertex offset by the four
    /// square corners. The result has exactly `pad` pixels of margin along each
    /// axis and no unbounded miter spikes on sharp corners.
    fn pad_polygon(poly: &[[f32; 2]], pad: f32) -> Vec<[f32; 2]> {
        let mut points = Vec::with_capacity(poly.len() * 4);
        for &[x, y] in poly {
            points.push([x - pad, y - pad]);
            points.push([x + pad, y - pad]);
            points.push([x - pad, y + pad]);
            points.push([x + pad, y + pad]);
        }
        convex_hull(&mut points)
    }

    /// Rounds a padded polygon to integer client coordinates without shrinking
    /// it: every vertex contributes its unit-cell corners, whose convex hull
    /// contains the float polygon (each vertex lies inside its own cell), so no
    /// visible margin is lost to rounding. `poly` is already clipped to the
    /// client, so the final clamp is only a safety net against float drift.
    fn region_polygon(poly: &[[f32; 2]], width: f32, height: f32) -> Vec<[i32; 2]> {
        let mut points = Vec::with_capacity(poly.len() * 4);
        for &[x, y] in poly {
            let (x0, x1) = (x.floor(), x.ceil());
            let (y0, y1) = (y.floor(), y.ceil());
            points.push([x0, y0]);
            points.push([x1, y0]);
            points.push([x0, y1]);
            points.push([x1, y1]);
        }
        convex_hull(&mut points)
            .into_iter()
            .map(|point| {
                [
                    point[0].clamp(0.0, width) as i32,
                    point[1].clamp(0.0, height) as i32,
                ]
            })
            .collect()
    }

    /// Andrew's monotone chain convex hull with collinear points removed. All
    /// coordinates must be finite.
    fn convex_hull(points: &mut Vec<[f32; 2]>) -> Vec<[f32; 2]> {
        points.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        points.dedup();
        if points.len() <= 2 {
            return points.clone();
        }
        let mut hull: Vec<[f32; 2]> = Vec::with_capacity(points.len() * 2);
        for &point in points.iter() {
            while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0.0
            {
                hull.pop();
            }
            hull.push(point);
        }
        let threshold = hull.len() + 1;
        for &point in points.iter().rev().skip(1) {
            while hull.len() >= threshold
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0.0
            {
                hull.pop();
            }
            hull.push(point);
        }
        hull.pop();
        hull
    }

    fn cross(origin: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
        (a[0] - origin[0]) * (b[1] - origin[1]) - (a[1] - origin[1]) * (b[0] - origin[0])
    }

    pub(super) fn hull_bits(mut points: Vec<[f32; 2]>) -> Vec<[u32; 2]> {
        convex_hull(&mut points)
            .iter()
            .map(|p| p.map(f32::to_bits))
            .collect()
    }

    pub(super) fn client_clip(points: &[[f32; 2]], width: f32, height: f32) -> Vec<[f32; 2]> {
        clip_to_client(points, width, height)
    }

    pub(super) fn raw_bits(geometry: &HitGeometry) -> Vec<Vec<Vec<[u32; 2]>>> {
        geometry
            .instances
            .iter()
            .map(|instance| {
                instance
                    .polys
                    .iter()
                    .map(|poly| poly.iter().map(|p| p.map(f32::to_bits)).collect())
                    .collect()
            })
            .collect()
    }

    pub(super) fn materialize_polys(polys: &[Vec<[f32; 2]>], size: [u32; 2]) -> Vec<Vec<[i32; 2]>> {
        let [width, height] = size.map(|n| n as f32);
        polys
            .iter()
            .filter_map(|poly| {
                let padded = pad_polygon(poly, HIT_PADDING_PX);
                let clipped = clip_to_client(&padded, width, height);
                let region = region_polygon(&clipped, width, height);
                (region.len() >= 3).then_some(region)
            })
            .collect()
    }

    pub(super) fn triangle(
        triangle: [[f32; 3]; 3],
        size: [u32; 2],
        view: SceneView,
    ) -> Result<Vec<Vec<[f32; 2]>>, String> {
        let mut result = Vec::new();
        push_triangle(
            &mut result,
            triangle,
            size,
            view,
            size[0].max(1) as f32,
            size[1].max(1) as f32,
        )?;
        Ok(result)
    }
}
