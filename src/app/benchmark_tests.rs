use super::*;
use crate::app::cli::BenchMode;
use crate::renderer::SceneSkipReason;

fn options(reloads: Option<u32>) -> BenchOptions {
    BenchOptions {
        instances: 4,
        mode: BenchMode::Pixel,
        seconds: reloads.is_none().then_some(Duration::from_secs(30)),
        reloads,
    }
}

fn diagnostics() -> GraphicsDiagnostics {
    GraphicsDiagnostics {
        adapter: "test hardware".into(),
        backend: "vulkan".into(),
        device_type: "DiscreteGpu".into(),
        physical_size: [960, 640],
        control_physical_size: [380, 680],
        present_mode: "Fifo".into(),
        alpha_mode: "PreMultiplied".into(),
        format: "Bgra8Unorm".into(),
    }
}

fn benchmark(reloads: Option<u32>, capacity: usize) -> Benchmark {
    let mut benchmark = Benchmark::with_capacities(options(reloads), capacity, capacity, false);
    benchmark
        .configure(diagnostics(), [960, 640], 1.0, 1.0, false)
        .unwrap();
    benchmark
}

fn measured_sample(outcome: SceneFrameOutcome) -> SceneSample {
    let mut sample = SceneSample::new(Phase::Measure, Duration::from_millis(17));
    sample.outcome = Some(outcome);
    sample.advance = Some(Duration::from_micros(100));
    sample.steps = 1;
    sample
}

fn start_reloads(count: u32) -> Benchmark {
    let mut b = benchmark(Some(count), 10);
    b.on_submission(0.25, Some(1), std::iter::empty()).unwrap();
    b.update(10.25).unwrap();
    b.update(15.25).unwrap();
    assert_eq!(b.phase, Phase::Reload);
    b
}

#[test]
fn fixed_layout_is_balanced_bounded_and_deterministic() {
    for count in [4, 16, 32, 64] {
        let sprites = positions(count, ModelKind::Sprite2d);
        let skeletons = positions(count, ModelKind::Skeleton3d);
        assert_eq!(sprites.len(), count / 2);
        assert_eq!(skeletons.len(), count / 2);
        assert_eq!(sprites, positions(count, ModelKind::Sprite2d));
        for position in sprites.iter().chain(&skeletons) {
            assert!(position.x >= -1.8001 && position.x <= 1.8001);
            assert!(position.y >= -0.2001 && position.y <= 0.6001);
            assert_eq!(position.z, 0.0);
        }
        let mut all = sprites;
        all.extend(skeletons);
        for (index, position) in all.iter().enumerate() {
            assert!(!all[index + 1..].contains(position));
        }
    }
    assert!(positions(8, ModelKind::Sprite2d).is_empty());
}

#[test]
fn setup_window_changes_cannot_keep_the_frozen_configuration_valid() {
    let sizes = [[960, 640], [380, 680]];
    let scales = [1.0, 1.0];
    let mut unchanged = benchmark(None, 1);
    assert!(unchanged.validate_windows(sizes, scales).is_ok());
    unchanged
        .on_submission(0.25, Some(1), std::iter::empty())
        .unwrap();
    assert!(unchanged.validate_windows(sizes, scales).is_ok());
    unchanged.update(10.25).unwrap();
    assert!(unchanged.validate_windows(sizes, scales).is_ok());

    for (actual_sizes, actual_scales) in [
        ([[1280, 640], [380, 680]], scales),
        ([[960, 640], [400, 680]], scales),
        (sizes, [1.25, 1.0]),
        (sizes, [1.0, 1.25]),
    ] {
        let mut changed = benchmark(None, 1);
        changed
            .on_submission(0.25, Some(1), std::iter::empty())
            .unwrap();
        let error = changed
            .validate_windows(actual_sizes, actual_scales)
            .unwrap_err();
        changed.fail(error);
        assert_eq!(changed.phase, Phase::Failed);
        let result = changed.result();
        assert_eq!(result["valid"], false);
        assert_eq!(
            result["config"]["physical_size"],
            serde_json::json!(sizes[0])
        );
        assert_eq!(
            result["config"]["control_physical_size"],
            serde_json::json!(sizes[1])
        );
        assert_eq!(result["config"]["scale_factor"], scales[0]);
        assert_eq!(result["config"]["control_scale_factor"], scales[1]);
    }
}

#[test]
fn nearest_rank_statistics_and_empty_samples_are_explicit() {
    let stats = statistics([5, 1, 3, 2, 4].into_iter().map(Duration::from_millis));
    assert_eq!(stats.count, 5);
    assert_eq!(stats.mean_ms, Some(3.0));
    assert_eq!(stats.p50_ms, Some(3.0));
    assert_eq!(stats.p95_ms, Some(5.0));
    assert_eq!(stats.p99_ms, Some(5.0));
    let empty = statistics(std::iter::empty());
    assert_eq!(empty.count, 0);
    assert!(empty.mean_ms.is_none() && empty.p50_ms.is_none() && empty.max_ms.is_none());
    let zero = statistics([Duration::ZERO].into_iter());
    assert_eq!(zero.count, 1);
    assert_eq!(zero.mean_ms, Some(0.0));
}

#[test]
fn skipped_attempts_keep_intervals_but_not_unexecuted_stages() {
    let mut b = benchmark(None, 4);
    b.record_scene(measured_sample(SceneFrameOutcome::Submitted))
        .unwrap();
    let mut skipped = measured_sample(SceneFrameOutcome::Skipped(SceneSkipReason::Timeout));
    skipped.advance = None;
    b.record_scene(skipped).unwrap();
    b.record_control(Phase::Measure, Duration::from_millis(2))
        .unwrap();
    let result = b.result();
    assert_eq!(result["scene"]["attempts"], 2);
    assert_eq!(result["scene"]["submitted"], 1);
    assert_eq!(result["scene"]["skipped"], 1);
    assert_eq!(result["scene"]["skip_reasons"]["timeout"], 1);
    assert_eq!(result["stages"]["frame_interval"]["count"], 2);
    assert_eq!(result["stages"]["advance"]["count"], 1);
    assert_eq!(result["stages"]["submit"]["count"], 0);
    assert!(result["stages"]["submit"]["mean_ms"].is_null());
    assert_eq!(result["stages"]["control_callback"]["count"], 1);
    assert!(result["gpu_execution_ms"].is_null());
}

#[test]
fn sample_storage_is_fixed_and_warmup_does_not_consume_it() {
    let mut b = benchmark(None, 2);
    b.record_scene(SceneSample::new(Phase::Warmup, Duration::ZERO))
        .unwrap();
    b.record_control(Phase::Warmup, Duration::from_secs(1))
        .unwrap();
    assert_eq!(b.scene.used, 0);
    assert_eq!(b.control.used, 0);
    let before = b.result()["sampler"]["allocated_bytes"].clone();
    for _ in 0..2 {
        b.record_scene(measured_sample(SceneFrameOutcome::Submitted))
            .unwrap();
    }
    assert!(
        b.record_scene(measured_sample(SceneFrameOutcome::Submitted))
            .is_err()
    );
    for _ in 0..2 {
        b.record_control(Phase::Measure, Duration::ZERO).unwrap();
    }
    assert!(b.record_control(Phase::Measure, Duration::ZERO).is_err());
    assert_eq!(b.result()["sampler"]["allocated_bytes"], before);
    assert_eq!(b.scene.values.len(), 2);
    assert_eq!(b.control.values.len(), 2);
}

#[test]
fn scene_and_control_sample_capacities_are_independent() {
    for (scene_capacity, control_capacity) in [(1, 3), (3, 1)] {
        let mut b =
            Benchmark::with_capacities(options(None), scene_capacity, control_capacity, false);
        let scene_storage = b.scene.values.as_ptr();
        let control_storage = b.control.values.as_ptr();
        let allocated_bytes = scene_capacity * size_of::<SceneSample>()
            + control_capacity * size_of::<ControlSample>();
        for _ in 0..scene_capacity {
            b.record_scene(measured_sample(SceneFrameOutcome::Submitted))
                .unwrap();
        }
        assert!(
            b.record_scene(measured_sample(SceneFrameOutcome::Submitted))
                .is_err()
        );
        for _ in 0..control_capacity {
            b.record_control(Phase::Measure, Duration::ZERO).unwrap();
        }
        assert!(b.record_control(Phase::Measure, Duration::ZERO).is_err());
        assert_eq!(b.scene.values.as_ptr(), scene_storage);
        assert_eq!(b.control.values.as_ptr(), control_storage);
        let result = b.result();
        let sampler = &result["sampler"];
        assert_eq!(sampler["scene_capacity"], scene_capacity);
        assert_eq!(sampler["control_capacity"], control_capacity);
        assert_eq!(sampler["scene_used"], scene_capacity);
        assert_eq!(sampler["control_used"], control_capacity);
        assert_eq!(sampler["allocated_bytes"], allocated_bytes);
    }
}

#[test]
fn warmup_begins_on_ready_submission_and_phase_deadlines_use_actual_time() {
    let mut b = benchmark(None, 2);
    b.on_submission(0.2, None, std::iter::empty()).unwrap();
    assert_eq!(b.phase, Phase::Setup);
    b.on_submission(0.25, Some(1), std::iter::empty()).unwrap();
    assert_eq!(b.phase, Phase::Warmup);
    b.update(10.249).unwrap();
    assert_eq!(b.phase, Phase::Warmup);
    b.update(10.25).unwrap();
    assert_eq!(b.phase, Phase::Measure);
    b.record_scene(measured_sample(SceneFrameOutcome::Submitted))
        .unwrap();
    b.update(40.249).unwrap();
    assert_eq!(b.phase, Phase::Measure);
    b.update(40.5).unwrap();
    assert_eq!(b.phase, Phase::Done);
    let result = b.result();
    assert_eq!(result["measured_seconds"], 30.25);
    assert_eq!(result["valid"], true);
    assert_eq!(result["scene"]["submitted_per_second"], json!(1.0 / 30.25));
}

#[test]
fn replacement_requires_matching_request_asset_instance_and_real_submission() {
    let mut b = start_reloads(2);
    assert_eq!(b.reload_due(15.25), Some((1, true)));
    b.begin_reload(7, 1, 15.25).unwrap();
    assert!(b.begin_reload(8, 1, 15.25).is_err());
    assert!(b.load_succeeded(8, 30).is_err());
    assert_eq!(b.successful, 0);
    assert!(
        b.on_submission(15.26, None, [(1, 30)].into_iter())
            .unwrap()
            .is_none()
    );
    b.load_succeeded(7, 30).unwrap();
    assert!(b.load_succeeded(7, 30).is_err());
    assert!(
        b.on_submission(15.27, None, [(2, 30), (1, 29)].into_iter())
            .unwrap()
            .is_none()
    );
    assert!(b.reload_due(15.5).is_none());
    let first = b
        .on_submission(15.3, None, [(1, 30)].into_iter())
        .unwrap()
        .unwrap();
    assert_eq!(
        (first.request, first.instance, first.asset, first.cycle),
        (7, 1, 30, 1)
    );
    assert!(b.reload_due(15.49).is_none());
    assert_eq!(b.reload_due(15.5), Some((1, false)));
    b.begin_reload(8, 1, 15.5).unwrap();
    b.load_succeeded(8, 31).unwrap();
    let second = b
        .on_submission(15.6, None, [(1, 31)].into_iter())
        .unwrap()
        .unwrap();
    assert_eq!(second.cycle, 2);
    assert_eq!(b.phase, Phase::Settle);
    assert!(b.reload_due(20.0).is_none());
    b.update(25.599).unwrap();
    assert_eq!(b.phase, Phase::Settle);
    b.update(25.601).unwrap();
    assert_eq!(b.phase, Phase::Done);
    assert_eq!(
        b.result()["reloads"],
        json!({"requested":2,"successful":2,"render_confirmed":2,"expected":2})
    );
}

#[test]
fn pending_zero_is_not_a_success_or_render_confirmation() {
    let mut b = start_reloads(1);
    b.begin_reload(7, 1, 15.25).unwrap();
    b.update(16.0).unwrap();
    assert_eq!(b.successful, 0);
    assert_eq!(b.confirmed, 0);
    assert!(b.update(30.251).is_err());
    assert_ne!(b.phase, Phase::Done);
}

#[test]
fn setup_and_overall_reload_timeouts_are_reported() {
    let mut b = benchmark(None, 2);
    assert!(b.update(30.001).is_err());
    let mut b = start_reloads(100);
    assert!(b.update(165.251).is_err());
}

#[test]
fn failures_or_no_submissions_cannot_be_complete_results() {
    let mut b = benchmark(None, 2);
    b.on_submission(0.25, Some(1), std::iter::empty()).unwrap();
    b.update(10.25).unwrap();
    b.record_scene(measured_sample(SceneFrameOutcome::Skipped(
        SceneSkipReason::Occluded,
    )))
    .unwrap();
    b.update(40.25).unwrap();
    assert_eq!(b.result()["valid"], false);
    let mut b = benchmark(None, 2);
    b.record_scene(measured_sample(SceneFrameOutcome::Submitted))
        .unwrap();
    b.fail("early close".into());
    assert_eq!(b.result()["valid"], false);
    assert_eq!(b.result()["failure"], "early close");
    b.fail("secondary error".into());
    assert_eq!(b.result()["failure"], "early close");
}

#[test]
fn error_attempts_and_region_failures_are_not_hidden_as_skips() {
    let mut b = benchmark(None, 2);
    let mut sample = SceneSample::new(Phase::Measure, Duration::from_millis(100));
    sample.region = RegionDecision::Failed;
    sample.dropped_steps = 3;
    b.record_scene(sample).unwrap();
    let result = b.result();
    assert_eq!(result["scene"]["attempts"], 1);
    assert_eq!(result["scene"]["errors"], 1);
    assert_eq!(result["scene"]["skipped"], 0);
    assert_eq!(result["regions"]["failed"], 1);
    assert_eq!(result["simulation"]["dropped_steps"], 3);
}

#[test]
fn live_references_and_latest_example_cache_determine_retention() {
    assert_eq!(
        expected_assets([1, 1, 2, 2].into_iter(), &[Some(1), Some(2)]),
        2
    );
    assert_eq!(
        expected_assets([3, 1, 2, 2].into_iter(), &[Some(1), Some(3)]),
        3
    );
    assert_eq!(
        expected_assets([4, 1, 2, 2].into_iter(), &[Some(4), Some(3)]),
        4
    );
    assert_eq!(
        expected_assets([5, 1, 2, 2].into_iter(), &[Some(4), Some(5)]),
        4
    );
    assert_eq!(expected_assets(std::iter::empty(), &[None, None]), 0);
    assert_eq!(
        expected_assets([1, 1, 2, 2].into_iter(), &[Some(1), Some(2), None, None]),
        2
    );
    assert_eq!(
        expected_assets([5, 1, 2].into_iter(), &[Some(1), Some(2), Some(3), Some(4)]),
        5
    );
}

#[test]
fn result_config_echoes_the_wander_provenance() {
    // The stationary default records wander=false.
    assert_eq!(benchmark(None, 1).result()["config"]["wander"], false);
    // A wandering configuration records wander=true on a completed run.
    let mut b = Benchmark::with_capacities(options(None), 2, 2, false);
    b.configure(diagnostics(), [960, 640], 1.0, 1.0, true)
        .unwrap();
    b.on_submission(0.25, Some(1), std::iter::empty()).unwrap();
    b.update(10.25).unwrap();
    b.record_scene(measured_sample(SceneFrameOutcome::Submitted))
        .unwrap();
    b.update(40.5).unwrap();
    let result = b.result();
    assert_eq!(result["valid"], true);
    assert_eq!(result["config"]["wander"], true);
    assert_eq!(result["config"]["instance_count"], 4);
    assert_eq!(result["config"]["warmup_seconds"], 10.0);
}
