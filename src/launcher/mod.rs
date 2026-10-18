//! The launcher: a stable entry point that keeps the engine up to date and
//! runs it.
//!
//! Delicium ships two executables. `delicium-launcher.exe` sits at a fixed
//! path and is the only thing a shortcut points at; `delicium.exe` lives in
//! `app\<version>\` and is replaced by installing a new version directory,
//! never by overwriting a running image. The launcher checks for a newer
//! release at startup, installs it when one exists, and then supervises the
//! engine process.
//!
//! Requirements this module is built around:
//!
//! - A double click goes straight to the pet. The update check is silent and
//!   its failure never blocks the launch.
//! - The launcher and the engine are both updated by the same release
//!   mechanism. The launcher replaces its own running image with
//!   `ReplaceFileW` (the one Win32 call that can, measured on this machine);
//!   the engine is installed into a new version directory.
//! - Nothing is executed straight out of the staging directory, and every
//!   byte is verified before it becomes part of the installation.
//!
//! The settings page is a native window with native controls, so the
//! launcher needs no GPU, no renderer and no second copy of the UI stack.
//!
//! Threads: [`run`] owns the process's only message loop (the message-only
//! window plus the optional settings window), and a supervisor thread owns
//! the engine process and runs update work. They share [`Shared`] and the
//! worker wakes the UI thread with [`contract::WM_WAKE`].

pub mod cli;
pub mod contract;
pub mod logging;
pub mod paths;
pub mod supervisor;
pub mod update;

#[cfg(windows)]
pub mod instance;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod run;
#[cfg(windows)]
pub mod settings;
#[cfg(windows)]
pub mod win32;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::update::{InstallState, InstalledVersion};

/// Unix seconds, the clock every stored timestamp uses.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Formats a Unix timestamp for a log line or the settings page, in UTC.
pub fn format_time(unix: u64) -> String {
    if unix == 0 {
        return "never".to_owned();
    }
    let (year, month, day) = logging::civil_date(unix);
    let seconds = unix % 86_400;
    let (hour, minute) = (seconds / 3_600, (seconds % 3_600) / 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

/// What one update check produced, as the UI shows it.
#[derive(Clone, Debug)]
pub struct CheckReport {
    /// Unix seconds the check finished.
    pub at: u64,
    /// The message for the status line, success or failure.
    pub message: String,
    /// The failure, when the check failed; kept separately so the settings
    /// page can show it in its own line.
    pub error: Option<String>,
    /// An engine version was installed; the next start uses it.
    pub installed: bool,
}

/// Whether an update check is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckPhase {
    Idle,
    Running,
}

/// A window handle that may travel to the supervisor thread.
///
/// `HWND` is a raw pointer, which Rust declares neither `Send` nor `Sync`.
/// The launcher only ever posts to the handle from the supervisor thread
/// (`PostMessageW` is documented as callable from any thread for any window),
/// and never dereferences it, so carrying it across threads is sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiWindow(pub *mut core::ffi::c_void);

// SAFETY: see the note above; the value is an opaque handle and the only
// operation performed on it off-thread is `PostMessageW`.
unsafe impl Send for UiWindow {}
// SAFETY: as above.
unsafe impl Sync for UiWindow {}

/// State the UI thread and the supervisor thread share.
///
/// Every field is plain data guarded by one mutex: the critical sections are
/// a few string clones, and the alternative (several atomics plus message
/// passing) would be harder to keep consistent. The engine process itself
/// never leaves the supervisor thread; so does the network, which is why no
/// fetcher is stored here.
pub struct Shared {
    root: PathBuf,
    launcher: Option<PathBuf>,
    state: Mutex<InstallState>,
    /// The engine this installation should run; the supervisor replaces it
    /// when an update installs a newer version.
    target: Mutex<Option<InstalledVersion>>,
    /// `--no-update`: never touch the network.
    no_update: bool,
    /// Set by the UI thread when it wants the supervisor to stop.
    quit: AtomicBool,
    /// The engine pid while one runs.
    engine_pid: AtomicU32,
    /// Whether the supervised engine came from `state.pending`, so three
    /// early crashes discard it and fall back.
    pending_start: AtomicBool,
    /// The settings page asked for a check; the supervisor clears it.
    check_requested: AtomicBool,
    /// The settings page asked to install and restart the pet.
    restart_requested: AtomicBool,
    phase: Mutex<CheckPhase>,
    last: Mutex<Option<CheckReport>>,
    /// The failure that ended the supervisor, reported after the loop exits.
    fatal: Mutex<Option<String>>,
    /// The message-only window, posted to from the supervisor.
    ui_window: Mutex<Option<UiWindow>>,
}

impl Shared {
    /// The shared state of one launcher run.
    pub fn new(
        root: PathBuf,
        launcher: Option<PathBuf>,
        state: InstallState,
        target: Option<InstalledVersion>,
        no_update: bool,
    ) -> Self {
        let pending_start = state.pending.is_some();
        Self {
            root,
            launcher,
            state: Mutex::new(state),
            target: Mutex::new(target),
            no_update,
            quit: AtomicBool::new(false),
            engine_pid: AtomicU32::new(0),
            pending_start: AtomicBool::new(pending_start),
            check_requested: AtomicBool::new(false),
            restart_requested: AtomicBool::new(false),
            phase: Mutex::new(CheckPhase::Idle),
            last: Mutex::new(None),
            fatal: Mutex::new(None),
            ui_window: Mutex::new(None),
        }
    }

    /// The state as it is now; a poisoned lock is impossible here because
    /// every critical section only touches plain data, but it must not turn
    /// into a panic either.
    pub fn state(&self) -> MutexGuard<'_, InstallState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn target(&self) -> Option<InstalledVersion> {
        self.target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_target(&self, target: Option<InstalledVersion>) {
        *self.target.lock().unwrap_or_else(PoisonError::into_inner) = target;
    }

    pub fn phase(&self) -> CheckPhase {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_phase(&self, phase: CheckPhase) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = phase;
    }

    pub fn last_report(&self) -> Option<CheckReport> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_report(&self, report: CheckReport) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(report);
    }

    /// Records the failure that ended the supervisor.
    pub fn set_fatal(&self, message: String) {
        *self.fatal.lock().unwrap_or_else(PoisonError::into_inner) = Some(message);
    }

    /// The failure that ended the supervisor, if there was one.
    pub fn fatal(&self) -> Option<String> {
        self.fatal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn launcher(&self) -> Option<&Path> {
        self.launcher.as_deref()
    }

    pub fn no_update(&self) -> bool {
        self.no_update
    }

    pub fn engine_pid(&self) -> u32 {
        self.engine_pid.load(Ordering::SeqCst)
    }

    pub fn set_engine_pid(&self, pid: u32) {
        self.engine_pid.store(pid, Ordering::SeqCst);
    }

    pub fn pending_start(&self) -> bool {
        self.pending_start.load(Ordering::SeqCst)
    }

    pub fn set_pending_start(&self, pending: bool) {
        self.pending_start.store(pending, Ordering::SeqCst);
    }

    pub fn quit_requested(&self) -> bool {
        self.quit.load(Ordering::SeqCst)
    }

    pub fn request_quit(&self) {
        self.quit.store(true, Ordering::SeqCst);
    }

    /// The settings page asked for an update check.
    pub fn request_check(&self, restart_after: bool) {
        self.check_requested.store(true, Ordering::SeqCst);
        self.restart_requested
            .store(restart_after, Ordering::SeqCst);
    }

    /// Reads and clears the pending check request; returns whether the pet
    /// should restart when a new version was installed.
    pub fn take_check_request(&self) -> Option<bool> {
        if !self.check_requested.swap(false, Ordering::SeqCst) {
            return None;
        }
        Some(self.restart_requested.swap(false, Ordering::SeqCst))
    }

    /// The message-only window, once created.
    pub fn ui_window(&self) -> Option<UiWindow> {
        *self
            .ui_window
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_ui_window(&self, window: Option<UiWindow>) {
        *self
            .ui_window
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = window;
    }

    /// Posts `message` to the UI thread, if it has a window yet.
    #[cfg(windows)]
    pub fn post(&self, message: u32) {
        let Some(window) = self.ui_window() else {
            return;
        };
        // SAFETY: `window` is a live window of this process (cleared when it
        // is destroyed); posting only enqueues a message.
        if unsafe { win32::PostMessageW(window.0, message, 0, 0) } == 0 {
            // SAFETY: the post just failed and set the thread error slot.
            let error = unsafe { win32::GetLastError() };
            log::warn!("PostMessageW({message}) failed with error {error}");
        }
    }
}

/// Runs the launcher and returns the process exit code.
///
/// A clean engine exit ends the launcher with code 0; every failure that
/// prevented the pet from starting is reported and ends with a non-zero code.
#[cfg(windows)]
pub fn run() -> Result<(), String> {
    run::run()
}

#[cfg(not(windows))]
pub fn run() -> Result<(), String> {
    Err("The Delicium launcher runs on Windows only".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_print_as_utc() {
        assert_eq!(format_time(0), "never");
        assert_eq!(format_time(1_792_281_600), "2026-10-18 00:00 UTC");
        assert_eq!(format_time(1_792_281_600 + 3_661), "2026-10-18 01:01 UTC");
    }

    #[test]
    fn reports_carry_their_own_error() {
        let shared = Shared::new(
            std::path::PathBuf::from(r"C:\x"),
            None,
            InstallState::default(),
            None,
            true,
        );
        assert!(shared.last_report().is_none());
        shared.set_report(CheckReport {
            at: 7,
            message: "up to date (0.1.0).".into(),
            error: None,
            installed: false,
        });
        assert_eq!(shared.last_report().unwrap().at, 7);
        assert_eq!(shared.phase(), CheckPhase::Idle);
        assert!(shared.no_update());
        assert!(shared.target().is_none());
        assert!(!shared.pending_start());
        // A check request is taken exactly once, carrying the restart flag.
        assert_eq!(shared.take_check_request(), None);
        shared.request_check(true);
        assert_eq!(shared.take_check_request(), Some(true));
        assert_eq!(shared.take_check_request(), None);
    }
}
