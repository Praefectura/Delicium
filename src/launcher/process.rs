//! Starting and watching the engine process.
//!
//! The engine is a plain child process: the launcher keeps its handle and
//! waits on it in the message loop, so user input, the settings window and
//! the engine's exit are all handled by one thread. The launcher also sets
//! [`ENV_LAUNCHER_EXE`] so the engine knows a settings page exists.

use std::ffi::OsString;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use super::win32;

/// Keeps a console window from appearing when the launcher starts the
/// console-subsystem engine. Ignored for GUI-subsystem executables.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A running engine.
pub struct Engine {
    child: Child,
    pid: u32,
    /// The binary that was started, for the log and the settings page.
    executable: PathBuf,
    /// The raw process handle to wait on; cleared once the process ended.
    raw: Option<*mut core::ffi::c_void>,
}

impl Engine {
    pub fn spawn(executable: &Path, args: &[OsString], launcher: &Path) -> Result<Self, String> {
        let mut command = Command::new(executable);
        command.args(args);
        command.env(super::contract::ENV_LAUNCHER_EXE, launcher);
        command.creation_flags(CREATE_NO_WINDOW);
        let child = command
            .spawn()
            .map_err(|error| format!("Cannot start {}: {error}", executable.display()))?;
        let pid = child.id();
        // SAFETY: the handle belongs to `child` and stays valid until the
        // child is reaped, which only happens after `raw` is cleared.
        let raw = Some(child.as_raw_handle());
        log::info!("engine started pid={pid} exe={}", executable.display());
        Ok(Self {
            child,
            pid,
            executable: executable.to_path_buf(),
            raw,
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The binary this engine was started from.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The process handle to wait on, while the process is running.
    pub fn wait_handle(&self) -> Option<*mut core::ffi::c_void> {
        self.raw
    }

    /// The exit code, once the process has ended.
    pub fn poll(&mut self) -> Option<i32> {
        self.raw?;
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.raw = None;
                let code = status.code().unwrap_or(-1);
                log::info!("engine exited pid={} code={code}", self.pid);
                Some(code)
            }
            Ok(None) => None,
            Err(error) => {
                log::warn!("Cannot read the engine status: {error}");
                self.raw = None;
                Some(-1)
            }
        }
    }

    /// Ends the process immediately.
    pub fn terminate(&mut self) {
        if self.raw.is_some() {
            log::warn!("terminating engine pid={}", self.pid);
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.raw = None;
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // A launcher that goes away must not leave an orphaned pet behind.
        self.terminate();
    }
}

/// Whether a process with this id is still running.
pub fn alive(pid: u32) -> bool {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_TIMEOUT: u32 = 258;
    if pid == 0 {
        return false;
    }
    // SAFETY: `OpenProcess` only reads the process table; the handle is
    // closed immediately below.
    let handle = unsafe { win32::OpenProcess(SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return false;
    }
    // SAFETY: `handle` is a live process handle closed exactly once, below.
    let state = unsafe { win32::WaitForSingleObject(handle, 0) };
    // SAFETY: the handle is still owned here.
    unsafe {
        win32::CloseHandle(handle);
    }
    state == WAIT_TIMEOUT
}
