//! P0 decision gate for the exact native region build: the integer scanline
//! candidate against the production chunk-41 path and the independent
//! sequential oracle, on real Eous geometry and on resource-boundary cases.
//!
//! The candidate is test-only until every gate here passes; see the plan
//! and `.artifacts/eous-region-native-20261017-190924/`.
//!
//! Two layers are checked separately:
//! - the fill itself, compared against GDI on arbitrary integer polygons;
//! - the production decision, which must accept real Eous frames and must
//!   decline non-convex, self-intersecting and oversized input instead of
//!   scanning it (a single batch's winding accumulates across polygons, so
//!   a self-intersecting polygon can cancel a neighbour; the OR fast path
//!   and the batch are only equivalent on simple polygons).

use std::time::{Duration, Instant};

use super::*;
use crate::platform::scan_probe;

fn ms(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1000.0
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

/// A scanned build measured end to end through the production decision:
/// applicability, the exact fill, RGNDATA packing and the single
/// `ExtCreateRegion`, but not `SetWindowRgn`.
fn timed_scan(polygons: &[Vec<[i32; 2]>]) -> f64 {
    let start = Instant::now();
    let region = scan_probe::classify_frame(polygons)
        .unwrap()
        .unwrap_or_else(|decline| {
            panic!(
                "a timed real frame was declined by the fast path: {}",
                decline.reason()
            )
        });
    drop(region);
    ms(start.elapsed())
}

fn timed_chunked(polygons: &[Vec<[i32; 2]>]) -> f64 {
    let start = Instant::now();
    probe_region_build(polygons, ProbedPlan::Chunked(41)).unwrap();
    ms(start.elapsed())
}

/// Andrew's monotone chain over integer points, used only to generate
/// convex random polygons so the boundary matrix never feeds a
/// self-intersecting shape to the OR-vs-batch comparison.
fn hull_i32(points: &mut Vec<[i32; 2]>) -> Vec<[i32; 2]> {
    points.sort_unstable();
    points.dedup();
    if points.len() <= 2 {
        return points.clone();
    }
    let turn = |o: [i32; 2], a: [i32; 2], b: [i32; 2]| {
        i64::from(a[0] - o[0]) * i64::from(b[1] - o[1])
            - i64::from(a[1] - o[1]) * i64::from(b[0] - o[0])
    };
    let mut hull: Vec<[i32; 2]> = Vec::new();
    for &point in points.iter() {
        while hull.len() >= 2 && turn(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0 {
            hull.pop();
        }
        hull.push(point);
    }
    let lower = hull.len() + 1;
    for &point in points.iter().rev().skip(1) {
        while hull.len() >= lower && turn(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0 {
            hull.pop();
        }
        hull.push(point);
    }
    hull.pop();
    hull
}

/// Real Eous animation frames at one explicit view, with the polygons
/// materialized exactly as production would before installing a region.
fn eous_frames(steps: usize, size: [u32; 2]) -> Vec<Vec<Vec<[i32; 2]>>> {
    eous_frames_at(
        steps,
        size,
        crate::renderer::desktop_view_at(size[1], 1.5, 0.0, 0.0),
    )
}

/// [`eous_frames`] under an explicit view: the preview capture runs `--bangboo`
/// in a 960x640 bounded window, which uses [`crate::renderer::LEGACY`] and not
/// the desktop view, so equivalence has to be shown under that view too.
fn eous_frames_at(
    steps: usize,
    size: [u32; 2],
    view: crate::renderer::SceneView,
) -> Vec<Vec<Vec<[i32; 2]>>> {
    let mut simulation = temporal::eous_scene();
    let mut frames = Vec::with_capacity(steps);
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
        let mut geometry =
            HitGeometry::collect_frame_with_view(&simulation, &frame, size, view).unwrap();
        geometry.materialize().unwrap();
        frames.push(geometry.polygons().to_vec());
    }
    frames
}

/// `count` one-pixel squares in a grid, all strictly convex.
fn squares(count: usize) -> Vec<Vec<[i32; 2]>> {
    let mut polygons = Vec::with_capacity(count);
    for index in 0..count {
        let x = (index % 256) as i32 * 3;
        let y = (index / 256) as i32 * 3;
        polygons.push(vec![[x, y], [x + 1, y], [x + 1, y + 1], [x, y + 1]]);
    }
    polygons
}

#[test]
#[ignore = "P0 decision gate; run release explicitly with --ignored"]
fn region_scan_gate_probe() {
    const DESKTOP: [u32; 2] = [2560, 1600];
    const WIDE: [u32; 2] = [4160, 2560];
    const FRAMES: usize = 240;
    const WIDE_FRAMES: usize = 40;
    const ROUNDS: usize = 3;
    const TIMED_FRAMES: usize = 60;

    // ---- E1: boundary matrix and seeded random convex sets -------------
    let rule = scan_probe::measured_rule();
    let mut boundary_cases: Vec<Vec<Vec<[i32; 2]>>> = vec![
        vec![],
        // Degenerate: collinear and repeated vertices fill nothing in both
        // the batch and the scan.
        vec![vec![[0, 0], [1, 1], [2, 2]]],
        vec![vec![[0, 0], [0, 0], [0, 0]]],
        vec![vec![[0, 0], [40, 0], [40, 40], [0, 40]]],
        vec![
            vec![[0, 0], [40, 0], [0, 40]],
            vec![[40, 0], [40, 40], [0, 40]],
        ],
        // Ear/leg spacing: shapes with a one-pixel gap, and a concave L
        // whose notch must stay empty (simple, so the batch and the
        // per-polygon OR still agree).
        vec![
            vec![[0, 0], [20, 0], [20, 8], [8, 8], [8, 20], [0, 20]],
            vec![[30, 30], [45, 30], [45, 45], [30, 45]],
        ],
        // Overlap must stay filled (union, not XOR).
        vec![
            vec![[0, 0], [8, 0], [8, 8], [0, 8]],
            vec![[4, 4], [12, 4], [12, 12], [4, 12]],
        ],
        // GDI's signed 27-bit coordinate corners, kept local per case.
        vec![vec![
            [-(1 << 26), -(1 << 26)],
            [-(1 << 26) + 40, -(1 << 26)],
            [-(1 << 26), -(1 << 26) + 40],
        ]],
        vec![vec![
            [(1 << 26) - 40, (1 << 26) - 40],
            [(1 << 26) - 1, (1 << 26) - 40],
            [(1 << 26) - 40, (1 << 26) - 1],
        ]],
    ];

    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    fn next(seed: &mut u64, limit: i32) -> i32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed % limit as u64) as i32 - limit / 5
    }
    for _ in 0..16 {
        let mut polygons = Vec::new();
        for _ in 0..12 {
            let x = next(&mut seed, 600);
            let y = next(&mut seed, 400);
            let w = 1 + next(&mut seed, 120).unsigned_abs() as i32;
            let h = 1 + next(&mut seed, 90).unsigned_abs() as i32;
            polygons.push(match seed % 3 {
                0 => vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
                1 => vec![[x, y], [x + w, y], [x + w / 2, y + h]],
                _ => {
                    let mut points = vec![
                        [x, y],
                        [x + w, y + h / 4],
                        [x + w / 3, y + h],
                        [x + w / 5, y + h / 2],
                    ];
                    let hull = hull_i32(&mut points);
                    // A hull below four vertices is still a valid convex
                    // polygon; fewer than three cannot reach GDI.
                    if hull.len() >= 3 {
                        hull
                    } else {
                        vec![[x, y], [x + w, y], [x, y + h]]
                    }
                }
            });
        }
        boundary_cases.push(polygons);
    }
    let mut equivalent = 0_usize;
    for polygons in &boundary_cases {
        assert!(
            scan_probe::scanline_matches_chunked(polygons, rule).unwrap(),
            "boundary/random set differs from chunk-41: {polygons:?}"
        );
        assert!(
            scan_probe::scanline_matches_sequential(polygons, rule).unwrap(),
            "boundary/random set differs from sequential: {polygons:?}"
        );
        equivalent += 1;
    }
    println!("PROBE boundary_matrix cases={equivalent} status=equivalent");

    // ---- E1b: production applicability decisions ----------------------
    // A frame at the minimum density that is otherwise buildable.
    let mut min_frame = squares(scan_probe::MIN_SCAN_POLYGONS - 1);
    assert_eq!(
        scan_probe::applicable(&min_frame),
        Err(scan_probe::NotApplicable::TooFewPolygons)
    );
    min_frame.push(vec![[0, 0], [1, 0], [1, 1], [0, 1]]);
    assert_eq!(
        scan_probe::applicable(&min_frame),
        Ok(()),
        "16,384 simple convex polygons must be applicable"
    );

    // Non-convex, self-intersecting and over-long polygons decline the
    // whole frame: the fast path never scans a subset.
    let concave = vec![[0, 0], [20, 0], [20, 8], [8, 8], [8, 20], [0, 20]];
    let star = vec![
        [10, 0],
        [12, 7],
        [20, 7],
        [14, 12],
        [16, 20],
        [10, 15],
        [4, 20],
        [6, 12],
        [0, 7],
        [8, 7],
    ];
    let bowtie = vec![[0, 0], [10, 10], [10, 0], [0, 10]];
    for (name, polygon) in [("concave", &concave), ("star", &star), ("bowtie", &bowtie)] {
        let mut frame = squares(scan_probe::MIN_SCAN_POLYGONS);
        frame.push(polygon.clone());
        assert_eq!(
            scan_probe::applicable(&frame),
            Err(scan_probe::NotApplicable::NonConvex),
            "{name} input must decline instead of being scanned"
        );
    }
    assert!(!scan_probe::is_strictly_convex_simple(&[
        [10, 0],
        [12, 7],
        [20, 7],
        [14, 12],
        [16, 20],
        [10, 15],
        [4, 20],
        [6, 12],
        [0, 7],
        [8, 7],
    ]));
    // The pentagram over a regular pentagon's own vertices is the case a
    // local cross-product test alone would pass: five equal turns, same
    // vertex set as the hull, wrong order.
    let pentagram = vec![[0, 20], [12, 12], [7, 0], [17, 20], [2, 12]];
    assert!(!scan_probe::is_strictly_convex_simple(&pentagram));

    // A 33-vertex convex polygon exceeds the per-polygon cap; 32 is taken.
    let parabola: Vec<[i32; 2]> = (0..33).map(|index| [index, index * index]).collect();
    assert!(scan_probe::is_strictly_convex_simple(&parabola));
    let mut large = squares(scan_probe::MIN_SCAN_POLYGONS);
    large.push(parabola.clone());
    assert_eq!(
        scan_probe::applicable(&large),
        Err(scan_probe::NotApplicable::PolygonTooLarge)
    );
    large.pop();
    large.push(parabola[..32].to_vec());
    assert_eq!(scan_probe::applicable(&large), Ok(()));

    // Span: exactly the cap is taken, one past it declines.
    let mut span_ok = squares(scan_probe::MIN_SCAN_POLYGONS - 1);
    span_ok.push(vec![[0, 0], [16_384, 0], [16_384, 1], [0, 1]]);
    assert_eq!(scan_probe::applicable(&span_ok), Ok(()));
    span_ok.pop();
    span_ok.push(vec![[0, 0], [16_385, 0], [16_385, 1], [0, 1]]);
    assert_eq!(
        scan_probe::applicable(&span_ok),
        Err(scan_probe::NotApplicable::SpanTooLarge)
    );

    // Bounding-box area: exactly 64,000,000 pixels is taken, more declines.
    let mut area_ok = squares(scan_probe::MIN_SCAN_POLYGONS - 1);
    area_ok.push(vec![[0, 0], [8_000, 0], [8_000, 8_000], [0, 8_000]]);
    assert_eq!(scan_probe::applicable(&area_ok), Ok(()));
    area_ok.pop();
    area_ok.push(vec![[0, 0], [8_001, 0], [8_001, 8_000], [0, 8_000]]);
    assert_eq!(
        scan_probe::applicable(&area_ok),
        Err(scan_probe::NotApplicable::TooManyPixels)
    );

    // Rectangle output: each 1,638-triangle band yields nine rows of
    // distinct widths (rows 0 and 1 share a width and merge vertically), so
    // one band is 14,742 rectangles. 16 bands is 235,872 (under the 262,144
    // cap and inside the 4 MiB RGNDATA budget); 18 bands is 265,356 and must
    // decline before that output is allocated.
    fn triangle_bands(bands: i32) -> Vec<Vec<[i32; 2]>> {
        let mut polygons = Vec::new();
        for band in 0..bands {
            let y0 = band * 11;
            for column in 0..1_638 {
                let x0 = column * 10;
                polygons.push(vec![[x0, y0], [x0 + 9, y0], [x0, y0 + 10]]);
            }
        }
        polygons
    }
    let rects_ok = triangle_bands(16);
    match scan_probe::classify_frame(&rects_ok).unwrap() {
        Ok(region) => drop(region),
        Err(decline) => panic!(
            "16 tapered bands must build, declined with {}",
            decline.reason()
        ),
    }
    let rects_over = triangle_bands(18);
    match scan_probe::classify_frame(&rects_over).unwrap() {
        Ok(region) => {
            drop(region);
            panic!("18 tapered bands exceed the rectangle cap but were built");
        }
        Err(decline) => assert_eq!(decline, scan_probe::NotApplicable::TooManyRects),
    }

    // The declined shapes still build through the old path, unchanged.
    for polygons in [
        vec![concave.clone()],
        vec![star.clone()],
        vec![bowtie.clone()],
        vec![
            concave.clone(),
            vec![[30, 30], [45, 30], [45, 45], [30, 45]],
        ],
    ] {
        assert!(
            probe_region_equivalence(&polygons, ProbedPlan::Chunked(41)).unwrap(),
            "the old path must still accept declined input: {polygons:?}"
        );
    }
    println!("PROBE applicability decisions=ok");

    // ---- E2: real Eous frames at the desktop view ---------------------
    let frames = eous_frames(FRAMES, DESKTOP);
    let mut polygon_counts: Vec<usize> = frames.iter().map(Vec::len).collect();
    polygon_counts.sort_unstable();
    let max_points = frames
        .iter()
        .flat_map(|frame| frame.iter().map(Vec::len))
        .max()
        .unwrap_or(0);
    assert!(
        max_points <= scan_probe::MAX_SCAN_POLYGON_POINTS,
        "real Eous geometry needs {max_points} points in one polygon, above the fast-path cap"
    );
    println!(
        "PROBE eous_frames count={} polygons_p50={} polygons_max={} polygon_points_max={}",
        frames.len(),
        polygon_counts[polygon_counts.len() / 2],
        polygon_counts[polygon_counts.len() - 1],
        max_points
    );
    let mut accepted = 0_usize;
    let mut declined = std::collections::BTreeMap::new();
    for polygons in &frames {
        match scan_probe::classify_frame(polygons).unwrap() {
            Ok(region) => {
                drop(region);
                accepted += 1;
            }
            Err(decline) => *declined.entry(decline.reason()).or_insert(0_usize) += 1,
        }
    }
    println!("PROBE eous_applicability accepted={accepted}/{FRAMES} declined={declined:?}");
    let mut frame_scan = Vec::with_capacity(TIMED_FRAMES * ROUNDS);
    let mut frame_chunked = Vec::with_capacity(TIMED_FRAMES * ROUNDS);
    for (index, polygons) in frames.iter().enumerate() {
        assert!(
            scan_probe::scanline_matches_chunked(polygons, rule).unwrap(),
            "Eous frame {index} differs from chunk-41"
        );
    }
    println!("PROBE eous_equivalence frames={FRAMES} status=equivalent");

    // ---- wide synthetic geometry: budget and equivalence --------------
    // Same animation at a four-monitor-sized viewport; this checks geometry
    // equivalence and the resource caps, not a physical multi-display rig.
    let wide = eous_frames(WIDE_FRAMES, WIDE);
    for (index, polygons) in wide.iter().enumerate() {
        assert!(
            scan_probe::scanline_matches_chunked(polygons, rule).unwrap(),
            "wide frame {index} differs from chunk-41"
        );
    }
    let wide_max = wide.iter().map(Vec::len).max().unwrap_or(0);
    println!(
        "PROBE wide_equivalence frames={WIDE_FRAMES} polygons_max={wide_max} status=equivalent"
    );

    // ---- micro-performance: alternating paired rounds -----------------
    for _ in 0..ROUNDS {
        for polygons in frames.iter().take(TIMED_FRAMES) {
            frame_scan.push(timed_scan(polygons));
            frame_chunked.push(timed_chunked(polygons));
        }
    }
    let ratios: Vec<f64> = frame_scan
        .iter()
        .zip(&frame_chunked)
        .map(|(scan, chunked)| scan / chunked)
        .collect();
    let mut scan_times = frame_scan.clone();
    let mut chunked_times = frame_chunked.clone();
    let (scan_p50, scan_p95) = percentiles(&mut scan_times);
    let (chunked_p50, chunked_p95) = percentiles(&mut chunked_times);
    let mut ratios_sorted = ratios.clone();
    let paired = median(&mut ratios_sorted);
    println!(
        "PROBE micro frames={} samples={} scan_p50_ms={scan_p50:.3} scan_p95_ms={scan_p95:.3} \
         chunk_p50_ms={chunked_p50:.3} chunk_p95_ms={chunked_p95:.3} paired_median={paired:.4}",
        TIMED_FRAMES,
        frame_scan.len()
    );

    // Where the scan time goes: applicability, fill, RGNDATA plus the single
    // native call.
    let mut apply_times = Vec::new();
    let mut fill_times = Vec::new();
    let mut pack_times = Vec::new();
    for polygons in frames.iter().take(TIMED_FRAMES) {
        let [apply_ms, fill_ms, pack_ms, _] = scan_probe::stage_stats(polygons).unwrap();
        apply_times.push(apply_ms);
        fill_times.push(fill_ms);
        pack_times.push(pack_ms);
    }
    let (apply_p50, apply_p95) = percentiles(&mut apply_times);
    let (fill_p50, fill_p95) = percentiles(&mut fill_times);
    let (pack_p50, pack_p95) = percentiles(&mut pack_times);
    println!(
        "PROBE stages apply_p50_ms={apply_p50:.3} apply_p95_ms={apply_p95:.3} \
         fill_p50_ms={fill_p50:.3} fill_p95_ms={fill_p95:.3} \
         pack_p50_ms={pack_p50:.3} pack_p95_ms={pack_p95:.3}"
    );

    // Actual per-frame build statistics: rectangles, bytes, fill area and
    // scan work.
    let mut rects: Vec<usize> = Vec::new();
    let mut bytes: Vec<usize> = Vec::new();
    let mut pixels: Vec<u64> = Vec::new();
    let mut work: Vec<u64> = Vec::new();
    for polygons in frames.iter().take(TIMED_FRAMES) {
        let (scan, _) = scan_probe::build_stats(polygons).unwrap();
        rects.push(scan.rects.len());
        bytes.push(scan.rects.len() * 16 + 32);
        pixels.push(scan.pixels);
        work.push(scan.work);
    }
    rects.sort_unstable();
    bytes.sort_unstable();
    pixels.sort_unstable();
    work.sort_unstable();
    println!(
        "PROBE resources rects_p50={} rects_max={} bytes_max={} pixels_max={} work_max={}",
        rects[rects.len() / 2],
        rects[rects.len() - 1],
        bytes[bytes.len() - 1],
        pixels[pixels.len() - 1],
        work[work.len() - 1]
    );

    // ---- gate ---------------------------------------------------------
    assert_eq!(equivalent, boundary_cases.len());
    assert_eq!(accepted, FRAMES, "real Eous frames must all be applicable");
    assert!(
        scan_p50 <= 10.0,
        "fast path p50 {scan_p50:.3}ms exceeds the registered 10ms gate"
    );
    assert!(
        scan_p95 <= 15.0,
        "fast path p95 {scan_p95:.3}ms exceeds the registered 15ms gate"
    );
    assert!(
        paired <= 0.25,
        "fast path paired median {paired:.4} exceeds the registered 0.25 gate"
    );
    println!("PROBE gate status=passed");
}

/// Exactness under the preview's own conditions.
///
/// The acceptance capture runs `--bangboo` in a 960x640 bounded window, which
/// uses [`crate::renderer::LEGACY`] and therefore a different projection from
/// the desktop view the main gate exercises. The fast path is accepted on the
/// claim that it reproduces the installed set exactly, so that claim has to
/// hold for the view a real window actually uses: the same integer polygons
/// must produce the same region through the scan and through chunk-41.
#[test]
#[ignore = "preview-view equivalence probe; run release explicitly with --ignored"]
fn region_scan_preview_view_probe() {
    const SIZE: [u32; 2] = [960, 640];
    const STEPS: usize = 240;
    let rule = scan_probe::measured_rule();
    let frames = eous_frames_at(STEPS, SIZE, crate::renderer::LEGACY);
    let mut compared = 0_usize;
    let mut declined = 0_usize;
    for (index, polygons) in frames.iter().enumerate() {
        match scan_probe::applicable(polygons) {
            Ok(()) => {}
            Err(decline) => {
                // Below the density gate is a legitimate decline, not a pass.
                println!("PROBE preview frame={index} declined={}", decline.reason());
                declined += 1;
                continue;
            }
        }
        assert!(
            scan_probe::scanline_matches_chunked(polygons, rule).unwrap(),
            "preview-view frame {index} differs from chunk-41 ({} polygons)",
            polygons.len()
        );
        assert!(
            scan_probe::scanline_matches_sequential(polygons, rule).unwrap(),
            "preview-view frame {index} differs from sequential ({} polygons)",
            polygons.len()
        );
        compared += 1;
    }
    println!(
        "PROBE preview view=legacy size=960x640 compared={compared} declined={declined} status=equivalent"
    );
    assert_eq!(
        declined, 0,
        "{declined} preview frames fell below the density gate; the probe did not exercise the fast path"
    );
}
