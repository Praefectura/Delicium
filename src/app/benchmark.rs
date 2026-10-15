use std::{
    collections::BTreeSet,
    io::{self, Write},
    mem::size_of,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use glam::Vec3;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    model::ModelKind,
    renderer::{GraphicsDiagnostics, SceneFrameOutcome, SceneFrameTimings},
};

use super::cli::BenchOptions;

const SCENE_SAMPLE_CAPACITY: usize = 65_536;
const CONTROL_SAMPLE_CAPACITY: usize = 131_072;
const WARMUP_SECONDS: f64 = 10.0;
const BASELINE_SECONDS: f64 = 5.0;
const SETTLE_SECONDS: f64 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Setup,
    Warmup,
    Measure,
    ReloadBaseline,
    Reload,
    Settle,
    Done,
    Failed,
}

impl Phase {
    pub fn measured(self) -> bool {
        matches!(
            self,
            Self::Measure | Self::ReloadBaseline | Self::Reload | Self::Settle
        )
    }
}

#[derive(Clone, Copy, Default)]
pub(super) enum RegionDecision {
    #[default]
    NotUsed,
    Reused,
    Rebuilt,
    Failed,
}

#[derive(Clone, Copy)]
pub(super) struct SceneSample {
    pub phase: Phase,
    pub interval: Duration,
    pub advance: Option<Duration>,
    pub render_frame: Option<Duration>,
    pub hit_geometry: Option<Duration>,
    pub hit_compare: Option<Duration>,
    pub native_region_build_install: Option<Duration>,
    pub graphics: SceneFrameTimings,
    pub callback: Option<Duration>,
    pub steps: u32,
    pub dropped_steps: u64,
    pub outcome: Option<SceneFrameOutcome>,
    pub region: RegionDecision,
    pub polygons: usize,
}

impl SceneSample {
    pub fn new(phase: Phase, interval: Duration) -> Self {
        Self {
            phase,
            interval,
            advance: None,
            render_frame: None,
            hit_geometry: None,
            hit_compare: None,
            native_region_build_install: None,
            graphics: SceneFrameTimings::default(),
            callback: None,
            steps: 0,
            dropped_steps: 0,
            outcome: None,
            region: RegionDecision::NotUsed,
            polygons: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct ControlSample {
    phase: Phase,
    duration: Duration,
}

struct Samples<T> {
    values: Box<[T]>,
    used: usize,
}

impl<T: Copy> Samples<T> {
    fn new(capacity: usize, initial: T) -> Self {
        Self {
            values: vec![initial; capacity].into_boxed_slice(),
            used: 0,
        }
    }

    fn push(&mut self, value: T) -> Result<(), String> {
        let slot = self
            .values
            .get_mut(self.used)
            .ok_or("Benchmark sample capacity exceeded")?;
        *slot = value;
        self.used += 1;
        Ok(())
    }

    fn used(&self) -> &[T] {
        &self.values[..self.used]
    }
}

#[derive(Serialize)]
struct PhaseRecord {
    phase: Phase,
    start_utc_ms: u64,
    end_utc_ms: Option<u64>,
    start_elapsed_s: f64,
    end_elapsed_s: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
struct Inflight {
    request: u64,
    instance: u64,
    asset: Option<u64>,
    started: f64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ReloadConfirmation {
    pub request: u64,
    pub instance: u64,
    pub asset: u64,
    pub cycle: u32,
}

pub(super) struct Benchmark {
    pub options: BenchOptions,
    pub phase: Phase,
    started: Instant,
    utc_anchor_ms: u64,
    phase_started: f64,
    phases: Vec<PhaseRecord>,
    metadata: Option<Value>,
    window_sizes: Option<[[u32; 2]; 2]>,
    window_scales: Option<[f64; 2]>,
    failure: Option<String>,
    scene: Samples<SceneSample>,
    control: Samples<ControlSample>,
    target: Option<u64>,
    inflight: Option<Inflight>,
    last_request: Option<f64>,
    requested: u32,
    successful: u32,
    confirmed: u32,
    last_snapshot: f64,
    emit_enabled: bool,
}

impl Benchmark {
    pub fn new(options: BenchOptions) -> Self {
        Self::with_capacities(
            options,
            SCENE_SAMPLE_CAPACITY,
            CONTROL_SAMPLE_CAPACITY,
            true,
        )
    }

    fn with_capacities(
        options: BenchOptions,
        scene_capacity: usize,
        control_capacity: usize,
        emit_enabled: bool,
    ) -> Self {
        let started = Instant::now();
        let utc_anchor_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        // Warmup is nonzero: initialization touches every sample page before measurement.
        let scene = Samples::new(
            scene_capacity,
            SceneSample::new(Phase::Warmup, Duration::ZERO),
        );
        let control = Samples::new(
            control_capacity,
            ControlSample {
                phase: Phase::Warmup,
                duration: Duration::ZERO,
            },
        );
        let mut phases = Vec::with_capacity(8);
        phases.push(PhaseRecord {
            phase: Phase::Setup,
            start_utc_ms: utc_anchor_ms,
            end_utc_ms: None,
            start_elapsed_s: 0.0,
            end_elapsed_s: None,
        });
        Self {
            options,
            phase: Phase::Setup,
            started,
            utc_anchor_ms,
            phase_started: 0.0,
            phases,
            metadata: None,
            window_sizes: None,
            window_scales: None,
            failure: None,
            scene,
            control,
            target: None,
            inflight: None,
            last_request: None,
            requested: 0,
            successful: 0,
            confirmed: 0,
            last_snapshot: -1.0,
            emit_enabled,
        }
    }

    pub fn elapsed(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn utc_at(&self, now: f64) -> u64 {
        self.utc_anchor_ms + (now * 1000.0) as u64
    }

    pub fn configure(
        &mut self,
        graphics: GraphicsDiagnostics,
        requested_size: [u32; 2],
        scale: f64,
        control_scale: f64,
        wander: bool,
    ) -> Result<(), String> {
        self.window_sizes = Some([graphics.physical_size, graphics.control_physical_size]);
        self.window_scales = Some([scale, control_scale]);
        self.metadata = Some(json!({
            "requested_logical_size": requested_size,
            "logical_size": [graphics.physical_size[0] as f64 / scale, graphics.physical_size[1] as f64 / scale],
            "physical_size": graphics.physical_size,
            "control_physical_size": graphics.control_physical_size,
            "scale_factor": scale,
            "control_scale_factor": control_scale,
            "adapter": graphics.adapter, "backend": graphics.backend, "device_type": graphics.device_type,
            "present_mode": graphics.present_mode, "alpha_mode": graphics.alpha_mode,
            "format": graphics.format,
            "wander": wander,
        }));
        self.emit("phase", self.elapsed(), json!({}))
    }

    pub fn validate_windows(&self, sizes: [[u32; 2]; 2], scales: [f64; 2]) -> Result<(), String> {
        if self.window_sizes != Some(sizes) {
            return Err("Benchmark window dimensions changed after configuration".into());
        }
        if self.window_scales != Some(scales) {
            return Err("Benchmark window scale factor changed after configuration".into());
        }
        Ok(())
    }

    fn emit(&self, event: &str, now: f64, extra: Value) -> Result<(), String> {
        if !self.emit_enabled {
            return Ok(());
        }
        let mut value = json!({ "schema_version": 1, "event": event, "phase": self.phase,
            "elapsed_s": now, "utc_ms": self.utc_at(now) });
        if let (Some(target), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
            target.extend(extra.clone());
        }
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "BENCH_EVENT {value}")
            .and_then(|()| stdout.flush())
            .map_err(|e| e.to_string())
    }

    fn transition(&mut self, phase: Phase, now: f64) -> Result<(), String> {
        let utc_ms = self.utc_at(now);
        if let Some(previous) = self.phases.last_mut() {
            previous.end_elapsed_s = Some(now);
            previous.end_utc_ms = Some(utc_ms);
        }
        self.phase = phase;
        self.phase_started = now;
        self.phases.push(PhaseRecord {
            phase,
            start_utc_ms: utc_ms,
            end_utc_ms: None,
            start_elapsed_s: now,
            end_elapsed_s: None,
        });
        self.emit("phase", now, json!({}))
    }

    pub fn fail(&mut self, error: String) {
        if self.failure.is_some() {
            return;
        }
        self.failure = Some(error.clone());
        let now = self.elapsed();
        if let Err(output_error) = self
            .transition(Phase::Failed, now)
            .and_then(|()| self.emit("failure", now, json!({ "error": error })))
        {
            log::warn!("Benchmark output failed: {output_error}");
        }
    }

    pub fn stopped(&self) -> bool {
        matches!(self.phase, Phase::Done | Phase::Failed)
    }

    pub fn update(&mut self, now: f64) -> Result<(), String> {
        let elapsed = now - self.phase_started;
        match self.phase {
            Phase::Setup if elapsed > 30.0 => {
                return Err("Benchmark initial setup timed out".into());
            }
            Phase::Warmup if elapsed >= WARMUP_SECONDS => {
                self.transition(
                    if self.options.reloads.is_some() {
                        Phase::ReloadBaseline
                    } else {
                        Phase::Measure
                    },
                    now,
                )?;
            }
            Phase::Measure if elapsed >= self.options.seconds.unwrap_or_default().as_secs_f64() => {
                self.transition(Phase::Done, now)?;
            }
            Phase::ReloadBaseline if elapsed >= BASELINE_SECONDS => {
                self.transition(Phase::Reload, now)?
            }
            Phase::Reload if elapsed > 150.0 => {
                return Err("Benchmark replacement workload timed out".into());
            }
            Phase::Settle if elapsed >= SETTLE_SECONDS => self.transition(Phase::Done, now)?,
            _ => {}
        }
        if self
            .inflight
            .is_some_and(|pending| now - pending.started > 15.0)
        {
            return Err("Benchmark replacement load/render confirmation timed out".into());
        }
        Ok(())
    }

    pub fn on_submission(
        &mut self,
        now: f64,
        ready_target: Option<u64>,
        drawn: impl Iterator<Item = (u64, u64)>,
    ) -> Result<Option<ReloadConfirmation>, String> {
        if self.phase == Phase::Setup {
            if let Some(target) = ready_target {
                self.target = Some(target);
                self.transition(Phase::Warmup, now)?;
            }
            return Ok(None);
        }
        let Some(pending) = self.inflight else {
            return Ok(None);
        };
        let Some(asset) = pending.asset else {
            return Ok(None);
        };
        if !drawn
            .into_iter()
            .any(|(instance, drawn_asset)| instance == pending.instance && drawn_asset == asset)
        {
            return Ok(None);
        }
        self.inflight = None;
        self.confirmed += 1;
        let confirmation = ReloadConfirmation {
            request: pending.request,
            instance: pending.instance,
            asset,
            cycle: self.confirmed,
        };
        if Some(self.confirmed) == self.options.reloads {
            self.transition(Phase::Settle, now)?;
        }
        Ok(Some(confirmation))
    }

    pub fn reload_due(&self, now: f64) -> Option<(u64, bool)> {
        if self.phase != Phase::Reload
            || self.inflight.is_some()
            || self.requested >= self.options.reloads.unwrap_or(0)
            || self.last_request.is_some_and(|last| now - last < 0.25)
        {
            return None;
        }
        Some((self.target?, self.requested.is_multiple_of(2)))
    }

    pub fn begin_reload(&mut self, request: u64, instance: u64, now: f64) -> Result<(), String> {
        if self.reload_due(now).map(|(target, _)| target) != Some(instance) {
            return Err("Benchmark replacement request was not due".into());
        }
        self.requested += 1;
        self.last_request = Some(now);
        self.inflight = Some(Inflight {
            request,
            instance,
            asset: None,
            started: now,
        });
        Ok(())
    }

    pub fn load_succeeded(&mut self, request: u64, asset: u64) -> Result<(), String> {
        let Some(pending) = &mut self.inflight else {
            return Ok(());
        };
        if pending.request != request || pending.asset.is_some() {
            return Err("Benchmark replacement result does not match its request".into());
        }
        pending.asset = Some(asset);
        self.successful += 1;
        Ok(())
    }

    pub fn record_scene(&mut self, sample: SceneSample) -> Result<(), String> {
        if sample.phase.measured() {
            self.scene.push(sample)?;
        }
        Ok(())
    }

    pub fn record_control(&mut self, phase: Phase, duration: Duration) -> Result<(), String> {
        if phase.measured() {
            self.control.push(ControlSample { phase, duration })?;
        }
        Ok(())
    }

    pub fn snapshot_due(&mut self, now: f64) -> bool {
        if now - self.last_snapshot < 1.0 {
            return false;
        }
        self.last_snapshot = now;
        true
    }

    pub fn snapshot(&self, now: f64, data: Value) -> Result<(), String> {
        self.emit("snapshot", now, data)
    }

    pub fn reload_confirmed(
        &self,
        now: f64,
        completed: ReloadConfirmation,
        mut data: Value,
    ) -> Result<(), String> {
        let fields = data.as_object_mut().ok_or("Invalid benchmark snapshot")?;
        fields.insert("request".into(), completed.request.into());
        fields.insert("instance".into(), completed.instance.into());
        fields.insert("asset".into(), completed.asset.into());
        fields.insert("cycle".into(), completed.cycle.into());
        self.emit("reload_complete", now, data)
    }

    fn measured_seconds(&self) -> f64 {
        self.phases
            .iter()
            .filter(|phase| phase.phase.measured())
            .map(|phase| {
                phase.end_elapsed_s.unwrap_or_else(|| self.elapsed()) - phase.start_elapsed_s
            })
            .sum()
    }

    pub fn result(&self) -> Value {
        let mut stages = serde_json::Map::new();
        let samples = self.scene.used();
        type StageReader = fn(&SceneSample) -> Option<Duration>;
        let stages_to_read: [(&str, StageReader); 11] = [
            ("advance", |s| s.advance),
            ("render_frame", |s| s.render_frame),
            ("hit_geometry", |s| s.hit_geometry),
            ("hit_compare", |s| s.hit_compare),
            ("native_region_build_install", |s| {
                s.native_region_build_install
            }),
            ("acquire", |s| s.graphics.acquire),
            ("encode_upload", |s| s.graphics.encode_upload),
            ("submit", |s| s.graphics.submit),
            ("present", |s| s.graphics.present),
            ("scene_callback", |s| s.callback),
            ("frame_interval", |s| Some(s.interval)),
        ];
        for (name, get) in stages_to_read {
            stages.insert(
                name.into(),
                json!(statistics(samples.iter().filter_map(get))),
            );
        }
        stages.insert(
            "control_callback".into(),
            json!(statistics(
                self.control
                    .used()
                    .iter()
                    .filter(|s| s.phase.measured())
                    .map(|s| s.duration)
            )),
        );
        let mut submitted = 0_u64;
        let mut skipped = 0_u64;
        let mut errors = 0_u64;
        let mut steps = 0_u64;
        let mut dropped_steps = 0_u64;
        let mut rebuilt = 0_u64;
        let mut reused = 0_u64;
        let mut failed = 0_u64;
        let mut polygon_max = 0;
        let mut skip_reasons = serde_json::Map::new();
        for sample in samples {
            match sample.outcome {
                Some(SceneFrameOutcome::Submitted) => submitted += 1,
                Some(SceneFrameOutcome::Skipped(reason)) => {
                    skipped += 1;
                    let entry = skip_reasons.entry(reason.as_str()).or_insert(json!(0));
                    *entry = json!(entry.as_u64().unwrap_or(0) + 1);
                }
                None => errors += 1,
            }
            steps += u64::from(sample.steps);
            dropped_steps += sample.dropped_steps;
            polygon_max = polygon_max.max(sample.polygons);
            match sample.region {
                RegionDecision::Rebuilt => rebuilt += 1,
                RegionDecision::Reused => reused += 1,
                RegionDecision::Failed => failed += 1,
                RegionDecision::NotUsed => {}
            }
        }
        let seconds = self.measured_seconds();
        let complete = self.phase == Phase::Done && self.failure.is_none();
        let reload_complete = self.requested == self.options.reloads.unwrap_or(0)
            && self.successful == self.requested
            && self.confirmed == self.requested
            && self.inflight.is_none();
        let valid = complete
            && submitted > 0
            && errors == 0
            && failed == 0
            && reload_complete
            && self.metadata.is_some();
        let failure = self.failure.clone().or_else(|| {
            (!valid).then(|| "Benchmark completion or sample invariants failed".into())
        });
        let mut config = self.metadata.clone().unwrap_or_else(|| json!({}));
        let fields = config.as_object_mut().expect("metadata is an object");
        fields.insert("instance_count".into(), self.options.instances.into());
        fields.insert("mode".into(), self.options.mode.as_str().into());
        fields.insert(
            "seconds".into(),
            json!(self.options.seconds.map(|s| s.as_secs_f64())),
        );
        fields.insert("reloads".into(), json!(self.options.reloads));
        fields.insert(
            "build_profile".into(),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .into(),
        );
        fields.insert("warmup_seconds".into(), WARMUP_SECONDS.into());
        json!({
            "schema_version": 1, "valid": valid,
            "completion": if valid { "completed" } else { "failed" }, "failure": failure,
            "config": config, "phases": self.phases, "measured_seconds": seconds,
            "scene": { "attempts": samples.len(), "submitted": submitted, "skipped": skipped, "errors": errors,
                "submitted_per_second": if seconds > 0.0 { submitted as f64 / seconds } else { 0.0 }, "skip_reasons": skip_reasons },
            "simulation": { "steps": steps, "dropped_steps": dropped_steps },
            "regions": { "rebuilt": rebuilt, "reused": reused, "failed": failed, "polygon_max": polygon_max },
            "stages": stages,
            "reloads": { "requested": self.requested, "successful": self.successful,
                "render_confirmed": self.confirmed, "expected": self.options.reloads.unwrap_or(0) },
            "sampler": { "scene_capacity": self.scene.values.len(), "control_capacity": self.control.values.len(),
                "allocated_bytes": self.scene.values.len() * size_of::<SceneSample>() + self.control.values.len() * size_of::<ControlSample>(),
                "scene_used": self.scene.used, "control_used": self.control.used },
            "gpu_execution_ms": null,
        })
    }

    pub fn finish(&mut self) -> Result<(), String> {
        if !self.stopped() {
            self.fail("Application closed before benchmark completion".into());
        }
        let now = self.elapsed();
        let utc_ms = self.utc_at(now);
        if let Some(last) = self.phases.last_mut() {
            last.end_elapsed_s = Some(now);
            last.end_utc_ms = Some(utc_ms);
        }
        let result = self.result();
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "BENCH_RESULT {result}")
            .and_then(|()| stdout.flush())
            .map_err(|e| e.to_string())?;
        if result["valid"] == true {
            Ok(())
        } else {
            Err(result["failure"]
                .as_str()
                .unwrap_or("Invalid benchmark")
                .into())
        }
    }
}

pub(super) fn positions(count: usize, model: ModelKind) -> Vec<Vec3> {
    let (columns, rows) = match count {
        4 => (2, 2),
        16 => (4, 4),
        32 => (8, 4),
        64 => (8, 8),
        _ => return Vec::new(),
    };
    let kind = usize::from(model == ModelKind::Skeleton3d);
    let mut positions = Vec::with_capacity(count / 2);
    for row in 0..rows {
        for column in 0..columns {
            if (row + column) % 2 == kind {
                positions.push(Vec3::new(
                    -1.8 + 3.6 * column as f32 / (columns - 1) as f32,
                    -0.2 + 0.8 * row as f32 / (rows - 1) as f32,
                    0.0,
                ));
            }
        }
    }
    positions
}

pub(super) fn expected_assets(
    instances: impl Iterator<Item = u64>,
    examples: &[Option<u64>],
) -> usize {
    instances
        .chain(examples.iter().flatten().copied())
        .collect::<BTreeSet<_>>()
        .len()
}

#[derive(Debug, Serialize)]
struct Statistics {
    count: usize,
    mean_ms: Option<f64>,
    p50_ms: Option<f64>,
    p95_ms: Option<f64>,
    p99_ms: Option<f64>,
    max_ms: Option<f64>,
}

fn statistics(values: impl Iterator<Item = Duration>) -> Statistics {
    let mut values: Vec<_> = values.map(|v| v.as_secs_f64() * 1000.0).collect();
    values.sort_unstable_by(f64::total_cmp);
    let count = values.len();
    let percentile = |percent: usize| {
        count
            .checked_sub(1)
            .map(|_| values[(count * percent).div_ceil(100) - 1])
    };
    Statistics {
        count,
        mean_ms: (count > 0).then(|| values.iter().sum::<f64>() / count as f64),
        p50_ms: percentile(50),
        p95_ms: percentile(95),
        p99_ms: percentile(99),
        max_ms: values.last().copied(),
    }
}

#[cfg(test)]
#[path = "benchmark_tests.rs"]
mod tests;
