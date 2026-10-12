pub mod cli;
mod loader;
mod ui;

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use glam::Vec3;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId, WindowLevel},
};

use crate::{
    model::ModelKind,
    platform,
    renderer::{Graphics, InstanceDraw, RenderFrame, screen_to_world},
    simulation::{AssetId, InstanceId, Simulation, render_asset},
};

use self::{
    cli::{Options, ParseResult},
    loader::{CompletedLoad, LoadJob, LoadSource, Loader},
    ui::{ControlUi, InstanceSummary, UiCommand},
};

const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);
const MAX_PENDING_LOADS: usize = loader::QUEUE_CAPACITY;

pub fn run() -> Result<(), String> {
    let options = match cli::parse_args(std::env::args_os().skip(1))? {
        ParseResult::Help => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        ParseResult::Run(options) => options,
    };
    let event_loop = EventLoop::new().map_err(|error| error.to_string())?;
    let mut application = Application::new(options)?;
    event_loop
        .run_app(&mut application)
        .map_err(|error| error.to_string())?;
    if let Some(error) = application.fatal {
        return Err(error);
    }
    if application.options.smoke_test && !application.smoke.passed {
        return Err(format!(
            "Smoke test incomplete: changed_pose={}, cross_type={}, invalid_preserved={}, input_api_toggled={}",
            application.smoke.pose_changed,
            application.smoke.cross_type,
            application.smoke.invalid_preserved,
            application.smoke.input_mode_toggled
        ));
    }
    Ok(())
}

struct Runtime {
    overlay: Arc<Window>,
    control: Arc<Window>,
    graphics: Graphics,
    egui_context: egui::Context,
    egui_input: egui_winit::State,
}

enum Operation {
    Spawn { positions: Vec<Vec3> },
    Replace { instance: InstanceId },
}

#[derive(Default)]
struct SmokeState {
    stage: u8,
    initial_pose: Option<Vec<f32>>,
    initial_instances: Vec<InstanceId>,
    pose_changed: bool,
    cross_type: bool,
    invalid_probe: Option<(u64, InstanceId, AssetId)>,
    invalid_preserved: bool,
    input_mode_toggled: bool,
    passed: bool,
}

struct Application {
    options: Options,
    runtime: Option<Runtime>,
    simulation: Simulation,
    loader: Loader,
    ui: ControlUi,
    pending: HashMap<u64, Operation>,
    initial_models: VecDeque<PathBuf>,
    latest_replacement: HashMap<InstanceId, u64>,
    sources: HashMap<InstanceId, LoadSource>,
    examples: [Option<AssetId>; 2],
    next_request: u64,
    cursor: [f32; 2],
    dragging: Option<(InstanceId, Vec3)>,
    started: Instant,
    last_frame: Instant,
    next_frame: Instant,
    rendered_frames: u64,
    smoke: SmokeState,
    fatal: Option<String>,
}

impl Application {
    fn new(options: Options) -> Result<Self, String> {
        let now = Instant::now();
        Ok(Self {
            options,
            runtime: None,
            simulation: Simulation::new(),
            loader: Loader::new()?,
            ui: ControlUi::default(),
            pending: HashMap::new(),
            initial_models: VecDeque::new(),
            latest_replacement: HashMap::new(),
            sources: HashMap::new(),
            examples: [None, None],
            next_request: 1,
            cursor: [0.0; 2],
            dragging: None,
            started: now,
            last_frame: now,
            next_frame: now,
            rendered_frames: 0,
            smoke: SmokeState::default(),
            fatal: None,
        })
    }

    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let monitor = event_loop.primary_monitor();
        let origin = monitor
            .as_ref()
            .map_or(PhysicalPosition::new(0, 0), |monitor| monitor.position());
        let scale = monitor
            .as_ref()
            .map_or(1.0, |monitor| monitor.scale_factor());
        let monitor_size = monitor
            .as_ref()
            .map(|monitor| monitor.size().to_logical::<f64>(scale));
        let width = monitor_size.map_or(self.options.overlay_size[0] as f64, |size| {
            (self.options.overlay_size[0] as f64).min((size.width - 430.0).max(320.0))
        });
        let height = monitor_size.map_or(self.options.overlay_size[1] as f64, |size| {
            (self.options.overlay_size[1] as f64).min((size.height - 110.0).max(240.0))
        });
        let overlay = Arc::new(
            event_loop
                .create_window(platform::overlay_attributes(
                    Window::default_attributes()
                        .with_title("Delicium Scene")
                        .with_decorations(false)
                        .with_transparent(true)
                        .with_window_level(WindowLevel::AlwaysOnTop)
                        .with_inner_size(LogicalSize::new(width, height))
                        .with_position(PhysicalPosition::new(
                            origin.x + (30.0 * scale) as i32,
                            origin.y + (50.0 * scale) as i32,
                        )),
                ))
                .map_err(|error| error.to_string())?,
        );
        let control_height =
            monitor_size.map_or(680.0, |size| (size.height - 110.0).clamp(480.0, 680.0));
        let control = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Delicium Controls")
                        .with_window_level(WindowLevel::AlwaysOnTop)
                        .with_inner_size(LogicalSize::new(380.0, control_height))
                        .with_position(PhysicalPosition::new(
                            origin.x + ((width + 50.0) * scale) as i32,
                            origin.y + (50.0 * scale) as i32,
                        )),
                )
                .map_err(|error| error.to_string())?,
        );
        let graphics = pollster::block_on(Graphics::new(
            overlay.clone(),
            control.clone(),
            self.options.backend,
        ))
        .map_err(|error| error.to_string())?;
        self.ui.adapter = graphics.adapter_description().to_owned();
        log::info!(
            "adapter={} transparency={:?}",
            self.ui.adapter,
            graphics.transparency_mode()
        );
        let egui_context = egui::Context::default();
        let egui_input = egui_winit::State::new(
            egui_context.clone(),
            egui::ViewportId::ROOT,
            control.as_ref(),
            Some(control.scale_factor() as f32),
            None,
            None,
        );
        platform::set_click_through(&overlay, &control, true)?;
        self.runtime = Some(Runtime {
            overlay,
            control,
            graphics,
            egui_context,
            egui_input,
        });
        self.started = Instant::now();
        self.last_frame = self.started;
        self.next_frame = self.started;
        if self.options.models.is_empty() {
            self.submit_load(
                LoadSource::Example2d,
                Operation::Spawn {
                    positions: vec![Vec3::new(-2.4, 0.15, 0.0), Vec3::new(-0.8, 0.15, 0.0)],
                },
            )?;
            self.submit_load(
                LoadSource::Example3d,
                Operation::Spawn {
                    positions: vec![Vec3::new(0.8, 0.15, 0.0), Vec3::new(2.4, 0.15, 0.0)],
                },
            )?;
        } else {
            self.initial_models
                .extend(self.options.models.iter().cloned());
        }
        Ok(())
    }

    fn submit_load(&mut self, source: LoadSource, operation: Operation) -> Result<u64, String> {
        if self.pending.len() >= MAX_PENDING_LOADS {
            return Err("Too many pending model loads; wait for the current requests".into());
        }
        if let Operation::Replace { instance } = &operation
            && self.simulation.instance(*instance).is_none()
        {
            return Err("The selected character no longer exists".into());
        }
        let request = self.next_request;
        self.next_request = request
            .checked_add(1)
            .ok_or("Model request counter exhausted")?;
        self.loader.submit(LoadJob {
            id: request,
            source,
        })?;
        if let Operation::Replace { instance } = &operation {
            self.latest_replacement.insert(*instance, request);
        }
        self.pending.insert(request, operation);
        self.ui.pending = self.pending.len();
        Ok(request)
    }

    fn poll_loads(&mut self) -> Result<(), String> {
        while let Some(completed) = self.loader.poll()? {
            self.finish_load(completed);
        }
        while self.pending.len() < MAX_PENDING_LOADS {
            let Some(path) = self.initial_models.pop_front() else {
                break;
            };
            let position = self.next_spawn_position();
            if let Err(error) = self.submit_load(
                LoadSource::File(path),
                Operation::Spawn {
                    positions: vec![position],
                },
            ) {
                self.report_error(error);
            }
        }
        self.ui.pending = self.pending.len() + self.initial_models.len();
        Ok(())
    }

    fn finish_load(&mut self, completed: CompletedLoad) {
        let Some(operation) = self.pending.remove(&completed.id) else {
            return;
        };
        if let Operation::Replace { instance } = &operation {
            if self.latest_replacement.get(instance) != Some(&completed.id) {
                return;
            }
            self.latest_replacement.remove(instance);
            if self.simulation.instance(*instance).is_none() {
                return;
            }
        }
        let model = match completed.result {
            Ok(model) => model,
            Err(error) => {
                if let Some((request, instance, asset)) = self.smoke.invalid_probe
                    && request == completed.id
                {
                    self.smoke.invalid_preserved = self
                        .simulation
                        .instance(instance)
                        .is_some_and(|info| info.asset == asset);
                    log::info!(
                        "smoke invalid_replacement_preserved={}",
                        self.smoke.invalid_preserved
                    );
                }
                self.report_error(format!("{}: {error}", completed.source.label()));
                return;
            }
        };
        let asset = match self.simulation.insert_asset(model.clone()) {
            Ok(asset) => asset,
            Err(error) => {
                self.report_error(error.to_string());
                return;
            }
        };
        let Some(runtime) = self.runtime.as_mut() else {
            self.simulation.discard_asset(asset);
            return;
        };
        if let Err(error) = runtime
            .graphics
            .register_asset(asset, &render_asset(&model))
        {
            self.simulation.discard_asset(asset);
            self.report_error(error.to_string());
            return;
        }
        let result = match operation {
            Operation::Spawn { positions } => {
                let mut result = Ok(());
                for position in positions {
                    match self.simulation.spawn(asset, position) {
                        Ok(instance) => {
                            self.sources.insert(instance, completed.source.clone());
                            if self.ui.selected.is_none() {
                                self.ui.selected = Some(instance);
                            }
                            log::info!(
                                "spawn instance={instance} asset={asset} kind={}",
                                model.kind()
                            );
                        }
                        Err(error) => {
                            result = Err(error.to_string());
                            break;
                        }
                    }
                }
                match completed.source {
                    LoadSource::Example2d if self.examples[0].is_none() => {
                        self.examples[0] = Some(asset)
                    }
                    LoadSource::Example3d if self.examples[1].is_none() => {
                        self.examples[1] = Some(asset)
                    }
                    _ => {}
                }
                result
            }
            Operation::Replace { instance } => self
                .simulation
                .replace_model(instance, asset)
                .map(|()| {
                    self.sources.insert(instance, completed.source.clone());
                    match &completed.source {
                        LoadSource::Example2d => self.examples[0] = Some(asset),
                        LoadSource::Example3d => self.examples[1] = Some(asset),
                        _ => {}
                    }
                    log::info!(
                        "replace instance={instance} asset={asset} kind={}",
                        model.kind()
                    );
                })
                .map_err(|error| error.to_string()),
        };
        match result {
            Ok(()) => self.ui.status = format!("Loaded {}", model.name),
            Err(error) => self.report_error(error),
        }
        self.collect_assets();
    }

    fn collect_assets(&mut self) {
        let keep: Vec<_> = self.examples.iter().flatten().copied().collect();
        let removed = self.simulation.collect_unused_assets(&keep);
        if let Some(runtime) = &mut self.runtime {
            for asset in removed {
                runtime.graphics.remove_asset(asset);
            }
        }
    }

    fn next_spawn_position(&self) -> Vec3 {
        let index = self.simulation.instances().len() + self.pending.len();
        Vec3::new(
            (index % 5) as f32 * 1.1 - 2.2,
            (index / 5) as f32 * 0.35 + 0.15,
            0.0,
        )
    }

    fn apply_command(
        &mut self,
        command: UiCommand,
        event_loop: &ActiveEventLoop,
    ) -> Result<(), String> {
        match command {
            UiCommand::SpawnExample(kind) => {
                let index = match kind {
                    ModelKind::Sprite2d => 0,
                    ModelKind::Skeleton3d => 1,
                };
                let source = if index == 0 {
                    LoadSource::Example2d
                } else {
                    LoadSource::Example3d
                };
                let position = self.next_spawn_position();
                if let Some(asset) = self.examples[index] {
                    let instance = self
                        .simulation
                        .spawn(asset, position)
                        .map_err(|error| error.to_string())?;
                    self.sources.insert(instance, source);
                    self.ui.selected = Some(instance);
                    self.ui.status = format!("Added {kind}");
                } else {
                    self.submit_load(
                        source,
                        Operation::Spawn {
                            positions: vec![position],
                        },
                    )?;
                }
            }
            UiCommand::Remove(instance) => {
                self.simulation.despawn(instance);
                self.sources.remove(&instance);
                self.latest_replacement.remove(&instance);
                if self.dragging.is_some_and(|(target, _)| target == instance) {
                    self.dragging = None;
                }
                if self.ui.selected == Some(instance) {
                    self.ui.selected = None;
                }
                self.collect_assets();
            }
            UiCommand::Load { source, replace } => {
                let operation = replace.map_or_else(
                    || Operation::Spawn {
                        positions: vec![self.next_spawn_position()],
                    },
                    |instance| Operation::Replace { instance },
                );
                self.submit_load(LoadSource::File(source), operation)?;
            }
            UiCommand::Reload(instance) => {
                let source = self
                    .sources
                    .get(&instance)
                    .cloned()
                    .ok_or("No source for this character")?;
                self.submit_load(source, Operation::Replace { instance })?;
            }
            UiCommand::Move(instance, position) => {
                self.simulation
                    .set_position(instance, position)
                    .map_err(|error| error.to_string())?;
            }
            UiCommand::ClickThrough(enabled) => {
                let runtime = self.runtime.as_ref().ok_or("Windows are not ready")?;
                if let Err(error) =
                    platform::set_click_through(&runtime.overlay, &runtime.control, enabled)
                {
                    self.ui.click_through = !enabled;
                    return Err(error);
                }
                self.ui.click_through = enabled;
                self.dragging = None;
                log::info!("click_through={enabled}");
            }
            UiCommand::Quit => event_loop.exit(),
        }
        Ok(())
    }

    fn redraw_scene(&mut self) -> Result<(), String> {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_frame);
        self.last_frame = now;
        self.ui.frame_ms = self.ui.frame_ms * 0.9 + elapsed.as_secs_f32() * 1000.0 * 0.1;
        self.simulation
            .advance(elapsed)
            .map_err(|error| error.to_string())?;
        self.ui.tick_count = self.simulation.tick_count();
        self.ui.interactions = self.simulation.interactions().len();
        let frame = self
            .simulation
            .render_frame()
            .map_err(|error| error.to_string())?;
        if self.options.smoke_test {
            self.observe_smoke_frame(&frame);
        }
        if let Some(runtime) = &mut self.runtime {
            runtime
                .graphics
                .render_scene(&frame)
                .map_err(|error| error.to_string())?;
        }
        self.rendered_frames += 1;
        Ok(())
    }

    fn redraw_control(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let instances: Vec<_> = self
            .simulation
            .instances()
            .into_iter()
            .map(|info| InstanceSummary {
                id: info.id,
                name: info.name,
                kind: info.kind,
                position: info.position,
                neighbor: info.neighbor,
            })
            .collect();
        let mut commands = Vec::new();
        if let Some(runtime) = &mut self.runtime {
            let input = runtime.egui_input.take_egui_input(&runtime.control);
            let mut output = runtime.egui_context.run_ui(input, |ui| {
                commands = self.ui.draw(ui, &instances);
            });
            runtime
                .egui_input
                .handle_platform_output(&runtime.control, output.platform_output);
            let jobs = runtime
                .egui_context
                .tessellate(output.shapes, output.pixels_per_point);
            let render_result = runtime.graphics.render_control(
                &jobs,
                &output.textures_delta,
                output.pixels_per_point,
            );
            // Acknowledge handled deltas, or discard them on a fatal render error.
            output.textures_delta.clear();
            render_result.map_err(|error| error.to_string())?;
        }
        for command in commands {
            if let Err(error) = self.apply_command(command, event_loop) {
                self.report_error(error);
            }
        }
        Ok(())
    }

    fn observe_smoke_frame(&mut self, frame: &RenderFrame) {
        if self.simulation.instances().len() < 4 {
            return;
        }
        let signature = pose_sample(frame);
        let instances: Vec<_> = frame.instances.iter().map(InstanceDraw::instance).collect();
        let same_instances = instances == self.smoke.initial_instances;
        if let Some(initial) = &self.smoke.initial_pose
            && same_instances
            && initial.len() == signature.len()
        {
            if initial
                .iter()
                .zip(&signature)
                .any(|(before, after)| (before - after).abs() > 0.0001)
            {
                self.smoke.pose_changed = true;
            }
        } else {
            self.smoke.initial_instances = instances;
            self.smoke.initial_pose = Some(signature);
        }
        for edge in self.simulation.interactions() {
            if let (Some(from), Some(to)) = (
                self.simulation.instance(edge.from),
                self.simulation.instance(edge.to),
            ) && from.kind != to.kind
            {
                self.smoke.cross_type = true;
            }
        }
    }

    fn advance_smoke(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        if !self.options.smoke_test || self.simulation.instances().len() < 4 {
            return Ok(());
        }
        let seconds = self.started.elapsed().as_secs_f32();
        if self.smoke.stage == 0 && seconds > 2.0 {
            let was_enabled = self.ui.click_through;
            self.apply_command(UiCommand::ClickThrough(false), event_loop)?;
            self.smoke.input_mode_toggled = was_enabled && !self.ui.click_through;
            self.smoke.stage = 1;
        }
        if self.smoke.stage == 1 && seconds > 3.0 {
            let instances = self.simulation.instances();
            let first_2d = instances
                .iter()
                .find(|info| info.kind == ModelKind::Sprite2d)
                .map(|info| info.id);
            let first_3d = instances
                .iter()
                .find(|info| info.kind == ModelKind::Skeleton3d)
                .map(|info| info.id);
            let mut other_x = 5.0;
            for info in &instances {
                let x = if Some(info.id) == first_2d {
                    -0.75
                } else if Some(info.id) == first_3d {
                    0.75
                } else {
                    let x = other_x;
                    other_x += 3.0;
                    x
                };
                self.simulation
                    .set_position(info.id, Vec3::new(x, 0.15, 0.0))
                    .map_err(|error| error.to_string())?;
            }
            self.smoke.stage = 2;
        }
        if self.smoke.stage == 2 && seconds > 4.0 {
            let info = self
                .simulation
                .instances()
                .into_iter()
                .find(|info| info.kind == ModelKind::Sprite2d)
                .ok_or("Smoke test has no 2D character")?;
            let request = self.submit_load(
                LoadSource::ValidationProbe,
                Operation::Replace { instance: info.id },
            )?;
            self.smoke.invalid_probe = Some((request, info.id, info.asset));
            self.smoke.stage = 3;
        }
        if self.smoke.stage == 3 && seconds > 6.0 {
            let was_disabled = !self.ui.click_through;
            self.apply_command(UiCommand::ClickThrough(true), event_loop)?;
            self.smoke.input_mode_toggled &= was_disabled && self.ui.click_through;
            self.smoke.stage = 4;
        }
        if self.smoke.stage >= 4 && seconds > 9.0 {
            let passed = self.smoke.pose_changed
                && self.smoke.cross_type
                && self.smoke.invalid_preserved
                && self.smoke.input_mode_toggled
                && self.rendered_frames > 30;
            if self.smoke.stage == 4 || passed != self.smoke.passed {
                log::info!(
                    "SMOKE_RESULT pass={} frames={} ticks={} pose_changed={} cross_type={} invalid_preserved={} input_api_toggled={}",
                    passed,
                    self.rendered_frames,
                    self.simulation.tick_count(),
                    self.smoke.pose_changed,
                    self.smoke.cross_type,
                    self.smoke.invalid_preserved,
                    self.smoke.input_mode_toggled
                );
            }
            self.smoke.passed = passed;
            self.smoke.stage = 5;
        }
        Ok(())
    }

    fn pick_character(&self) -> Option<InstanceId> {
        let runtime = self.runtime.as_ref()?;
        let size = runtime.overlay.inner_size();
        let point = screen_to_world(self.cursor, [size.width, size.height]);
        let mut frame = self.simulation.render_frame().ok()?;
        frame.instances.sort_by_key(|draw| {
            (
                matches!(draw, InstanceDraw::Sprite2d { .. }),
                draw.instance(),
            )
        });
        for draw in frame.instances.iter().rev() {
            let mut bounds = [
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            ];
            let mut include = |position: Vec3| {
                if position.is_finite() {
                    bounds[0] = bounds[0].min(position.x);
                    bounds[1] = bounds[1].min(position.y);
                    bounds[2] = bounds[2].max(position.x);
                    bounds[3] = bounds[3].max(position.y);
                }
            };
            match draw {
                InstanceDraw::Sprite2d {
                    transform, layers, ..
                } => {
                    for vertex in layers.iter().flatten() {
                        include(transform.transform_point3(Vec3::new(vertex[0], vertex[1], 0.0)));
                    }
                }
                InstanceDraw::Skeleton3d {
                    asset,
                    transform,
                    skin_matrices,
                    ..
                } => {
                    let model = self.simulation.asset(*asset)?;
                    let crate::model::ModelData::Skeleton3d(skeleton) = &model.data else {
                        continue;
                    };
                    for vertex in &skeleton.vertices {
                        let bind = Vec3::from_array(vertex.position);
                        let mut position = Vec3::ZERO;
                        for (joint, weight) in vertex.joints.iter().zip(vertex.weights) {
                            position +=
                                skin_matrices.get(*joint as usize)?.transform_point3(bind) * weight;
                        }
                        include(transform.transform_point3(position));
                    }
                }
            }
            if point[0] >= bounds[0] - 0.1
                && point[0] <= bounds[2] + 0.1
                && point[1] >= bounds[1] - 0.1
                && point[1] <= bounds[3] + 0.1
            {
                return Some(draw.instance());
            }
        }
        None
    }

    fn report_error(&mut self, error: String) {
        log::warn!("{error}");
        self.ui.last_error = Some(error);
    }

    fn fail(&mut self, error: String, event_loop: &ActiveEventLoop) {
        log::error!("{error}");
        self.fatal = Some(error);
        event_loop.exit();
    }
}

impl ApplicationHandler for Application {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.runtime.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            self.fail(error, event_loop);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        let is_control = window_id == runtime.control.id();
        let is_overlay = window_id == runtime.overlay.id();
        if !is_control && !is_overlay {
            return;
        }
        if is_control {
            let response = runtime.egui_input.on_window_event(&runtime.control, &event);
            if response.repaint {
                runtime.control.request_redraw();
            }
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => runtime.graphics.resize(window_id, size),
            WindowEvent::ScaleFactorChanged { .. } => {
                let window = if is_control {
                    &runtime.control
                } else {
                    &runtime.overlay
                };
                runtime.graphics.resize(window_id, window.inner_size());
                window.request_redraw();
            }
            WindowEvent::Focused(true) if is_overlay => platform::raise_controls(&runtime.control),
            WindowEvent::Focused(false) if is_overlay => self.dragging = None,
            WindowEvent::DroppedFile(path) => {
                let position = self.next_spawn_position();
                if let Err(error) = self.submit_load(
                    LoadSource::File(path),
                    Operation::Spawn {
                        positions: vec![position],
                    },
                ) {
                    self.report_error(error);
                }
            }
            WindowEvent::RedrawRequested => {
                let result = if is_control {
                    self.redraw_control(event_loop)
                } else {
                    self.redraw_scene()
                };
                if let Err(error) = result {
                    self.fail(error, event_loop);
                }
            }
            WindowEvent::CursorMoved { position, .. } if is_overlay => {
                self.cursor = [position.x as f32, position.y as f32];
                if let Some((instance, offset)) = self.dragging {
                    let size = self
                        .runtime
                        .as_ref()
                        .map(|runtime| runtime.overlay.inner_size());
                    if let Some(size) = size {
                        let point = screen_to_world(self.cursor, [size.width, size.height]);
                        let position = Vec3::new(point[0], point[1], 0.0) + offset;
                        if let Err(error) = self.simulation.set_position(instance, position) {
                            self.report_error(error.to_string());
                        }
                    }
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } if is_overlay && !self.ui.click_through => {
                if state == ElementState::Released {
                    self.dragging = None;
                } else if let Some(instance) = self.pick_character()
                    && let Some(info) = self.simulation.instance(instance)
                    && let Some(runtime) = &self.runtime
                {
                    let size = runtime.overlay.inner_size();
                    let point = screen_to_world(self.cursor, [size.width, size.height]);
                    self.dragging =
                        Some((instance, info.position - Vec3::new(point[0], point[1], 0.0)));
                    self.ui.selected = Some(instance);
                }
            }
            WindowEvent::CursorLeft { .. } if is_overlay => self.dragging = None,
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.runtime.is_none() {
            return;
        }
        if let Some(exit_after) = self.options.exit_after
            && self.started.elapsed() >= exit_after
        {
            log::info!(
                "exit frames={} ticks={} assets={}",
                self.rendered_frames,
                self.simulation.tick_count(),
                self.simulation.asset_count()
            );
            event_loop.exit();
            return;
        }
        if let Err(error) = self
            .poll_loads()
            .and_then(|()| self.advance_smoke(event_loop))
        {
            self.fail(error, event_loop);
            return;
        }
        let now = Instant::now();
        if now >= self.next_frame {
            self.next_frame = now + FRAME_INTERVAL;
            // Drive frames from the tick itself. On Windows `request_redraw`
            // only produces `RedrawRequested` through WM_PAINT synthesis,
            // which this overlay window does not receive; the tick keeps the
            // simulation and both windows on schedule regardless.
            if let Err(error) = self.redraw_scene() {
                self.fail(error, event_loop);
                return;
            }
            if let Err(error) = self.redraw_control(event_loop) {
                self.fail(error, event_loop);
                return;
            }
        }
        let wake = self.options.exit_after.map_or(self.next_frame, |duration| {
            self.next_frame.min(self.started + duration)
        });
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

fn pose_sample(frame: &RenderFrame) -> Vec<f32> {
    let mut values = Vec::new();
    for instance in &frame.instances {
        match instance {
            InstanceDraw::Sprite2d { layers, .. } => {
                for layer in layers {
                    values.extend(layer.iter().take(16).flatten().copied());
                }
            }
            InstanceDraw::Skeleton3d { skin_matrices, .. } => {
                for matrix in skin_matrices {
                    values.extend(matrix.to_cols_array());
                }
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LoadLimits, load_model_bytes};

    fn application_with_character() -> (Application, InstanceId, AssetId) {
        let mut app = Application::new(Options::default()).unwrap();
        let model = load_model_bytes(
            include_bytes!("../../assets/examples/puppet2d.dmodel.json"),
            &LoadLimits::default(),
        )
        .unwrap();
        let asset = app.simulation.insert_asset(model).unwrap();
        let instance = app.simulation.spawn(asset, Vec3::ZERO).unwrap();
        (app, instance, asset)
    }

    #[test]
    fn failed_background_replacement_keeps_live_model() {
        let (mut app, instance, asset) = application_with_character();
        app.simulation.advance(Duration::from_millis(50)).unwrap();
        let time = app.simulation.instance(instance).unwrap().animation_time;
        app.pending.insert(1, Operation::Replace { instance });
        app.latest_replacement.insert(instance, 1);
        app.finish_load(CompletedLoad {
            id: 1,
            source: LoadSource::ValidationProbe,
            result: load_model_bytes(b"invalid", &LoadLimits::default())
                .map_err(|error| error.to_string()),
        });
        assert_eq!(app.simulation.instance(instance).unwrap().asset, asset);
        assert_eq!(
            app.simulation.instance(instance).unwrap().animation_time,
            time
        );
        assert!(app.ui.last_error.is_some());
        assert!(app.pending.is_empty());
        assert!(app.latest_replacement.is_empty());
    }

    #[test]
    fn stale_or_removed_target_loads_never_install_assets() {
        let (mut app, instance, asset) = application_with_character();
        let model = app.simulation.asset(asset).unwrap().clone();
        app.pending.insert(1, Operation::Replace { instance });
        app.latest_replacement.insert(instance, 2);
        app.finish_load(CompletedLoad {
            id: 1,
            source: LoadSource::Example2d,
            result: Ok(model.clone()),
        });
        assert_eq!(app.simulation.asset_count(), 1);
        assert_eq!(app.latest_replacement.get(&instance), Some(&2));
        assert_eq!(app.simulation.instance(instance).unwrap().asset, asset);
        app.pending.insert(2, Operation::Replace { instance });
        app.simulation.despawn(instance);
        app.finish_load(CompletedLoad {
            id: 2,
            source: LoadSource::Example2d,
            result: Ok(model),
        });
        assert_eq!(app.simulation.asset_count(), 1);
        assert!(app.latest_replacement.is_empty());
        assert!(app.ui.last_error.is_none());
    }

    #[test]
    fn smoke_resets_baseline_when_the_rendered_instance_set_changes() {
        let (mut app, _, asset) = application_with_character();
        for index in 1..4 {
            app.simulation
                .spawn(asset, Vec3::new(index as f32, 0.0, 0.0))
                .unwrap();
        }
        let mut frame = app.simulation.render_frame().unwrap();
        app.observe_smoke_frame(&frame);
        assert!(!app.smoke.pose_changed);
        let InstanceDraw::Sprite2d {
            instance, layers, ..
        } = &mut frame.instances[0]
        else {
            panic!("expected 2D instance");
        };
        *instance += 100;
        layers[0][0][0] += 0.5;
        app.observe_smoke_frame(&frame);
        assert!(
            !app.smoke.pose_changed,
            "different instances are not animation evidence"
        );
        let InstanceDraw::Sprite2d { layers, .. } = &mut frame.instances[0] else {
            unreachable!()
        };
        layers[0][0][0] += 0.5;
        app.observe_smoke_frame(&frame);
        assert!(app.smoke.pose_changed);
    }
}
