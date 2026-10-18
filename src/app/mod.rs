mod attention;
mod benchmark;
pub mod cli;
mod config;
mod hit_region;
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
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId, WindowLevel},
};

use crate::{
    model::ModelKind,
    platform,
    renderer::{
        Graphics, InstanceDraw, LEGACY, RenderFrame, SceneFrameOutcome, SceneView, desktop_view_at,
        screen_to_world_with,
    },
    simulation::{
        AssetId, InstanceId, Simulation, WANDER_EDGE_MARGIN, WanderEvent, WorldRect, WorldXBounds,
        render_asset,
    },
};

use self::{
    benchmark::{Benchmark, Phase, RegionDecision, SceneSample},
    cli::{BenchMode, Options, ParseResult},
    hit_region::{HitGeometry, RegionChange},
    loader::{CompletedLoad, LoadJob, LoadSource, Loader},
    ui::{ControlUi, InstanceSummary, UiCommand},
};

const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);
const MAX_PENDING_LOADS: usize = loader::QUEUE_CAPACITY;

/// Largest pointer travel, in physical pixels, for a press and release on the
/// same character to count as a click (a poke) instead of a drag. Strictly
/// below this distance the gesture is a click.
const CLICK_TOLERANCE_PX: f32 = 4.0;

/// Whether the current endpoint is inside the click tolerance. The gesture's
/// latched `drag_moved` state also excludes any earlier excursion.
fn is_click_press(press: [f32; 2], release: [f32; 2]) -> bool {
    let dx = release[0] - press[0];
    let dy = release[1] - press[1];
    dx * dx + dy * dy < CLICK_TOLERANCE_PX * CLICK_TOLERANCE_PX
}

pub fn run() -> Result<(), String> {
    let options = match cli::parse_args(std::env::args_os().skip(1))? {
        ParseResult::Help => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        ParseResult::Run(options) => options,
    };
    let config_enabled = config::enabled(&options);
    let loaded = config_enabled.then(config::load).flatten();
    let event_loop = EventLoop::new().map_err(|error| error.to_string())?;
    let mut application = Application::new(options)?;
    application.pending_pixel_hittest = loaded.as_ref().map(|config| config.pixel_hittest);
    application.config = loaded;
    // A live pet follows the settings page; a bounded run must stay
    // reproducible, and the scenes that ignore the file ignore it here too.
    if config::preferences_enabled(&application.options)
        && application.config.is_none()
        // The pet has no saved scene, but it still honours the preference:
        // only `false` needs a command, since `true` is the default.
        && application.pending_pixel_hittest.is_none()
    {
        application.pending_pixel_hittest = config::read_preferences().map(|p| p.pixel_hittest);
    }
    if config::preferences_enabled(&application.options)
        && let Some((modified, length)) = config::stamp()
    {
        // The pet owns its scene, so its preference baseline comes from the
        // file rather than from `application.config`; before the first
        // write there is no file and nothing to follow.
        application.config_watch = config::read_preferences().map(|preferences| ConfigWatch {
            modified,
            length,
            preferences,
        });
    }
    if let Err(error) = event_loop.run_app(&mut application) {
        let error = error.to_string();
        if let Some(benchmark) = &mut application.benchmark {
            benchmark.fail(error.clone());
        }
        application.fatal = Some(error);
    }
    if let Some(benchmark) = &mut application.benchmark {
        benchmark.finish()?;
    }
    if let Some(error) = application.fatal {
        return Err(error);
    }
    // The smoke test and benchmarks never reach this point with persistence
    // enabled; a clean exit captures the scene and the hit-test preference.
    if config_enabled && let Err(error) = config::save(&application.capture_config()) {
        log::warn!("Cannot save the configuration: {error}");
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

/// The live configuration watch: where the file stood when its preferences
/// were last applied.
struct ConfigWatch {
    modified: std::time::SystemTime,
    length: u64,
    preferences: config::Preferences,
}

struct Runtime {
    overlay: Arc<Window>,
    control: Arc<Window>,
    graphics: Graphics,
    egui_context: egui::Context,
    egui_input: egui_winit::State,
    /// Shared by rendering, picking, dragging and native hit regions.
    scene_view: SceneView,
    /// The startup layout; bounded launches do not use desktop constraints.
    desktop: Option<DesktopLayout>,
}

struct DesktopLayout {
    bounds: platform::DesktopRect,
    primary: platform::DesktopRect,
    monitors: Vec<platform::DesktopRect>,
    scale: f32,
}

impl DesktopLayout {
    fn new(
        primary: platform::DesktopRect,
        monitors: Vec<platform::DesktopRect>,
        scale: f32,
    ) -> Result<Self, String> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err("Invalid primary monitor scale".into());
        }
        let bounds = platform::desktop_bounding_rect(&monitors)?;
        Ok(Self {
            bounds,
            primary,
            monitors,
            scale,
        })
    }

    fn view(&self) -> SceneView {
        self.view_at(self.bounds)
    }

    fn view_at(&self, rect: platform::DesktopRect) -> SceneView {
        let center_offset = (i64::from(rect.left) + i64::from(rect.right)
            - i64::from(self.primary.left)
            - i64::from(self.primary.right)) as f32
            * 0.5;
        let below_primary = (i64::from(rect.bottom) - i64::from(self.primary.bottom)) as f32;
        desktop_view_at(rect.size().height, self.scale, center_offset, below_primary)
    }

    fn window_view(&self, window: &Window) -> Result<SceneView, String> {
        let position = window.outer_position().map_err(|error| error.to_string())?;
        let rect = platform::DesktopRect::from_physical(position, window.inner_size())?;
        Ok(self.view_at(rect))
    }

    fn regions(&self) -> Vec<WorldRect> {
        let view = self.view();
        let size = self.bounds.size();
        self.monitors
            .iter()
            .map(|rect| {
                let top_left = screen_to_world_with(
                    [
                        (i64::from(rect.left) - i64::from(self.bounds.left)) as f32,
                        (i64::from(rect.top) - i64::from(self.bounds.top)) as f32,
                    ],
                    [size.width, size.height],
                    view,
                );
                let bottom_right = screen_to_world_with(
                    [
                        (i64::from(rect.right) - i64::from(self.bounds.left)) as f32,
                        (i64::from(rect.bottom) - i64::from(self.bounds.top)) as f32,
                    ],
                    [size.width, size.height],
                    view,
                );
                WorldRect {
                    left: top_left[0],
                    right: bottom_right[0],
                    bottom: bottom_right[1],
                    top: top_left[1],
                }
            })
            .collect()
    }

    /// Compare first: DPI/resize callbacks must not generate a resize loop.
    fn restore_window(&self, window: &Window) {
        if window.outer_position().ok() != Some(self.bounds.position()) {
            window.set_outer_position(self.bounds.position());
        }
        if window.inner_size() != self.bounds.size() {
            let _ = window.request_inner_size(self.bounds.size());
        }
    }
}

enum Operation {
    Spawn { positions: Vec<Vec3> },
    Replace { instance: InstanceId },
}

/// How the scene window intercepts input.
///
/// `Interactive` is whole-window input, `Pixel` is the character outline
/// region, `FullWindow` is the whole window with no native region (used while
/// a drag must survive the cursor leaving a character), and `Fallback` means
/// region building failed and the whole window is click-through until the
/// user switches modes again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegionMode {
    Interactive,
    Pixel,
    /// Whole-window input through a cleared region; on Windows this keeps the
    /// window's own shape instead of growing a synthetic full-client region.
    FullWindow,
    Fallback,
}

/// Horizontal world bounds of a scene view at the given physical size,
/// matching [`screen_to_world_with`]: `x = center_x ± span / 2 * aspect`.
fn wander_bounds(view: SceneView, size: PhysicalSize<u32>) -> WorldXBounds {
    let aspect = size.width.max(1) as f32 / size.height.max(1) as f32;
    let half = view.span * 0.5 * aspect;
    WorldXBounds {
        left: view.center_x - half,
        right: view.center_x + half,
    }
}

/// Pushes the current scene view into the simulation and logs the result.
///
/// `left`/`right` are the raw visible range; the walkable inset
/// ([`WANDER_EDGE_MARGIN`]) is applied inside the simulation and reported
/// here so scripts can assert against the effective range without copying
/// the policy. Failures only warn: an invalid bounds value would be a bug,
/// but it must not kill a running frame.
fn sync_wander_bounds(
    simulation: &mut Simulation,
    wander_enabled: bool,
    view: SceneView,
    size: PhysicalSize<u32>,
) {
    let bounds = wander_enabled.then(|| wander_bounds(view, size));
    match simulation.set_wander_bounds(bounds) {
        Ok(()) => match bounds {
            Some(bounds) => log::info!(
                "wander bounds left={:.4} right={:.4} margin={:.3}",
                bounds.left,
                bounds.right,
                WANDER_EDGE_MARGIN
            ),
            None => log::info!("wander bounds none"),
        },
        Err(error) => log::warn!("wander bounds unchanged: {error}"),
    }
}

/// The last successfully installed region and the geometry it came from.
struct HitRegionCache {
    mode: RegionMode,
    geometry: Option<HitGeometry>,
    /// Whether the overlay is currently whole-window click-through
    /// (`WS_EX_TRANSPARENT` on, i.e. `set_click_through(enabled = true)`).
    /// Tracked so mode transitions flip the style only when it differs and in
    /// an order that never widens input interception on the way.
    click_through: bool,
    failed: bool,
    /// Consecutive frames whose fresh geometry differed from the installed
    /// baseline without being installed (the install-throttle counter).
    dirty_frames: u32,
    /// The next changed frame installs regardless of the cadence. Set by the
    /// drag restore so a released character never runs on an empty region.
    force_install: bool,
}

/// Frames between installs under sustained change, by current polygon count.
///
/// The installed region may lag the silhouette by up to `cadence - 1` dirty
/// frames only while movement stays within the safe padding budget; larger
/// movement and structural changes install immediately. `region_cadence` is
/// 1 below 2048 polygons, so the desktop
/// (four instances, ~650 polygons) and every GUI acceptance script keep the
/// historical install-every-changed-frame behavior. The thresholds sit well
/// clear of the measured polygon counts (648 / 2616 / 5232 / 10512), so a
/// resize that shifts the geometry slightly cannot flap between tiers.
fn region_cadence(polygons: usize) -> u32 {
    if polygons < 2048 {
        1
    } else if polygons < 8192 {
        2
    } else {
        4
    }
}

impl HitRegionCache {
    fn decision(&mut self, current: &HitGeometry) -> RegionDecision {
        let Some(previous) = &self.geometry else {
            return RegionDecision::Rebuilt;
        };
        if self.force_install {
            return RegionDecision::Rebuilt;
        }
        match current.change_since(previous) {
            RegionChange::Immediate => RegionDecision::Rebuilt,
            RegionChange::Unchanged => {
                self.note_unchanged();
                RegionDecision::Reused
            }
            RegionChange::WithinMargin => {
                // Corresponding raw topology is unchanged. Reuse the actual
                // installed count without materializing a deferred candidate.
                let polygons = previous.polygons().len();
                if self.note_changed(polygons) {
                    RegionDecision::Rebuilt
                } else {
                    RegionDecision::Deferred
                }
            }
        }
    }

    /// The fresh geometry matches the installed baseline: nothing to deliver,
    /// so the throttle no longer owes an install.
    fn note_unchanged(&mut self) {
        self.dirty_frames = 0;
    }

    /// The fresh geometry differs from the installed baseline. Returns
    /// `true` when this frame should install, `false` when the cadence
    /// defers the delivery. A `true` does not reset the counter; only
    /// [`Self::note_installed`] does, so a failed install keeps owing.
    fn note_changed(&mut self, polygons: usize) -> bool {
        let cadence = if self.force_install {
            1
        } else {
            region_cadence(polygons)
        };
        if self.dirty_frames + 1 >= cadence {
            true
        } else {
            self.dirty_frames += 1;
            false
        }
    }

    /// An install succeeded: the delivered geometry is the new baseline, so
    /// the owed install and any forced pass are both consumed.
    fn note_installed(&mut self) {
        self.dirty_frames = 0;
        self.force_install = false;
    }
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
    examples: [Option<AssetId>; 4],
    /// The bundled Bangboo asset loaded by `--bangboo`; kept alive beyond its
    /// instances so the controls can add another pet without a reload. It
    /// never occupies one of the four example slots, whose meaning is pinned
    /// by the smoke and benchmark workloads.
    bangboo_asset: Option<AssetId>,
    next_request: u64,
    cursor: [f32; 2],
    /// Cursor position where the current drag gesture went down; only set
    /// while a drag is live. Release uses it to tell a click from a drag.
    press_cursor: Option<[f32; 2]>,
    /// Latched once the pointer crosses the click tolerance, even if it returns.
    drag_moved: bool,
    attention_poll: attention::AttentionPoll,
    dragging: Option<(InstanceId, Vec3)>,
    /// The region mode a drag suspended (`Pixel`), restored on the first
    /// frame after the drag ends.
    drag_region: Option<RegionMode>,
    hit_region: HitRegionCache,
    started: Instant,
    last_frame: Instant,
    next_frame: Instant,
    rendered_frames: u64,
    smoke: SmokeState,
    benchmark: Option<Benchmark>,
    /// The installed tray icon; `None` whenever the shell rejected it or a
    /// benchmark is running, in which case closing the control window quits
    /// as before.
    tray: Option<platform::TrayIcon>,
    /// The loaded configuration, kept unchanged until a clean exit rewrites
    /// it; `None` when persistence is disabled or the file was unusable.
    config: Option<config::Config>,
    /// Configured instances not yet submitted; drained by `poll_loads` in
    /// order, respecting the loader queue.
    initial_config: VecDeque<config::InstanceEntry>,
    /// A configured hit-test preference waiting for the runtime; applied
    /// through the normal command path once the windows exist.
    pending_pixel_hittest: Option<bool>,
    /// When the configuration file was last read for live preference changes,
    /// and the preferences applied from it; `None` disables the watch (the
    /// smoke test, benchmarks, `--bangboo`, `DELICIUM_NO_CONFIG`, and any run
    /// where the file did not load).
    config_watch: Option<ConfigWatch>,
    scene_sample: Option<SceneSample>,
    fatal: Option<String>,
}

impl Application {
    fn new(options: Options) -> Result<Self, String> {
        let now = Instant::now();
        let benchmark = options.bench.clone().map(Benchmark::new);
        let mut simulation = Simulation::new();
        simulation.set_attention_enabled(!options.smoke_test && benchmark.is_none());
        Ok(Self {
            options,
            runtime: None,
            simulation,
            loader: Loader::new()?,
            ui: ControlUi::default(),
            pending: HashMap::new(),
            initial_models: VecDeque::new(),
            latest_replacement: HashMap::new(),
            sources: HashMap::new(),
            examples: [None; 4],
            bangboo_asset: None,
            next_request: 1,
            cursor: [0.0; 2],
            press_cursor: None,
            drag_moved: false,
            attention_poll: attention::AttentionPoll::new(now),
            dragging: None,
            drag_region: None,
            hit_region: HitRegionCache {
                mode: RegionMode::Pixel,
                geometry: None,
                click_through: false,
                failed: false,
                dirty_frames: 0,
                force_install: false,
            },
            started: now,
            last_frame: now,
            next_frame: now,
            rendered_frames: 0,
            smoke: SmokeState::default(),
            benchmark,
            tray: None,
            config: None,
            initial_config: VecDeque::new(),
            pending_pixel_hittest: None,
            config_watch: None,
            scene_sample: None,
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
        // Ordinary launch uses a fixed snapshot of all physical monitors.
        // Explicit sizes, smoke and bench retain their bounded window path.
        let desktop = if self.options.overlay_size.is_none() {
            let monitors = event_loop
                .available_monitors()
                .map(|monitor| {
                    platform::DesktopRect::from_physical(monitor.position(), monitor.size())
                })
                .collect::<Result<Vec<_>, _>>()?;
            match (monitor.as_ref(), monitors.is_empty()) {
                (Some(primary), false) => Some(DesktopLayout::new(
                    platform::DesktopRect::from_physical(primary.position(), primary.size())?,
                    monitors,
                    scale as f32,
                )?),
                _ => None,
            }
        } else {
            None
        };
        let desktop_scene = desktop.is_some();
        let requested = self.options.overlay_size.unwrap_or([960, 640]);
        let width = monitor_size.map_or(requested[0] as f64, |size| {
            (requested[0] as f64).min((size.width - 430.0).max(320.0))
        });
        let height = monitor_size.map_or(requested[1] as f64, |size| {
            (requested[1] as f64).min((size.height - 110.0).max(240.0))
        });
        let overlay = Arc::new(if let Some(layout) = &desktop {
            let position = layout.bounds.position();
            let scene_size = layout.bounds.size();
            log::info!(
                "desktop scene monitors={} position=({}, {}) size={}x{}",
                layout.monitors.len(),
                position.x,
                position.y,
                scene_size.width,
                scene_size.height
            );
            event_loop
                .create_window(platform::overlay_attributes(
                    Window::default_attributes()
                        .with_title("Delicium Scene")
                        .with_visible(false)
                        .with_decorations(false)
                        .with_transparent(true)
                        .with_window_level(WindowLevel::AlwaysOnTop)
                        .with_inner_size(scene_size)
                        .with_position(position),
                ))
                .map_err(|error| error.to_string())?
        } else {
            event_loop
                .create_window(platform::overlay_attributes(
                    Window::default_attributes()
                        .with_title("Delicium Scene")
                        .with_visible(false)
                        .with_decorations(false)
                        .with_transparent(true)
                        .with_window_level(WindowLevel::AlwaysOnTop)
                        .with_inner_size(LogicalSize::new(width, height))
                        .with_position(PhysicalPosition::new(
                            origin.x + (30.0 * scale) as i32,
                            origin.y + (50.0 * scale) as i32,
                        )),
                ))
                .map_err(|error| error.to_string())?
        });
        if let Some(layout) = &desktop {
            platform::mark_non_fullscreen(&overlay)?;
            layout.restore_window(&overlay);
            log::info!(
                "desktop non_fullscreen=true primary_scale={:.4} center_x={:.6}",
                layout.scale,
                layout.view().center_x
            );
        }
        let scene_view = desktop.as_ref().map_or(LEGACY, DesktopLayout::view);
        log::info!(
            "scene_view mode={} span={:.6} bottom={:.6}",
            if desktop_scene { "desktop" } else { "bounded" },
            scene_view.span,
            scene_view.bottom
        );
        let control_height =
            monitor_size.map_or(680.0, |size| (size.height - 110.0).clamp(480.0, 680.0));
        // Desktop mode anchors the control window at the monitor's top-right
        // corner (50 logical pixels in from each edge; the 430 constant is the
        // 380-wide control plus that 50-pixel margin). Bounded mode keeps the
        // control window beside the scene exactly as before.
        let control_x = if desktop_scene {
            let monitor = monitor.as_ref().unwrap();
            monitor.position().x + monitor.size().width as i32 - (430.0 * scale) as i32
        } else {
            origin.x + ((width + 50.0) * scale) as i32
        };
        let control = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Delicium Controls")
                        .with_window_level(WindowLevel::AlwaysOnTop)
                        .with_inner_size(LogicalSize::new(380.0, control_height))
                        .with_position(PhysicalPosition::new(
                            control_x,
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
        // Default mode is per-character hit regions: normal hit testing with
        // an empty region until the first frame builds the character outlines,
        // so nothing is intercepted before anything is visible. If even the
        // empty region cannot be installed (non-Windows builds), the overlay
        // stays whole-window click-through, as it was before regions existed,
        // and holds that fallback until the user switches modes.
        platform::set_click_through(&overlay, &control, false)?;
        match platform::set_hit_region(&overlay, &[]) {
            Ok(()) => log::info!("hit_region mode=pixel polygons=0"),
            Err(error) => {
                platform::set_click_through(&overlay, &control, true)?;
                self.hit_region.mode = RegionMode::Fallback;
                self.hit_region.failed = true;
                self.hit_region.click_through = true;
                self.ui.pixel_hittest = false;
                log::info!("hit_region mode=fallback error={error}");
                self.report_error(format!(
                    "Character hit regions are unavailable; the scene is click-through until the mode changes: {error}"
                ));
            }
        }
        self.runtime = Some(Runtime {
            overlay,
            control,
            graphics,
            egui_context,
            egui_input,
            scene_view,
            desktop,
        });
        // The icon must not appear while a benchmark drives the windows;
        // scripts and harness runs assume closing a window ends the process.
        if self.benchmark.is_none() {
            match platform::tray::install() {
                Ok(tray) => {
                    log::info!("tray installed");
                    self.tray = Some(tray);
                }
                Err(error) => {
                    log::warn!("tray install failed: {error}");
                    self.report_error(format!(
                        "The system tray is unavailable; closing the control window will quit: {error}"
                    ));
                }
            }
        }
        log::info!(
            "wander mode={}",
            if self.options.wander { "on" } else { "off" }
        );
        {
            let runtime = self.runtime.as_ref().unwrap();
            self.simulation
                .set_wander_regions(runtime.desktop.as_ref().map(DesktopLayout::regions))
                .map_err(|error| error.to_string())?;
            sync_wander_bounds(
                &mut self.simulation,
                self.options.wander,
                runtime.scene_view,
                runtime.overlay.inner_size(),
            );
        }
        self.runtime.as_ref().unwrap().overlay.set_visible(true);
        self.started = Instant::now();
        self.last_frame = self.started;
        self.next_frame = self.started;
        // A configured hit-test preference goes through the normal command
        // path, so it gets the same fallback handling as a checkbox click.
        if self.pending_pixel_hittest.take() == Some(false) {
            self.apply_command(UiCommand::PixelHitTest(false), event_loop)?;
        }
        if let Some(options) = self.options.bench.clone() {
            let runtime = self.runtime.as_ref().unwrap();
            self.benchmark.as_mut().unwrap().configure(
                runtime.graphics.diagnostics(),
                self.options.overlay_size.unwrap_or([960, 640]),
                runtime.overlay.scale_factor(),
                runtime.control.scale_factor(),
                self.options.wander,
            )?;
            if options.mode == BenchMode::Interactive {
                self.apply_command(UiCommand::PixelHitTest(false), event_loop)?;
            }
            self.submit_load(
                LoadSource::Example2d,
                Operation::Spawn {
                    positions: benchmark::positions(options.instances, ModelKind::Sprite2d),
                },
            )?;
            self.submit_load(
                LoadSource::Example3d,
                Operation::Spawn {
                    positions: benchmark::positions(options.instances, ModelKind::Skeleton3d),
                },
            )?;
        } else if self.options.bangboo {
            // The pet scene: two instances of the same bundled asset at a
            // fixed pair of positions, close enough to find each other.
            self.submit_load(
                LoadSource::Bangboo,
                Operation::Spawn {
                    positions: vec![Vec3::new(-0.65, 0.15, 0.0), Vec3::new(0.65, 0.15, 0.0)],
                },
            )?;
        } else if !self.options.models.is_empty() {
            // Explicit `--model` arguments replace the configured scene
            // entirely, exactly as they replaced the default scene before.
            self.initial_models
                .extend(self.options.models.iter().cloned());
        } else if let Some(config) = &self.config {
            // The configuration owns the scene once the file exists: an
            // empty instance list means an empty scene, not the defaults.
            self.initial_config.extend(config.instances.iter().cloned());
        } else {
            self.submit_load(
                LoadSource::TexturedExample2d,
                Operation::Spawn {
                    positions: vec![Vec3::new(-2.4, 0.15, 0.0), Vec3::new(-0.8, 0.15, 0.0)],
                },
            )?;
            self.submit_load(
                LoadSource::TexturedExample3d,
                Operation::Spawn {
                    positions: vec![Vec3::new(0.8, 0.15, 0.0), Vec3::new(2.4, 0.15, 0.0)],
                },
            )?;
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
        while self.pending.len() < MAX_PENDING_LOADS {
            let Some(entry) = self.initial_config.pop_front() else {
                break;
            };
            let position = Vec3::from_array(entry.position);
            let source = entry.source.into_load_source();
            // A configured bundled example that is already in the cache
            // spawns immediately; everything else goes through the loader
            // queue like any other load.
            if let Some(index) = source.example_index()
                && let Some(asset) = self.examples[index]
            {
                match self.simulation.spawn(asset, position) {
                    Ok(instance) => {
                        self.sources.insert(instance, source);
                        if self.ui.selected.is_none() {
                            self.ui.selected = Some(instance);
                        }
                        if let Some(info) = self.simulation.instance(instance) {
                            log::info!(
                                "spawn instance={instance} asset={asset} kind={}",
                                info.kind
                            );
                        }
                    }
                    Err(error) => self.report_error(error.to_string()),
                }
                continue;
            }
            if let Err(error) = self.submit_load(
                source,
                Operation::Spawn {
                    positions: vec![position],
                },
            ) {
                self.report_error(error);
            }
        }
        self.ui.pending =
            self.pending.len() + self.initial_models.len() + self.initial_config.len();
        Ok(())
    }

    /// Captures the current scene and preferences for persistence on a clean
    /// exit. Instances without a persistable source (loaded in this session
    /// from nowhere, or smoke-test probes) are skipped; the order is by
    /// instance id so the file is stable across runs.
    fn capture_config(&self) -> config::Config {
        let mut instances: Vec<_> = self
            .simulation
            .instances()
            .into_iter()
            .filter_map(|info| {
                let source = self.sources.get(&info.id)?;
                let source = config::EntrySource::from_load_source(source)?;
                Some((
                    info.id,
                    config::InstanceEntry {
                        source,
                        position: info.position.to_array(),
                    },
                ))
            })
            .collect();
        instances.sort_by_key(|(id, _)| *id);
        config::Config {
            version: config::VERSION,
            pixel_hittest: self.ui.pixel_hittest,
            instances: instances.into_iter().map(|(_, entry)| entry).collect(),
        }
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
                if let Some(index) = completed.source.example_index() {
                    self.examples[index].get_or_insert(asset);
                }
                result
            }
            Operation::Replace { instance } => self
                .simulation
                .replace_model(instance, asset)
                .map(|()| {
                    self.sources.insert(instance, completed.source.clone());
                    if let Some(index) = completed.source.example_index() {
                        self.examples[index] = Some(asset);
                    }
                    log::info!(
                        "replace instance={instance} asset={asset} kind={}",
                        model.kind()
                    );
                })
                .map_err(|error| error.to_string()),
        };
        let succeeded = result.is_ok();
        self.note_bangboo_load(&completed.source, asset, succeeded);
        match result {
            Ok(()) => self.ui.status = format!("Loaded {}", model.name),
            Err(error) => self.report_error(error),
        }
        self.collect_assets();
        if succeeded
            && let Some(benchmark) = &mut self.benchmark
            && let Err(error) = benchmark.load_succeeded(completed.id, asset)
        {
            benchmark.fail(error);
        }
    }

    /// Remembers the bundled pet asset once a Bangboo load has actually been
    /// committed, so the cache outlives its instances and the controls can
    /// add another pet without a reload. Failed loads never reach this point.
    fn note_bangboo_load(&mut self, source: &LoadSource, asset: AssetId, succeeded: bool) {
        if succeeded && matches!(source, LoadSource::Bangboo) {
            self.bangboo_asset.get_or_insert(asset);
        }
    }

    fn collect_assets(&mut self) {
        let mut keep: Vec<_> = self.examples.iter().flatten().copied().collect();
        if let Some(asset) = self.bangboo_asset {
            keep.push(asset);
        }
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
        if self
            .benchmark
            .as_ref()
            .is_some_and(|b| b.phase != Phase::Setup)
            && !matches!(&command, UiCommand::Quit)
        {
            return Err("Benchmark workload modified through the controls".into());
        }
        match command {
            UiCommand::SpawnExample(kind)
                if self.options.bangboo && kind == ModelKind::Skeleton3d =>
            {
                // The pet scene adds another Bangboo instead of the old robot.
                let position = self.next_spawn_position();
                if let Some(asset) = self.bangboo_asset {
                    let instance = self
                        .simulation
                        .spawn(asset, position)
                        .map_err(|error| error.to_string())?;
                    self.sources.insert(instance, LoadSource::Bangboo);
                    self.ui.selected = Some(instance);
                    self.ui.status = "Added Bangboo".into();
                    log::info!("spawn instance={instance} asset={asset} kind={kind}");
                } else {
                    self.submit_load(
                        LoadSource::Bangboo,
                        Operation::Spawn {
                            positions: vec![position],
                        },
                    )?;
                }
            }
            UiCommand::SpawnExample(kind) => {
                let (source, index) = match (kind, self.options.bench.is_some()) {
                    (ModelKind::Sprite2d, true) => (LoadSource::Example2d, 0),
                    (ModelKind::Skeleton3d, true) => (LoadSource::Example3d, 1),
                    (ModelKind::Sprite2d, false) => (LoadSource::TexturedExample2d, 2),
                    (ModelKind::Skeleton3d, false) => (LoadSource::TexturedExample3d, 3),
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
                    log::info!("spawn instance={instance} asset={asset} kind={kind}");
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
                    self.end_drag();
                }
                if self.ui.selected == Some(instance) {
                    self.ui.selected = None;
                }
                self.collect_assets();
                log::info!("remove instance={instance}");
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
                // A position edit owns the character for as long as the user
                // drags the value; the walk is cancelled by set_position.
                if let Err(error) = self.simulation.suspend_wander(instance) {
                    self.report_error(error.to_string());
                }
                self.simulation
                    .set_position(instance, position)
                    .map_err(|error| error.to_string())?;
            }
            UiCommand::PixelHitTest(enabled) => {
                let target = if enabled {
                    RegionMode::Pixel
                } else {
                    RegionMode::Interactive
                };
                // An explicit mode switch clears the fallback latch, so a
                // failed mode can be retried, unlike the per-frame path. The
                // throttle state resets with the baseline it belongs to.
                self.hit_region.failed = false;
                self.hit_region.geometry = None;
                self.hit_region.dirty_frames = 0;
                self.hit_region.force_install = false;
                self.hit_region.mode = target;
                self.end_drag();
                self.drag_region = None;
                self.sync_hit_region_now()?;
                // Report the state that was actually reached: a switch that
                // fell back leaves the checkbox off, and the `interactive`
                // line is only logged by the switch that reached it.
                self.ui.pixel_hittest = enabled && self.hit_region.mode == RegionMode::Pixel;
            }
            UiCommand::Quit => event_loop.exit(),
        }
        Ok(())
    }

    fn redraw_scene(&mut self) -> Result<(), String> {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_frame);
        self.last_frame = now;
        self.scene_sample = self
            .benchmark
            .as_ref()
            .map(|b| SceneSample::new(b.phase, elapsed));
        self.ui.frame_ms = self.ui.frame_ms * 0.9 + elapsed.as_secs_f32() * 1000.0 * 0.1;
        self.poll_attention(now);
        let timer = self.bench_clock();
        let advanced = self.simulation.advance(elapsed);
        if let Some(sample) = &mut self.scene_sample {
            sample.advance = timer.map(|timer| timer.elapsed());
        }
        let report = advanced.map_err(|error| error.to_string())?;
        if let Some(sample) = &mut self.scene_sample {
            sample.steps = report.steps;
            sample.dropped_steps = report.dropped_steps;
        }
        // Drained here, right after the advance: every step of this frame has
        // already been recorded, and a zero-step frame cannot replay events.
        for event in self.simulation.drain_wander_events() {
            match event {
                WanderEvent::Start {
                    instance,
                    x,
                    y,
                    target_x,
                } => log::info!(
                    "wander start instance={instance} x={x:.3} y={y:.3} target={target_x:.3}"
                ),
                WanderEvent::Arrive { instance, x, y } => {
                    log::info!("wander arrive instance={instance} x={x:.3} y={y:.3}")
                }
            }
        }
        for event in self.simulation.drain_action_events() {
            log::info!(
                "action change instance={} action={} restart={}",
                event.instance,
                event.action.as_str(),
                event.restarted
            );
        }
        for event in self.simulation.drain_social_events() {
            log::info!(
                "social pair first={} second={} phase={}",
                event.first,
                event.second,
                event.phase.as_str()
            );
        }
        for event in self.simulation.drain_attention_events() {
            log::info!(
                "attention change instance={} source={}",
                event.instance,
                event.source.as_str()
            );
        }
        self.ui.tick_count = self.simulation.tick_count();
        self.ui.interactions = self.simulation.interactions().len();
        let timer = self.bench_clock();
        let generated = self.simulation.render_frame();
        if let Some(sample) = &mut self.scene_sample {
            sample.render_frame = timer.map(|timer| timer.elapsed());
        }
        let frame = generated.map_err(|error| error.to_string())?;
        if self.options.smoke_test {
            self.observe_smoke_frame(&frame);
        }
        self.restore_region_after_drag();
        self.sync_hit_region(&frame)?;
        if let Some(runtime) = &mut self.runtime {
            let view = runtime.scene_view;
            if let Some(sample) = &mut self.scene_sample {
                let measured = runtime
                    .graphics
                    .render_scene_measured(&frame, view)
                    .map_err(|error| error.to_string())?;
                sample.outcome = Some(measured.outcome);
                sample.graphics = measured.timings;
            } else {
                runtime
                    .graphics
                    .render_scene(&frame, view)
                    .map_err(|error| error.to_string())?;
            }
        }
        if let Some(mut sample) = self.scene_sample.take() {
            sample.callback = Some(now.elapsed());
            let submitted = sample.outcome == Some(SceneFrameOutcome::Submitted);
            self.benchmark.as_mut().unwrap().record_scene(sample)?;
            if submitted {
                // The benchmark loads the two untextured examples into slots
                // 0 and 1; slots 2/3 hold the textured variants used by
                // ordinary launches and stay empty under --bench, so only
                // those two slots can gate readiness.
                let ready_target = if self.pending.is_empty()
                    && self.examples[0].is_some()
                    && self.examples[1].is_some()
                    && self.simulation.asset_count() == 2
                    && frame.instances.len() == self.benchmark.as_ref().unwrap().options.instances
                {
                    frame.instances.iter().find_map(|draw| match draw {
                        InstanceDraw::Sprite2d { instance, .. } => Some(*instance),
                        _ => None,
                    })
                } else {
                    None
                };
                let benchmark = self.benchmark.as_mut().unwrap();
                let elapsed = benchmark.elapsed();
                let confirmation = benchmark.on_submission(
                    elapsed,
                    ready_target,
                    frame.instances.iter().map(draw_identity),
                )?;
                if let Some(confirmation) = confirmation {
                    let data = self.benchmark_snapshot()?;
                    self.benchmark.as_ref().unwrap().reload_confirmed(
                        elapsed,
                        confirmation,
                        data,
                    )?;
                }
            }
        }
        self.rendered_frames += 1;
        Ok(())
    }

    fn bench_clock(&self) -> Option<Instant> {
        self.benchmark.as_ref().map(|_| Instant::now())
    }

    fn redraw_control(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let measurement = self.benchmark.as_ref().map(|b| (b.phase, Instant::now()));
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
        if let Some((phase, timer)) = measurement {
            self.benchmark
                .as_mut()
                .unwrap()
                .record_control(phase, timer.elapsed())?;
        }
        Ok(())
    }

    fn benchmark_snapshot(&self) -> Result<serde_json::Value, String> {
        let benchmark = self.benchmark.as_ref().ok_or("Benchmark not enabled")?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or("Benchmark runtime unavailable")?;
        let instances = self.simulation.instances();
        let expected =
            benchmark::expected_assets(instances.iter().map(|info| info.asset), &self.examples);
        let graphics = runtime.graphics.diagnostics();
        benchmark.validate_windows(
            [graphics.physical_size, graphics.control_physical_size],
            [
                runtime.overlay.scale_factor(),
                runtime.control.scale_factor(),
            ],
        )?;
        let gpu = runtime.graphics.scene_resource_diagnostics();
        let cpu = self.simulation.asset_count();
        if cpu != expected || gpu.asset_count != expected {
            return Err(format!(
                "Benchmark asset mismatch: cpu={cpu} gpu={} expected={expected}",
                gpu.asset_count
            ));
        }
        if instances.len() != benchmark.options.instances {
            return Err("Benchmark instance count changed".into());
        }
        let mode = match self.hit_region.mode {
            RegionMode::Pixel => "pixel",
            RegionMode::Interactive => "interactive",
            RegionMode::FullWindow => "full-window",
            RegionMode::Fallback => "fallback",
        };
        if mode != benchmark.options.mode.as_str() {
            return Err("Benchmark input mode changed or entered fallback".into());
        }
        Ok(serde_json::json!({
            "instance_count": instances.len(),
            "sprite_instances": instances.iter().filter(|i| i.kind == ModelKind::Sprite2d).count(),
            "skeleton_instances": instances.iter().filter(|i| i.kind == ModelKind::Skeleton3d).count(),
            "cpu_assets": cpu, "gpu_assets": gpu.asset_count, "expected_assets": expected,
            "mask_bytes": gpu.mask_bytes, "sprite_vertex_capacity_bytes": gpu.sprite_vertex_capacity_bytes,
            "instance_capacity_bytes": gpu.instance_capacity_bytes, "palette_capacity_bytes": gpu.palette_capacity_bytes,
            "pending": self.pending.len(), "region_mode": mode,
        }))
    }

    fn advance_benchmark(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let Some(benchmark) = &mut self.benchmark else {
            return Ok(());
        };
        let now = benchmark.elapsed();
        benchmark.update(now)?;
        if benchmark.stopped() {
            event_loop.exit();
            return Ok(());
        }
        if let Some((instance, is_3d)) = benchmark.reload_due(now) {
            let source = if is_3d {
                LoadSource::Example3d
            } else {
                LoadSource::Example2d
            };
            let request = self.submit_load(source, Operation::Replace { instance })?;
            self.benchmark
                .as_mut()
                .unwrap()
                .begin_reload(request, instance, now)?;
        }
        let benchmark = self.benchmark.as_mut().unwrap();
        if benchmark.phase != Phase::Setup && benchmark.snapshot_due(now) {
            let data = self.benchmark_snapshot()?;
            self.benchmark.as_ref().unwrap().snapshot(now, data)?;
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
            let was_enabled = self.ui.pixel_hittest;
            self.apply_command(UiCommand::PixelHitTest(false), event_loop)?;
            self.smoke.input_mode_toggled = was_enabled
                && !self.ui.pixel_hittest
                && self.hit_region.mode == RegionMode::Interactive;
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
            let was_disabled = !self.ui.pixel_hittest;
            self.apply_command(UiCommand::PixelHitTest(true), event_loop)?;
            // The mode must actually be back on the region, not merely show
            // the checkbox as checked (a fallback would leave it unchecked).
            self.smoke.input_mode_toggled &= was_disabled
                && self.ui.pixel_hittest
                && self.hit_region.mode == RegionMode::Pixel
                && !self.hit_region.failed;
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
        let point =
            screen_to_world_with(self.cursor, [size.width, size.height], runtime.scene_view);
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
        if let Some(benchmark) = &mut self.benchmark {
            benchmark.fail(error.clone());
        }
        self.ui.last_error = Some(error);
    }

    // -- hit region ----------------------------------------------------------

    fn refresh_hit_region_after_resize(&mut self) -> Result<(), String> {
        self.hit_region.geometry = None;
        self.hit_region.dirty_frames = 0;
        self.hit_region.force_install = true;
        // A drag deliberately keeps whole-window capture. Fallback must stay
        // click-through; only the pixel mode has geometry tied to this view.
        if self.hit_region.mode == RegionMode::Pixel {
            let runtime = self.runtime.as_ref().ok_or("Windows are not ready")?;
            let size = runtime.overlay.inner_size();
            if size.width == 0 || size.height == 0 {
                if let Err(error) = platform::set_hit_region(&runtime.overlay, &[]) {
                    self.enter_fallback(format!("Cannot clear resized hit regions: {error}"))?;
                }
            } else {
                self.sync_hit_region_now()?;
            }
        }
        Ok(())
    }

    /// Installs the target mode's region immediately, for mode switches and
    /// the start of a drag. A failed build or native call enters the
    /// click-through fallback and returns `Ok`; an error is only returned
    /// when the fallback itself could not be established, because then the
    /// window could be left intercepting input and the app must not run on.
    fn sync_hit_region_now(&mut self) -> Result<(), String> {
        match self.hit_region.mode {
            RegionMode::Interactive | RegionMode::FullWindow => {
                let label = if self.hit_region.mode == RegionMode::Interactive {
                    "interactive"
                } else {
                    "full-window"
                };
                if let Err(error) = self.apply_full_input() {
                    self.enter_fallback(format!("Cannot restore whole-window input: {error}"))?;
                    return Ok(());
                }
                log::info!("hit_region mode={label}");
                Ok(())
            }
            RegionMode::Fallback => Ok(()),
            RegionMode::Pixel => {
                let geometry = match self.build_geometry(None) {
                    Ok(geometry) => geometry,
                    Err(error) => {
                        self.enter_fallback(format!(
                            "Cannot build character hit regions: {error}"
                        ))?;
                        return Ok(());
                    }
                };
                if let Err(error) = self.install_pixel_geometry(geometry) {
                    self.enter_fallback(format!("Cannot install character hit regions: {error}"))?;
                }
                Ok(())
            }
        }
    }

    /// Per-frame pixel-region update, right before the frame is drawn so the
    /// installed region matches the pose on screen.
    fn sync_hit_region(&mut self, frame: &RenderFrame) -> Result<(), String> {
        if self.hit_region.mode != RegionMode::Pixel || self.hit_region.failed {
            return Ok(());
        }
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        let size = runtime.overlay.inner_size();
        let size = [size.width, size.height];
        let view = runtime.scene_view;
        let timer = self.bench_clock();
        let collected = HitGeometry::collect_frame_with_view(&self.simulation, frame, size, view);
        if let Some(sample) = &mut self.scene_sample {
            sample.hit_geometry = timer.map(|timer| timer.elapsed());
        }
        let geometry = match collected {
            Ok(geometry) => geometry,
            Err(error) => {
                if let Some(sample) = &mut self.scene_sample {
                    sample.region = RegionDecision::Failed;
                }
                self.enter_fallback(format!("Cannot build character hit regions: {error}"))?;
                return Ok(());
            }
        };
        let timer = self.bench_clock();
        let decision = self.hit_region.decision(&geometry);
        if let Some(sample) = &mut self.scene_sample {
            sample.hit_compare = timer.map(|timer| timer.elapsed());
            sample.polygons = self
                .hit_region
                .geometry
                .as_ref()
                .map_or(0, |previous| previous.polygons().len());
            sample.region = decision;
        }
        match decision {
            RegionDecision::Reused => {
                log::trace!("hit_region mode=pixel unchanged");
                return Ok(());
            }
            RegionDecision::Deferred => {
                log::trace!(
                    "hit_region mode=pixel deferred dirty_frames={}",
                    self.hit_region.dirty_frames
                );
                return Ok(());
            }
            _ => {}
        }
        if let Err(error) = self.install_pixel_geometry(geometry) {
            if let Some(sample) = &mut self.scene_sample {
                sample.region = RegionDecision::Failed;
            }
            self.enter_fallback(format!("Cannot install character hit regions: {error}"))?;
        } else if let Some(sample) = &mut self.scene_sample {
            sample.region = RegionDecision::Rebuilt;
        }
        Ok(())
    }

    /// Installs `geometry` as the window region and makes it the new
    /// comparison baseline. Mode transitions log at info; per-frame rebuilds
    /// of an already-installed mode log at trace.
    fn install_pixel_geometry(&mut self, mut geometry: HitGeometry) -> Result<(), String> {
        let Some(runtime) = &self.runtime else {
            return Ok(());
        };
        let timer = self.bench_clock();
        geometry.materialize()?;
        if let Some(elapsed) = timer.map(|timer| timer.elapsed())
            && let Some(sample) = &mut self.scene_sample
        {
            sample.hit_geometry = Some(sample.hit_geometry.unwrap_or_default() + elapsed);
            sample.polygons = geometry.polygons().len();
        }
        let timer = self.bench_clock();
        let installed = platform::set_hit_region(&runtime.overlay, geometry.polygons());
        if let Some(sample) = &mut self.scene_sample {
            sample.native_region_build_install = timer.map(|timer| timer.elapsed());
        }
        installed?;
        if self.hit_region.click_through {
            // Leaving the click-through fallback: the target region is
            // already installed, so enabling hit testing cannot leave the
            // whole window intercepting input in between.
            platform::set_click_through(&runtime.overlay, &runtime.control, false)?;
            self.hit_region.click_through = false;
        }
        let count = geometry.polygons().len();
        let first = self.hit_region.geometry.is_none();
        self.hit_region.geometry = Some(geometry);
        self.hit_region.note_installed();
        if first {
            log::info!("hit_region mode=pixel polygons={count}");
        } else {
            log::trace!("hit_region mode=pixel polygons={count} rebuilt");
        }
        Ok(())
    }

    /// Suspends the character regions so a drag may leave the character; the
    /// mode is restored by [`Self::restore_region_after_drag`].
    fn begin_region_drag(&mut self) -> Result<(), String> {
        if self.hit_region.mode != RegionMode::Pixel {
            return Ok(());
        }
        self.drag_region = Some(RegionMode::Pixel);
        self.hit_region.mode = RegionMode::FullWindow;
        // The installed region is dropped; the comparison baseline goes with
        // it, so the restore rebuilds instead of trusting stale geometry.
        self.hit_region.geometry = None;
        self.hit_region.dirty_frames = 0;
        self.hit_region.force_install = false;
        self.sync_hit_region_now()
    }

    /// Ends the current drag, if any, and releases the character's parked
    /// wandering. The single exit every drag-terminating path goes through:
    /// a release, the cursor leaving the overlay, focus loss, a mode switch,
    /// the fallback latch, or the character being removed.
    ///
    /// The resume is skipped when the instance is already gone, so removing a
    /// dragged character reports no error. Every other case resumes with a
    /// fresh idle pause, so a released character never leaves immediately.
    fn end_drag(&mut self) {
        self.finish_drag(false);
    }

    fn note_drag_motion(&mut self) {
        if self.drag_moved
            || self
                .press_cursor
                .is_none_or(|press| is_click_press(press, self.cursor))
        {
            return;
        }
        if let Some((instance, _)) = self.dragging {
            self.drag_moved = true;
            if let Err(error) = self.simulation.begin_drag_feedback(instance) {
                self.report_error(error.to_string());
            }
        }
    }

    fn finish_drag(&mut self, released: bool) {
        self.press_cursor = None;
        let moved = std::mem::take(&mut self.drag_moved);
        let Some((instance, _)) = self.dragging.take() else {
            return;
        };
        if self.simulation.instance(instance).is_none() {
            return;
        }
        if let Err(error) = self
            .simulation
            .end_drag_feedback(instance, released && moved)
        {
            self.report_error(error.to_string());
        }
        if let Err(error) = self.simulation.resume_wander(instance) {
            self.report_error(error.to_string());
        }
    }

    /// Reacts to a click on a character: turns it toward the cursor and
    /// starts the short surprise hop. Logged like the drag markers so GUI
    /// scripts can assert on the exact instance and point.
    fn poke_character(&mut self, instance: InstanceId) {
        let Some(runtime) = &self.runtime else {
            return;
        };
        let size = runtime.overlay.inner_size();
        let point =
            screen_to_world_with(self.cursor, [size.width, size.height], runtime.scene_view);
        let target = Vec3::new(point[0], point[1], 0.0);
        match self.simulation.poke_reaction(instance, target) {
            Ok(()) => {
                if let Some(info) = self.simulation.instance(instance) {
                    log::info!(
                        "poke instance={} x={:.3} y={:.3}",
                        instance,
                        info.position.x,
                        info.position.y
                    );
                }
            }
            Err(error) => self.report_error(error.to_string()),
        }
    }

    /// Restores the mode a drag suspended, on the first frame after the drag
    /// ended (release, cursor leaving the overlay, or focus loss). The next
    /// changed frame installs immediately: a released character must never
    /// run on the empty region or wait out the throttle.
    fn restore_region_after_drag(&mut self) {
        if self.dragging.is_none()
            && let Some(saved) = self.drag_region.take()
        {
            self.hit_region.mode = saved;
            self.hit_region.geometry = None;
            self.hit_region.dirty_frames = 0;
            // `drag_region` is only ever `Some(Pixel)` (begin_region_drag
            // guards on the mode), so the forced pass always lands.
            self.hit_region.force_install = true;
        }
    }

    /// Whole-window input: normal hit testing, no native region. Used by the
    /// `Interactive` and `FullWindow` modes. When coming out of the fallback
    /// the style is flipped first: the region is already clear, so the window
    /// must not start intercepting input before the target region is ready.
    fn apply_full_input(&mut self) -> Result<(), String> {
        let Some(runtime) = &self.runtime else {
            return Ok(());
        };
        if self.hit_region.click_through {
            platform::set_click_through(&runtime.overlay, &runtime.control, false)?;
            self.hit_region.click_through = false;
        } else {
            platform::clear_hit_region(&runtime.overlay)?;
        }
        Ok(())
    }

    /// Whole-window click-through: the native region is cleared first and the
    /// overlay stops hit testing entirely (the fallback state).
    fn apply_click_through(&mut self) -> Result<(), String> {
        let Some(runtime) = &self.runtime else {
            return Ok(());
        };
        platform::clear_hit_region(&runtime.overlay)?;
        platform::set_click_through(&runtime.overlay, &runtime.control, true)?;
        self.hit_region.click_through = true;
        Ok(())
    }

    fn build_geometry(&self, frame: Option<&RenderFrame>) -> Result<HitGeometry, String> {
        let runtime = self.runtime.as_ref().ok_or("Windows are not ready")?;
        let size = runtime.overlay.inner_size();
        let size = [size.width, size.height];
        let view = runtime.scene_view;
        let frame = match frame {
            Some(frame) => frame.clone(),
            None => self
                .simulation
                .render_frame()
                .map_err(|error| error.to_string())?,
        };
        HitGeometry::collect_frame_with_view(&self.simulation, &frame, size, view)
    }

    /// Region building or installation failed: clear the region, make the
    /// whole window click-through and hold that state until the user switches
    /// modes again. Returns a fatal error when even the fallback cannot be
    /// established, because the window would be left intercepting input.
    fn enter_fallback(&mut self, error: String) -> Result<(), String> {
        if self.runtime.is_none() {
            return Ok(());
        }
        let first = !self.hit_region.failed;
        self.hit_region.mode = RegionMode::Fallback;
        self.hit_region.failed = true;
        self.hit_region.geometry = None;
        self.hit_region.dirty_frames = 0;
        self.hit_region.force_install = false;
        self.end_drag();
        self.drag_region = None;
        if first && self.ui.pixel_hittest {
            self.ui.pixel_hittest = false;
            log::warn!("hit regions disabled after a failure; the checkbox reflects the fallback");
        }
        match self.apply_click_through() {
            Ok(()) => {
                log::info!("hit_region mode=fallback error={error}");
                if first {
                    self.report_error(format!(
                        "Character hit regions failed; the scene is click-through until the mode changes: {error}"
                    ));
                }
                Ok(())
            }
            Err(apply_error) => Err(format!(
                "Cannot apply the hit-region fallback ({apply_error}); the scene window may intercept input: {error}"
            )),
        }
    }

    /// Applies preference changes the settings page wrote while the pet runs.
    ///
    /// The file is re-read only when its modification time or size changed;
    /// a scene whose loader would have to change is left alone, because
    /// restarting the scene mid-run would drop whatever the user was doing.
    fn poll_config(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        // The watch is off for bounded runs, `--bangboo`, the smoke test and
        // `DELICIUM_NO_CONFIG`; this runs every frame, so it must not touch
        // the filesystem when nothing is watching.
        if self.config_watch.is_none() {
            return Ok(());
        }
        let Some((modified, length)) = config::stamp() else {
            return Ok(());
        };
        let Some(watch) = self.config_watch.as_mut() else {
            return Ok(());
        };
        if watch.modified == modified && watch.length == length {
            return Ok(());
        }
        watch.modified = modified;
        watch.length = length;
        let Some(preferences) = config::read_preferences() else {
            return Ok(());
        };
        let changed = preferences.pixel_hittest != watch.preferences.pixel_hittest;
        watch.preferences = preferences;
        if changed {
            log::info!(
                "applying the configured hit-test preference: pixel_hittest={}",
                preferences.pixel_hittest
            );
            self.apply_command(
                UiCommand::PixelHitTest(preferences.pixel_hittest),
                event_loop,
            )?;
        }
        Ok(())
    }

    fn fail(&mut self, error: String, event_loop: &ActiveEventLoop) {
        log::error!("{error}");
        if let Some(benchmark) = &mut self.benchmark {
            if let Some(sample) = self.scene_sample.take()
                && let Err(sample_error) = benchmark.record_scene(sample)
            {
                log::warn!("{sample_error}");
            }
            benchmark.fail(error.clone());
        }
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
            WindowEvent::CloseRequested => {
                if is_control && self.tray.is_some() {
                    runtime.control.set_visible(false);
                    log::info!("control hidden to tray");
                } else {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(benchmark) = &mut self.benchmark
                    && benchmark.phase != Phase::Setup
                {
                    let diagnostics = runtime.graphics.diagnostics();
                    let previous = if is_overlay {
                        diagnostics.physical_size
                    } else {
                        diagnostics.control_physical_size
                    };
                    if previous != [size.width, size.height] {
                        benchmark.fail("Benchmark window dimensions changed".into());
                    }
                }
                let size = if is_overlay && size.width > 0 && size.height > 0 {
                    if let Some(layout) = &runtime.desktop {
                        layout.restore_window(&runtime.overlay);
                        match layout.window_view(&runtime.overlay) {
                            Ok(view) => runtime.scene_view = view,
                            Err(error) => {
                                self.fail(error, event_loop);
                                return;
                            }
                        }
                    }
                    let actual = runtime.overlay.inner_size();
                    sync_wander_bounds(
                        &mut self.simulation,
                        self.options.wander,
                        runtime.scene_view,
                        actual,
                    );
                    actual
                } else {
                    size
                };
                if let Err(error) = runtime.graphics.resize(window_id, size) {
                    self.fail(error.to_string(), event_loop);
                    return;
                }
                if is_overlay && let Err(error) = self.refresh_hit_region_after_resize() {
                    self.fail(error, event_loop);
                }
            }
            WindowEvent::ScaleFactorChanged {
                mut inner_size_writer,
                ..
            } => {
                if let Some(benchmark) = &mut self.benchmark
                    && benchmark.phase != Phase::Setup
                {
                    benchmark.fail("Benchmark window scale factor changed".into());
                }
                let window = if is_control {
                    &runtime.control
                } else {
                    &runtime.overlay
                };
                if is_overlay {
                    if let Some(layout) = &runtime.desktop {
                        if let Err(error) =
                            inner_size_writer.request_inner_size(layout.bounds.size())
                        {
                            self.fail(
                                format!("Cannot preserve desktop size after DPI change: {error}"),
                                event_loop,
                            );
                            return;
                        }
                        layout.restore_window(window);
                        if window.inner_size().width > 0 && window.inner_size().height > 0 {
                            match layout.window_view(window) {
                                Ok(view) => runtime.scene_view = view,
                                Err(error) => {
                                    self.fail(error, event_loop);
                                    return;
                                }
                            }
                        }
                    }
                    sync_wander_bounds(
                        &mut self.simulation,
                        self.options.wander,
                        runtime.scene_view,
                        window.inner_size(),
                    );
                }
                if let Err(error) = runtime.graphics.resize(window_id, window.inner_size()) {
                    self.fail(error.to_string(), event_loop);
                    return;
                }
                window.request_redraw();
                if is_overlay && let Err(error) = self.refresh_hit_region_after_resize() {
                    self.fail(error, event_loop);
                }
            }
            WindowEvent::Moved(_) if is_overlay && runtime.desktop.is_some() => {
                let layout = runtime.desktop.as_ref().unwrap();
                layout.restore_window(&runtime.overlay);
                let size = runtime.overlay.inner_size();
                if size.width > 0 && size.height > 0 {
                    match layout.window_view(&runtime.overlay) {
                        Ok(view) => runtime.scene_view = view,
                        Err(error) => {
                            self.fail(error, event_loop);
                            return;
                        }
                    }
                    sync_wander_bounds(
                        &mut self.simulation,
                        self.options.wander,
                        runtime.scene_view,
                        size,
                    );
                    if let Err(error) = runtime.graphics.resize(window_id, size) {
                        self.fail(error.to_string(), event_loop);
                        return;
                    }
                    if let Err(error) = self.refresh_hit_region_after_resize() {
                        self.fail(error, event_loop);
                    }
                }
            }
            WindowEvent::Focused(true) if is_overlay => platform::raise_controls(&runtime.control),
            WindowEvent::Focused(false) if is_overlay => self.end_drag(),
            WindowEvent::DroppedFile(path) => {
                if self.benchmark.is_some() {
                    self.fail(
                        "Benchmark workload modified by a dropped file".into(),
                        event_loop,
                    );
                    return;
                }
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
                self.note_drag_motion();
                if let Some((instance, offset)) = self.dragging {
                    let probe = self
                        .runtime
                        .as_ref()
                        .map(|runtime| (runtime.overlay.inner_size(), runtime.scene_view));
                    if let Some((size, view)) = probe {
                        let point =
                            screen_to_world_with(self.cursor, [size.width, size.height], view);
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
            } if is_overlay => {
                if self.benchmark.is_some() {
                    self.fail("Benchmark scene received mouse input".into(), event_loop);
                    return;
                }
                // Picking needs a window that can actually receive input: the
                // click-through fallback must never pick, while an ongoing
                // drag still gets its release.
                let pick_allowed =
                    self.hit_region.mode != RegionMode::Fallback || self.dragging.is_some();
                if state == ElementState::Released {
                    let press = self.press_cursor.take();
                    if let Some((instance, _)) = self.dragging
                        && let Some(info) = self.simulation.instance(instance)
                    {
                        log::info!(
                            "drag release instance={} x={:.3} y={:.3}",
                            instance,
                            info.position.x,
                            info.position.y
                        );
                    }
                    // A press and release on the same character with almost
                    // no travel is a click (a poke), not a drag; classify
                    // before ending the drag, which clears the state.
                    let clicked = self.dragging.is_some()
                        && !self.drag_moved
                        && press.is_some_and(|press| is_click_press(press, self.cursor));
                    let released = self.dragging;
                    self.finish_drag(true);
                    if clicked && let Some((instance, _)) = released {
                        self.poke_character(instance);
                    }
                } else if pick_allowed
                    && let Some(instance) = self.pick_character()
                    && let Some(info) = self.simulation.instance(instance)
                    && let Some(runtime) = &self.runtime
                {
                    let size = runtime.overlay.inner_size();
                    let view = runtime.scene_view;
                    // Suspend the character region for the gesture: the
                    // pointer may leave the character without losing the
                    // drag, and the region is rebuilt on the first frame
                    // after the release.
                    if let Err(error) = self.begin_region_drag() {
                        self.fail(error, event_loop);
                        return;
                    }
                    if self.hit_region.mode == RegionMode::Fallback {
                        return;
                    }
                    let point = screen_to_world_with(self.cursor, [size.width, size.height], view);
                    self.dragging =
                        Some((instance, info.position - Vec3::new(point[0], point[1], 0.0)));
                    self.press_cursor = Some(self.cursor);
                    self.drag_moved = false;
                    self.ui.selected = Some(instance);
                    // Park the character: a press must not let it walk away
                    // from under the pointer, even if the pointer holds still
                    // for longer than one idle period.
                    if let Err(error) = self.simulation.suspend_wander(instance) {
                        self.report_error(error.to_string());
                    }
                    log::info!(
                        "drag start instance={} x={:.3} y={:.3}",
                        instance,
                        info.position.x,
                        info.position.y
                    );
                }
            }
            WindowEvent::CursorLeft { .. } if is_overlay => self.end_drag(),
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
            if let Some(benchmark) = &mut self.benchmark
                && !benchmark.stopped()
            {
                benchmark.fail("Benchmark emergency exit limit reached".into());
            }
            log::info!(
                "exit frames={} ticks={} assets={}",
                self.rendered_frames,
                self.simulation.tick_count(),
                self.simulation.asset_count()
            );
            event_loop.exit();
            return;
        }
        if let Some(tray) = &self.tray {
            while let Some(action) = tray.take_action() {
                match action {
                    platform::TrayAction::ToggleControls => {
                        if let Some(runtime) = &self.runtime {
                            let visible = runtime.control.is_visible().unwrap_or(true);
                            runtime.control.set_visible(!visible);
                            if !visible {
                                platform::raise_controls(&runtime.control);
                            }
                            log::info!("tray toggle controls visible={}", !visible);
                        }
                    }
                    platform::TrayAction::Quit => {
                        log::info!("tray quit");
                        event_loop.exit();
                        return;
                    }
                    platform::TrayAction::Settings => match platform::tray::open_settings() {
                        Ok(()) => log::info!("tray settings handed to the launcher"),
                        Err(error) => log::warn!("tray settings failed: {error}"),
                    },
                    platform::TrayAction::Reinstall => match tray.reinstall_icon() {
                        Ok(()) => log::info!("tray icon reinstalled"),
                        Err(error) => log::warn!("tray reinstall failed: {error}"),
                    },
                }
            }
        }
        if let Err(error) = self
            .poll_config(event_loop)
            .and_then(|()| self.poll_loads())
            .and_then(|()| self.advance_smoke(event_loop))
            .and_then(|()| self.advance_benchmark(event_loop))
        {
            self.fail(error, event_loop);
            return;
        }
        if self.benchmark.as_ref().is_some_and(Benchmark::stopped) {
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
            // A hidden control window has nothing to present; skipping its
            // redraw avoids presenting into an invisible surface. The tick
            // resumes drawing the frame it becomes visible again.
            let control_visible = self
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.control.is_visible())
                .unwrap_or(true);
            if control_visible && let Err(error) = self.redraw_control(event_loop) {
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

fn draw_identity(draw: &InstanceDraw) -> (u64, u64) {
    match draw {
        InstanceDraw::Sprite2d {
            instance, asset, ..
        }
        | InstanceDraw::Skeleton3d {
            instance, asset, ..
        } => (*instance, *asset),
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

    fn monitor_rect(x: i32, y: i32, width: u32, height: u32) -> platform::DesktopRect {
        platform::DesktopRect::from_physical(
            PhysicalPosition::new(x, y),
            PhysicalSize::new(width, height),
        )
        .unwrap()
    }

    #[test]
    fn staggered_desktop_preserves_primary_anchor_and_monitor_regions() {
        let primary = monitor_rect(0, 0, 2560, 1600);
        let secondary = monitor_rect(2560, -511, 1600, 2560);
        let layout = DesktopLayout::new(primary, vec![primary, secondary], 1.5).unwrap();
        let view = layout.view();
        assert_eq!(view.span, 12.0);
        assert_eq!(view.center_x, 3.75);
        assert!((view.bottom + 3.0046875).abs() < 0.00001);
        let bounds = wander_bounds(view, layout.bounds.size());
        assert!((bounds.left + 6.0).abs() < 0.00001);
        assert!((bounds.right - 13.5).abs() < 0.00001);
        for (world_x, screen_x) in [
            (-2.4, 768.0),
            (-0.8, 1109.3334),
            (0.8, 1450.6666),
            (2.4, 1792.0),
        ] {
            let point = crate::renderer::world_to_screen_with([world_x, 0.15], [4160, 2560], view);
            assert!((point[0] - screen_x).abs() < 0.001);
            assert!((point[1] - 511.0 - 1376.0).abs() < 0.001);
        }
        let regions = layout.regions();
        assert_eq!(regions.len(), 2);
        assert!((regions[0].right - regions[1].left).abs() < 0.00001);
        for screen_y in [-200.0, 800.0, 1800.0] {
            let y = screen_to_world_with([0.0, screen_y + 511.0], [4160, 2560], view)[1];
            let visible: Vec<_> = regions
                .iter()
                .filter(|rect| rect.bottom < y && y <= rect.top)
                .collect();
            assert_eq!(visible.len(), if screen_y == 800.0 { 2 } else { 1 });
            if visible.len() == 1 {
                assert!((visible[0].left - 6.0).abs() < 0.00001);
                assert!((visible[0].right - 13.5).abs() < 0.00001);
            }
        }
    }

    #[test]
    fn single_negative_monitor_degenerates_to_original_desktop_view() {
        let primary = monitor_rect(-1920, -300, 1920, 1080);
        let layout = DesktopLayout::new(primary, vec![primary], 1.25).unwrap();
        assert_eq!(layout.view(), crate::renderer::desktop_view(1080, 1.25));
        assert_eq!(layout.bounds, primary);
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(DesktopLayout::new(primary, vec![primary], scale).is_err());
        }
        assert!(DesktopLayout::new(primary, vec![], 1.0).is_err());
    }

    #[test]
    fn negative_left_and_upper_monitor_does_not_shift_primary_world_positions() {
        let primary = monitor_rect(100, 200, 1920, 1080);
        let secondary = monitor_rect(-1180, -824, 1280, 1024);
        let layout = DesktopLayout::new(primary, vec![primary, secondary], 1.25).unwrap();
        let size = layout.bounds.size();
        let old = crate::renderer::desktop_view(1080, 1.25);
        for world in [[-2.4, 0.15], [0.0, 2.0], [2.4, -0.5]] {
            let before = crate::renderer::world_to_screen_with(world, [1920, 1080], old);
            let after = crate::renderer::world_to_screen_with(
                world,
                [size.width, size.height],
                layout.view(),
            );
            assert!(
                (after[0] + layout.bounds.left as f32 - before[0] - primary.left as f32).abs()
                    < 0.001
            );
            assert!(
                (after[1] + layout.bounds.top as f32 - before[1] - primary.top as f32).abs()
                    < 0.001
            );
        }
    }
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
    fn click_classification_uses_a_strict_tolerance() {
        // No travel at all.
        assert!(is_click_press([100.0, 200.0], [100.0, 200.0]));
        // Just inside the tolerance on one axis.
        assert!(is_click_press([100.0, 200.0], [103.9, 200.0]));
        // Exactly at the tolerance is already a drag.
        assert!(!is_click_press([100.0, 200.0], [104.0, 200.0]));
        // Vertical travel counts the same as horizontal.
        assert!(!is_click_press([100.0, 200.0], [100.0, 204.0]));
        assert!(is_click_press([100.0, 200.0], [100.0, 203.9]));
        // Diagonal travel compares the true distance: 2.8^2 + 2.8^2 < 16.
        assert!(is_click_press([100.0, 200.0], [102.8, 202.8]));
        assert!(!is_click_press([100.0, 200.0], [103.0, 203.0]));
    }

    #[test]
    fn crossing_click_tolerance_is_latched_even_after_returning_to_press() {
        let (mut app, instance, _) = application_with_character();
        app.dragging = Some((instance, Vec3::ZERO));
        app.press_cursor = Some([100.0, 200.0]);
        app.cursor = [103.9, 200.0];
        app.note_drag_motion();
        assert!(!app.drag_moved);
        assert!(
            !app.simulation
                .attention_snapshot(instance)
                .unwrap()
                .dragging
        );
        app.cursor = [104.0, 200.0];
        app.note_drag_motion();
        assert!(app.drag_moved);
        assert!(
            app.simulation
                .attention_snapshot(instance)
                .unwrap()
                .dragging
        );
        app.cursor = [100.0, 200.0];
        app.note_drag_motion();
        assert!(is_click_press(app.press_cursor.unwrap(), app.cursor));
        assert!(
            app.drag_moved,
            "returning to the origin must not turn a drag into a poke"
        );
        app.finish_drag(true);
        let state = app.simulation.attention_snapshot(instance).unwrap();
        assert!(!state.dragging);
        assert!(state.settle_remaining > 0.0 && state.settle_remaining <= 0.2);
        assert!(!app.drag_moved);
        assert!(app.press_cursor.is_none() && app.dragging.is_none());
    }

    #[test]
    fn all_drag_cancellation_paths_clear_feedback_without_a_release_settle() {
        let (mut app, instance, _) = application_with_character();
        app.simulation.suspend_wander(instance).unwrap();
        app.dragging = Some((instance, Vec3::ZERO));
        app.press_cursor = Some([10.0, 20.0]);
        app.cursor = [30.0, 20.0];
        app.note_drag_motion();
        app.end_drag();
        let state = app.simulation.attention_snapshot(instance).unwrap();
        assert!(!state.dragging);
        assert_eq!(state.settle_remaining, 0.0);
        assert_eq!(state.shear_x, 0.0);
        assert_eq!(state.scale_y, 1.0);
        assert!(!app.drag_moved);
        assert!(app.press_cursor.is_none() && app.dragging.is_none());
        app.end_drag();
        assert!(app.fatal.is_none());
    }

    #[test]
    fn finishing_an_unmoved_press_or_removed_character_never_settles() {
        let (mut app, instance, _) = application_with_character();
        app.dragging = Some((instance, Vec3::ZERO));
        app.press_cursor = Some([0.0; 2]);
        app.finish_drag(true);
        assert_eq!(
            app.simulation
                .attention_snapshot(instance)
                .unwrap()
                .settle_remaining,
            0.0
        );
        app.dragging = Some((instance, Vec3::ZERO));
        app.press_cursor = Some([0.0; 2]);
        app.drag_moved = true;
        assert!(app.simulation.despawn(instance));
        app.finish_drag(true);
        assert!(!app.drag_moved);
        assert!(app.press_cursor.is_none() && app.dragging.is_none());
        assert!(app.ui.last_error.is_none());
    }

    #[test]
    fn capture_config_sorts_by_id_and_skips_unpersistable_instances() {
        let mut app = Application::new(Options::default()).unwrap();
        let model = load_model_bytes(
            include_bytes!("../../assets/examples/puppet2d.dmodel.json"),
            &LoadLimits::default(),
        )
        .unwrap();
        let asset = app.simulation.insert_asset(model).unwrap();
        let first = app
            .simulation
            .spawn(asset, Vec3::new(1.0, 0.15, 0.0))
            .unwrap();
        let second = app
            .simulation
            .spawn(asset, Vec3::new(-2.0, 0.15, 0.0))
            .unwrap();
        let probe = app
            .simulation
            .spawn(asset, Vec3::new(3.0, 0.15, 0.0))
            .unwrap();
        app.sources.insert(first, LoadSource::TexturedExample2d);
        app.sources
            .insert(second, LoadSource::File(PathBuf::from("model.dmodel.json")));
        app.sources.insert(probe, LoadSource::ValidationProbe);
        app.ui.pixel_hittest = false;
        let config = app.capture_config();
        assert_eq!(config.version, config::VERSION);
        assert!(!config.pixel_hittest);
        // One entry per persistable instance, ordered by instance id, with
        // the probe and its position left out; the ids are assigned in spawn
        // order, so the earlier spawn leads the list even though the sources
        // map is unordered.
        assert_eq!(config.instances.len(), 2);
        assert_eq!(
            config.instances[0].source,
            config::EntrySource::TexturedExample2d
        );
        assert_eq!(config.instances[0].position, [1.0, 0.15, 0.0]);
        assert_eq!(
            config.instances[1].source,
            config::EntrySource::File {
                path: PathBuf::from("model.dmodel.json")
            }
        );
        assert_eq!(config.instances[1].position, [-2.0, 0.15, 0.0]);
    }

    #[test]
    fn collection_keeps_all_four_example_slots_without_mixing_assets() {
        let mut app = Application::new(Options::default()).unwrap();
        let examples = [
            include_bytes!("../../assets/examples/puppet2d.dmodel.json").as_slice(),
            include_bytes!("../../assets/examples/robot3d.dmodel.json").as_slice(),
            include_bytes!("../../assets/examples/puppet2d-textured.dmodel.json").as_slice(),
            include_bytes!("../../assets/examples/robot3d-textured.dmodel.json").as_slice(),
        ];
        for (index, bytes) in examples.iter().enumerate() {
            let model = load_model_bytes(bytes, &LoadLimits::default()).unwrap();
            app.examples[index] = Some(app.simulation.insert_asset(model).unwrap());
        }
        let slots = app.examples;
        app.collect_assets();
        assert_eq!(app.simulation.asset_count(), 4);
        for (index, asset) in slots.into_iter().enumerate() {
            let model = app.simulation.asset(asset.unwrap()).unwrap();
            assert_eq!(model.images.is_empty(), index < 2);
        }
        let released = app.examples[2].take().unwrap();
        app.collect_assets();
        assert!(app.simulation.asset(released).is_none());
        assert_eq!(app.simulation.asset_count(), 3);
        for index in [0, 1, 3] {
            assert!(app.simulation.asset(app.examples[index].unwrap()).is_some());
        }
    }

    #[test]
    fn bangboo_cache_records_only_committed_bangboo_loads() {
        let mut app = Application::new(Options::default()).unwrap();
        // A failed load must not pollute the cache.
        app.note_bangboo_load(&LoadSource::Bangboo, 7, false);
        assert_eq!(app.bangboo_asset, None);
        // A successful load of any other source is not the pet scene.
        app.note_bangboo_load(&LoadSource::Example2d, 7, true);
        assert_eq!(app.bangboo_asset, None);
        // A committed Bangboo load is remembered exactly once.
        app.note_bangboo_load(&LoadSource::Bangboo, 7, true);
        assert_eq!(app.bangboo_asset, Some(7));
        app.note_bangboo_load(&LoadSource::Bangboo, 8, true);
        assert_eq!(app.bangboo_asset, Some(7));
    }

    #[test]
    fn bangboo_cache_survives_instance_collection_and_never_takes_a_slot() {
        let mut app = Application::new(Options::default()).unwrap();
        let model = load_model_bytes(
            include_bytes!("../../assets/examples/puppet2d.dmodel.json"),
            &LoadLimits::default(),
        )
        .unwrap();
        let asset = app.simulation.insert_asset(model).unwrap();
        let instance = app.simulation.spawn(asset, Vec3::ZERO).unwrap();
        app.bangboo_asset = Some(asset);
        // The cache is kept alive with no surviving instance...
        app.simulation.despawn(instance);
        app.collect_assets();
        assert!(app.simulation.asset(asset).is_some());
        // ...but only while it is still cached: dropping it releases the asset
        // and none of the four example slots ever held it.
        assert!(app.examples.iter().all(|slot| slot.is_none()));
        app.bangboo_asset = None;
        app.collect_assets();
        assert!(app.simulation.asset(asset).is_none());
        assert_eq!(app.simulation.asset_count(), 0);
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

    #[test]
    fn region_cadence_maps_measured_polygon_counts_to_k() {
        assert_eq!(region_cadence(0), 1);
        assert_eq!(region_cadence(648), 1);
        assert_eq!(region_cadence(2047), 1);
        assert_eq!(region_cadence(2048), 2);
        assert_eq!(region_cadence(2616), 2);
        assert_eq!(region_cadence(5232), 2);
        assert_eq!(region_cadence(8191), 2);
        assert_eq!(region_cadence(8192), 4);
        assert_eq!(region_cadence(10512), 4);
    }

    #[test]
    fn install_throttle_defers_until_k_dirty_frames() {
        let mut cache = HitRegionCache {
            mode: RegionMode::Pixel,
            geometry: None,
            click_through: false,
            failed: false,
            dirty_frames: 0,
            force_install: false,
        };
        // K=4 (10512 polygons): three dirty frames defer, the fourth installs.
        assert!(!cache.note_changed(10512));
        assert_eq!(cache.dirty_frames, 1);
        assert!(!cache.note_changed(10512));
        assert!(!cache.note_changed(10512));
        assert!(cache.note_changed(10512));
        assert_eq!(cache.dirty_frames, 3);
        cache.note_installed();
        assert_eq!(cache.dirty_frames, 0);
        // K=2 (2616 polygons): one defer, then install.
        assert!(!cache.note_changed(2616));
        assert!(cache.note_changed(2616));
        // K=1 (648 polygons): every changed frame installs, as before.
        cache.note_installed();
        assert!(cache.note_changed(648));
        cache.note_installed();
        assert!(cache.note_changed(648));
    }

    #[test]
    fn unchanged_frames_reset_the_throttle_counter() {
        let mut cache = HitRegionCache {
            mode: RegionMode::Pixel,
            geometry: None,
            click_through: false,
            failed: false,
            dirty_frames: 0,
            force_install: false,
        };
        assert!(!cache.note_changed(2616));
        assert_eq!(cache.dirty_frames, 1);
        cache.note_unchanged();
        assert_eq!(cache.dirty_frames, 0);
        // The K=2 sequence restarts instead of installing immediately.
        assert!(!cache.note_changed(2616));
        assert!(cache.note_changed(2616));
    }

    #[test]
    fn forced_install_bypasses_the_cadence_once() {
        let mut cache = HitRegionCache {
            mode: RegionMode::Pixel,
            geometry: None,
            click_through: false,
            failed: false,
            dirty_frames: 0,
            force_install: true,
        };
        // K=4 would defer, but a forced pass installs on the first frame.
        assert!(cache.note_changed(10512));
        cache.note_installed();
        assert!(!cache.force_install);
        // The one-shot is consumed: the next dirty frame defers again.
        assert!(!cache.note_changed(10512));
    }

    #[test]
    fn drag_restore_forces_the_next_pixel_install() {
        let (mut app, _, _) = application_with_character();
        app.drag_region = Some(RegionMode::Pixel);
        app.hit_region.dirty_frames = 2;
        app.restore_region_after_drag();
        assert_eq!(app.hit_region.mode, RegionMode::Pixel);
        assert!(app.hit_region.geometry.is_none());
        assert_eq!(app.hit_region.dirty_frames, 0);
        assert!(app.hit_region.force_install);
    }
}
