//! Running the launcher: the UI thread's message loop and the supervisor
//! thread that owns the engine process and the network.
//!
//! One process, two threads, one direction of control: the supervisor owns
//! [`process::Engine`] and the [`WinHttpFetcher`] (both are not `Sync`), the
//! UI thread owns every window. They meet in [`Shared`]; the supervisor wakes
//! the UI thread by posting a message to the message-only window.
//!
//! Why the split: a window procedure may not block, and a network check takes
//! seconds. Keeping the message loop free means the settings page stays
//! responsive while an update downloads, and the pet can be closed at any
//! time.
//!
//! Startup, in order:
//!
//! 1. Parse the command line, resolve the installation root, open the log.
//! 2. Claim the single-instance mutex. A second launcher wakes the first and
//!    exits without starting anything.
//! 3. Load (or recover) the installation state, create the message-only
//!    window, open the settings page if `--settings` asked for it.
//! 4. Start the supervisor. The pet starts first; the update check runs
//!    behind it, unless no engine is installed yet.
//!
//! Exit: a clean engine exit ends the launcher with code 0. A failure that
//! kept the pet from starting is returned to the caller, which reports it.

use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::cli::{self, ParseResult};
use super::supervisor::{Decision, Supervisor};
use super::win32::{self, Hwnd, Lparam, Lresult, Msg, Point, Wparam};
use super::{
    CheckPhase, CheckReport, Shared, contract, instance, logging, paths, process, settings,
    unix_now, update,
};
use crate::update::{InstallState, Installer, WinHttpFetcher};

/// How often the supervisor looks at the engine while it runs.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The shared state the window procedure acts on. Set once, before the
/// message loop starts.
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

/// Runs the launcher and returns the process exit code.
pub fn run() -> Result<(), String> {
    let options = match cli::parse_args(std::env::args_os().skip(1))? {
        ParseResult::Help => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        ParseResult::Version => {
            let version = crate::update::current_version()
                .map(|version| version.to_string())
                .unwrap_or_else(|_| "unknown".to_owned());
            println!("delicium-launcher {version}");
            return Ok(());
        }
        ParseResult::Run(options) => options,
    };
    let root = paths::install_root()?;
    paths::ensure_layout(&root)?;
    logging::init(
        &root,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "info"
        },
    )?;
    log::info!("launcher starting; root {}", root.display());

    // A release is a zip: on the first launch the launcher still lives in the
    // folder it was extracted to. Self update replaces
    // `<root>\delicium-launcher.exe`, so the image has to be there first.
    match update::adopt_self(&root) {
        Ok(true) => log::info!("running image copied into the installation"),
        Ok(false) => {}
        // The launcher keeps running from wherever it was; only a release
        // that requires a newer launcher than this build is affected.
        Err(error) => log::warn!("cannot adopt the running image: {error}"),
    }

    // The settings page is laid out in logical units; per-monitor awareness
    // keeps it crisp. A failure only means something else already set it.
    // SAFETY: the call only touches this process's DPI awareness.
    unsafe {
        win32::SetProcessDpiAwarenessContext(win32::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let Some(_guard) = instance::claim(&paths::install_key(&root))? else {
        log::info!("another launcher owns this installation; handing over");
        if options.settings {
            instance::wake_primary(true)?;
        }
        return Ok(());
    };

    let state = load_state(&root);
    let installer = Installer::new(&root);
    let target = update::target_version(&installer, &state);
    let shared = Arc::new(Shared::new(
        root.clone(),
        Some(paths::launcher_exe(&root)),
        state,
        target,
        options.no_update,
    ));

    let window = instance::create_message_window(window_proc)?;
    shared.set_ui_window(Some(super::UiWindow(window)));
    let _ = SHARED.set(Arc::clone(&shared));

    if options.settings
        && let Err(error) = settings::show(&shared, settings::Mode::CloseQuits)
    {
        // A page that cannot open is a failure the user must see, and without
        // it there is nothing to keep running.
        shared.set_ui_window(None);
        // SAFETY: `window` came from `create_message_window` just above.
        unsafe {
            instance::destroy_message_window(window);
        }
        return Err(error);
    }

    let worker = {
        let shared = Arc::clone(&shared);
        let options = options.clone();
        std::thread::Builder::new()
            .name("supervisor".to_owned())
            .spawn(move || {
                // Whatever ends this thread, the UI loop must end too, or the
                // launcher would linger without a pet or a page.
                struct QuitOnDrop(Arc<Shared>);
                impl Drop for QuitOnDrop {
                    fn drop(&mut self) {
                        self.0.post(win32::WM_QUIT);
                    }
                }
                let _quit = QuitOnDrop(Arc::clone(&shared));
                supervise(shared, options);
            })
            .map_err(|error| format!("Cannot start the supervisor thread: {error}"))?
    };

    message_loop();

    // The loop is done; tear down in the order the supervisor expects: it may
    // still be waiting on the engine, so it is told to stop and joined before
    // the window handles disappear.
    shared.request_quit();
    let _ = worker.join();
    settings::close();
    shared.set_ui_window(None);
    // SAFETY: `window` came from `create_message_window` and the message loop
    // has ended, so the last window of the class is gone.
    unsafe {
        instance::destroy_message_window(window);
    }
    drop(_guard);

    log::info!("launcher exiting");
    match shared.fatal() {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

/// The installation state, recovered from disk when the file is missing or
/// unusable.
fn load_state(root: &Path) -> InstallState {
    match InstallState::load(root) {
        Ok(Some(state)) => match state.validate() {
            Ok(()) => return state,
            Err(error) => log::warn!("ignoring the installation state: {error}"),
        },
        Ok(None) => {}
        Err(error) => log::warn!("cannot read the installation state: {error}"),
    }
    let installed = Installer::new(root).scan_installed();
    if !installed.is_empty() {
        log::info!(
            "recovered {} installed version(s) from the app directory",
            installed.len()
        );
    }
    InstallState::from_installed(installed, unix_now())
}

/// The UI thread's message loop, until `WM_QUIT`.
fn message_loop() {
    let mut message = Msg {
        hwnd: std::ptr::null_mut(),
        message: 0,
        wparam: 0,
        lparam: 0,
        time: 0,
        point: Point::default(),
        private: 0,
    };
    loop {
        // SAFETY: `message` is a live buffer; a null window asks for every
        // message of this thread.
        let result = unsafe { win32::GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        if result == 0 {
            break;
        }
        if result == -1 {
            // SAFETY: `GetMessageW` just failed and set the error slot.
            let error = unsafe { win32::GetLastError() };
            log::error!("GetMessageW failed with error {error}");
            break;
        }
        // SAFETY: the message is the one just received.
        unsafe {
            win32::TranslateMessage(&message);
            win32::DispatchMessageW(&message);
        }
    }
}

/// The message-only window's procedure: the only place messages from the
/// supervisor and from the engine are handled.
unsafe extern "system" fn window_proc(
    window: Hwnd,
    message: u32,
    wparam: Wparam,
    lparam: Lparam,
) -> Lresult {
    match message {
        contract::WM_OPEN_SETTINGS => {
            show_settings();
            0
        }
        contract::WM_WAKE => {
            // Only the settings wake carries an action; a bare wake exists so
            // a second launcher can prove it reached the first.
            if wparam == contract::WAKE_SETTINGS {
                show_settings();
            }
            0
        }
        contract::WM_UPDATE_READY => {
            // Runs on the UI thread, which is what the controls require.
            settings::refresh();
            0
        }
        win32::WM_QUIT => {
            // Posted by the supervisor, which cannot call `PostQuitMessage`
            // for this thread.
            // SAFETY: ends this thread's loop.
            unsafe {
                win32::PostQuitMessage(0);
            }
            0
        }
        win32::WM_DESTROY => {
            // SAFETY: ends this thread's loop.
            unsafe {
                win32::PostQuitMessage(0);
            }
            0
        }
        _ => {
            // SAFETY: a plain forward of a message this window does not handle.
            unsafe { win32::DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}

/// Opens or raises the settings page, leaving the pet running.
fn show_settings() {
    let Some(shared) = SHARED.get() else {
        return;
    };
    if let Err(error) = settings::show(shared, settings::Mode::Hide) {
        log::error!("cannot open the settings page: {error}");
    }
}

/// The supervisor thread: update checks, the engine's lifetime, restart
/// policy.
fn supervise(shared: Arc<Shared>, options: cli::Options) {
    let fetcher = WinHttpFetcher::new();
    let mut engine: Option<process::Engine> = None;
    let mut policy = Supervisor::new(shared.pending_start());
    let mut next_start = Instant::now();
    let mut first_check = !shared.no_update();
    // `--settings` opens the page and starts nothing; the page then closes
    // the launcher. Installing and restarting from that page is a request to
    // run the pet, which turns the launcher into an ordinary one.
    let mut standalone = options.settings;

    // Without an installed engine the update check is the only way to get
    // one, so it cannot run behind a pet that does not exist.
    if !standalone && needs_installed_engine(&options, &shared) && first_check {
        first_check = false;
        run_check(&shared, &fetcher);
    }

    loop {
        if shared.quit_requested() {
            break;
        }

        if let Some(restart_after) = shared.take_check_request() {
            let installed = run_check(&shared, &fetcher);
            if restart_after && installed {
                if let Some(mut running) = engine.take() {
                    // Stop the old pet; the new version starts in its place.
                    running.terminate();
                }
                if standalone {
                    standalone = false;
                    settings::set_mode(settings::Mode::Hide);
                }
                policy.begin(shared.pending_start(), Instant::now());
                next_start = Instant::now();
            }
        } else if first_check && (engine.is_some() || standalone) {
            // The pet is up; running the check here keeps the normal startup
            // silent instead of holding the window back behind the network.
            first_check = false;
            run_check(&shared, &fetcher);
        }

        if engine.is_none() {
            if standalone {
                // The page is open and starts nothing; wait for a quit
                // request or a manual check.
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let now = Instant::now();
            if now < next_start {
                std::thread::sleep((next_start - now).min(POLL_INTERVAL));
                continue;
            }
            match start_engine(&shared, &options) {
                Ok(started) => {
                    shared.set_engine_pid(started.pid());
                    log::info!("engine running from {}", started.executable().display());
                    policy.begin(shared.pending_start(), Instant::now());
                    shared.post(contract::WM_UPDATE_READY);
                    engine = Some(started);
                }
                Err(error) => {
                    log::error!("cannot start the engine: {error}");
                    shared.set_fatal(error);
                    break;
                }
            }
        }

        let Some(running) = engine.as_ref() else {
            continue;
        };
        let Some(handle) = running.wait_handle() else {
            continue;
        };
        // SAFETY: the handle is a live process handle owned by `engine`.
        unsafe {
            win32::WaitForSingleObject(handle, POLL_INTERVAL.as_millis() as u32);
        }

        // A run that stayed up long enough is a working version: commit it
        // while it is still alive, so a later crash cannot undo the install.
        if policy.confirmed(Instant::now()) {
            confirm_healthy(&shared);
        }

        let Some(code) = engine.as_mut().and_then(process::Engine::poll) else {
            continue;
        };
        engine = None;
        shared.set_engine_pid(0);
        let now = Instant::now();
        match policy.on_exit(code, now) {
            Decision::Quit => {
                log::info!("the pet closed; the launcher exits");
                break;
            }
            Decision::Restart {
                after,
                discard_pending,
            } => {
                if discard_pending {
                    discard_and_roll_back(&shared);
                }
                log::info!("restarting the engine in {} ms", after.as_millis());
                next_start = now + after;
            }
            Decision::GiveUp(message) => {
                log::error!("{message}");
                shared.set_fatal(message);
                break;
            }
        }
    }

    // A still-running pet is ended when `engine` drops here; `Engine::drop`
    // is the one place that guarantee lives.
}

/// Whether the engine must be fetched before it can start: `--engine` names
/// its own binary, so only the installed path depends on the update check.
fn needs_installed_engine(options: &cli::Options, shared: &Shared) -> bool {
    options.engine.is_none() && shared.target().is_none()
}

/// The engine's command line: the pet scene unless the caller was explicit.
fn engine_arguments(options: &cli::Options) -> Vec<OsString> {
    if options.engine_args.is_empty() {
        // The product is the pet: a double click starts the bundled Bangboo
        // scene, not the developer's example scene.
        vec![OsString::from("--bangboo")]
    } else {
        options.engine_args.clone()
    }
}

/// Starts the engine: the `--engine` override, or the installed target.
fn start_engine(shared: &Arc<Shared>, options: &cli::Options) -> Result<process::Engine, String> {
    let executable = match &options.engine {
        Some(path) => path.clone(),
        None => {
            let target = shared.target().ok_or_else(|| {
                "No engine version is installed. Use Check for updates in the settings page."
                    .to_owned()
            })?;
            update::engine_path(shared.root(), &target)
        }
    };
    if !executable.is_file() {
        return Err(format!(
            "The engine binary is missing: {}",
            executable.display()
        ));
    }
    let launcher = shared
        .launcher()
        .map(Path::to_path_buf)
        .ok_or_else(|| "The launcher path is unknown".to_owned())?;
    process::Engine::spawn(&executable, &engine_arguments(options), &launcher)
}

/// Runs one update check and records everything the page shows.
///
/// Returns whether an engine version was installed, which is what the
/// `Install and restart` button acts on.
fn run_check(shared: &Arc<Shared>, fetcher: &WinHttpFetcher) -> bool {
    log::info!("checking for updates");
    shared.set_phase(CheckPhase::Running);
    shared.post(contract::WM_UPDATE_READY);
    let snapshot = shared.state().clone();
    let result = update::check_and_install(shared.root(), &snapshot, fetcher);
    let now = unix_now();
    let mut installed_version = false;
    let report = match result {
        Ok(outcome) => {
            if let update::Outcome::EngineInstalled { installed, .. } = &outcome {
                let mut state = shared.state();
                let recorded = state
                    .record_pending(installed.clone())
                    .and_then(|()| state.save(shared.root(), now));
                match recorded {
                    Ok(()) => {
                        shared.set_target(Some(installed.clone()));
                        shared.set_pending_start(true);
                        installed_version = true;
                    }
                    Err(error) => {
                        // The files are installed but not recorded as pending;
                        // the next start still finds them by directory scan.
                        log::error!("cannot record the installed version: {error}");
                    }
                }
            }
            CheckReport {
                at: now,
                message: outcome.describe(),
                error: None,
                installed: installed_version,
            }
        }
        Err(error) => CheckReport {
            at: now,
            message: format!("Update check failed: {error}"),
            error: Some(error),
            installed: false,
        },
    };
    log::info!("update check finished: {}", report.message);
    shared.set_report(report);
    shared.set_phase(CheckPhase::Idle);
    shared.post(contract::WM_UPDATE_READY);
    installed_version
}

/// Commits a pending version once its first run proved healthy.
fn confirm_healthy(shared: &Arc<Shared>) {
    let mut state = shared.state();
    if state.pending.is_none() {
        return;
    }
    let now = unix_now();
    match state.confirm(now) {
        Ok(confirmed) => log::info!("engine start confirmed: {confirmed:?}"),
        Err(error) => {
            log::warn!("cannot confirm the running version: {error}");
            return;
        }
    }
    if let Err(error) = state.save(shared.root(), now) {
        log::error!("cannot save the installation state: {error}");
    }
    shared.set_pending_start(false);
    shared.post(contract::WM_UPDATE_READY);
}

/// Drops a pending version that failed three times and returns to the
/// version that worked before it.
fn discard_and_roll_back(shared: &Arc<Shared>) {
    let now = unix_now();
    let mut state = shared.state();
    if let Some(failed) = state.discard_pending() {
        log::warn!("discarding {} after repeated early crashes", failed.version);
    }
    if let Err(error) = state.roll_back(now) {
        // Nothing to fall back to: the current version stays and the caller
        // reports the failure.
        log::warn!("cannot roll back: {error}");
    }
    if let Err(error) = state.save(shared.root(), now) {
        log::error!("cannot save the installation state: {error}");
    }
    shared.set_pending_start(false);
    let installer = Installer::new(shared.root());
    shared.set_target(update::target_version(&installer, &state));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_launch_starts_the_pet_scene() {
        let options = cli::Options::default();
        assert_eq!(
            engine_arguments(&options),
            vec![OsString::from("--bangboo")]
        );
    }

    #[test]
    fn explicit_engine_arguments_are_forwarded_unchanged() {
        let options = cli::Options {
            engine_args: vec![OsString::from("--smoke-test"), OsString::from("--frames")],
            ..cli::Options::default()
        };
        assert_eq!(
            engine_arguments(&options),
            vec![OsString::from("--smoke-test"), OsString::from("--frames")]
        );
    }

    #[test]
    fn the_engine_override_replaces_the_installed_target() {
        let shared = Shared::new(
            std::path::PathBuf::from(r"C:\x"),
            None,
            InstallState::default(),
            None,
            true,
        );
        assert!(needs_installed_engine(&cli::Options::default(), &shared));
        let options = cli::Options {
            engine: Some(std::path::PathBuf::from(r"C:\x\delicium.exe")),
            ..cli::Options::default()
        };
        assert!(!needs_installed_engine(&options, &shared));
    }
}
