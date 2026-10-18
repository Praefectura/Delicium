//! Names and messages the launcher and the engine agree on.
//!
//! The engine's tray menu offers `Settings...` only when the launcher started
//! it, and the launcher exposes a message-only window with a fixed class and
//! title so the engine can find it without a shared handle. Both sides read
//! the same constants here, so the contract cannot drift.

/// Class of the launcher's message-only window.
pub const WINDOW_CLASS: &str = "DeliciumLauncherWindowClass";

/// Title of that window; `FindWindowExW` matches it exactly.
pub const WINDOW_TITLE: &str = "Delicium Launcher";

/// `WM_APP`; the first message id applications may use freely.
const WM_APP: u32 = 0x8000;

/// Posted to the launcher window when the user picks `Settings...` in the
/// engine's tray menu.
pub const WM_OPEN_SETTINGS: u32 = WM_APP + 1;

/// Posted to the launcher window by the update worker when its run ended.
pub const WM_UPDATE_READY: u32 = WM_APP + 2;

/// Posted by a second launcher asking the first one to act.
pub const WM_WAKE: u32 = WM_APP + 3;

/// `wparam` of [`WM_WAKE`]: open the settings page.
pub const WAKE_SETTINGS: usize = 1;

/// Set by the launcher in the engine's environment to its own path. The
/// engine shows the tray `Settings...` entry only when this is present.
pub const ENV_LAUNCHER_EXE: &str = "DELICIUM_LAUNCHER_EXE";
