//! The settings page: one native window with native controls.
//!
//! The launcher deliberately does not link the renderer: a settings page that
//! needs a GPU context would fail on exactly the machines where the pet
//! cannot start, and it would pull the whole egui stack into the stable entry
//! point that must keep working while the engine is broken. Native controls
//! need nothing but `user32` and `comctl32`'s built-in button classes.
//!
//! The page edits one engine setting, `pixel_hittest`, by rewriting
//! `%APPDATA%\Delicium\config.json` in place: the engine watches that file's
//! modification time and applies the change to the running pet, so no IPC
//! channel is needed. Everything else in the file (the saved instances) is
//! carried through untouched.
//!
//! All page text is ASCII: the launcher never loads a font that has CJK
//! glyphs, and `epaint`-style bundled fonts are not available here at all.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use super::win32::{self, Hfont, Hwnd, Lparam, Lresult, Wparam};
use super::{CheckPhase, Shared, format_time, paths};

/// Class of the settings window.
const WINDOW_CLASS: &str = "DeliciumSettingsWindowClass";

/// Title of the settings window.
const WINDOW_TITLE: &str = "Delicium Settings";

/// Control ids. `WM_COMMAND` carries them in the low word of `wparam`.
const ID_HITTEST: i32 = 1001;
const ID_CHECK: i32 = 1002;
const ID_INSTALL: i32 = 1003;
const ID_LOGS: i32 = 1004;
const ID_QUIT: i32 = 1005;
const ID_CLOSE: i32 = 1006;
const ID_VERSION: i32 = 1010;
const ID_ENGINE: i32 = 1011;
const ID_LAST: i32 = 1012;
const ID_STATUS: i32 = 1013;
const ID_ERROR: i32 = 1014;

/// The configuration this page writes is the engine's; only its format
/// version is repeated here, and only the one field the page owns.
const CONFIG_VERSION: u64 = 1;

/// What closing the page does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Hide it; the pet keeps running (the tray opens it this way).
    Hide,
    /// Destroy it and end the launcher (`--settings` with no pet).
    CloseQuits,
}

impl Mode {
    fn encode(self) -> usize {
        match self {
            Self::Hide => 0,
            Self::CloseQuits => 1,
        }
    }
}

/// The window, once created; `0` while there is none.
static WINDOW: AtomicUsize = AtomicUsize::new(0);
/// The control font, deleted with the window.
static FONT: AtomicUsize = AtomicUsize::new(0);
/// Whether the class is registered.
static CLASS: AtomicBool = AtomicBool::new(false);
/// What closing the page does.
static MODE: AtomicUsize = AtomicUsize::new(0);
/// The shared state the window's commands act on.
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

/// Shows the settings page, creating it on first use.
pub fn show(shared: &Arc<Shared>, mode: Mode) -> Result<(), String> {
    let _ = SHARED.set(Arc::clone(shared));
    let existing = WINDOW.load(Ordering::SeqCst);
    if existing != 0 {
        let window = existing as Hwnd;
        refresh();
        // SAFETY: `window` is this process's live settings window; the calls
        // only change its z-order and visibility.
        unsafe {
            win32::ShowWindow(window, win32::SW_SHOWNORMAL);
            win32::BringWindowToTop(window);
            win32::SetForegroundWindow(window);
        }
        return Ok(());
    }
    create(mode)
}

/// Re-reads the shared state into the page's labels. A no-op while the page
/// does not exist.
pub fn refresh() {
    let Some(shared) = SHARED.get() else {
        return;
    };
    let window = WINDOW.load(Ordering::SeqCst);
    if window == 0 {
        return;
    }
    // SAFETY: called on the UI thread with a live window of this process.
    unsafe {
        refresh_labels(window as Hwnd, shared);
    }
}

/// A modal error box, owned by the page when it is open.
pub fn show_error(message: &str) {
    let owner = WINDOW.load(Ordering::SeqCst) as Hwnd;
    let text = win32::wide(message);
    let caption = win32::wide("Delicium");
    // SAFETY: both strings are NUL-terminated and live for the call; the
    // owner is null or this process's live window.
    unsafe {
        win32::MessageBoxW(
            owner,
            text.as_ptr(),
            caption.as_ptr(),
            win32::MB_OK | win32::MB_ICONERROR | win32::MB_SETFOREGROUND,
        );
    }
}

/// Creates the window and its controls.
fn create(mode: Mode) -> Result<(), String> {
    MODE.store(mode.encode(), Ordering::SeqCst);
    let instance = win32::module();
    if instance.is_null() {
        return Err("the process module handle is unavailable".to_owned());
    }
    let class_name = win32::wide(WINDOW_CLASS);
    if !CLASS.swap(true, Ordering::SeqCst) {
        let class = win32::WndClassExW {
            cb_size: core::mem::size_of::<win32::WndClassExW>() as u32,
            style: 0,
            lpfn_wnd_proc: Some(window_proc),
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: instance,
            h_icon: std::ptr::null_mut(),
            // SAFETY: `IDC_ARROW` is a system resource id, not a pointer.
            h_cursor: unsafe { win32::LoadCursorW(std::ptr::null_mut(), win32::IDC_ARROW) },
            // SAFETY: `COLOR_BTNFACE` is a documented system colour index.
            hbr_background: unsafe { win32::GetSysColorBrush(win32::COLOR_BTNFACE) },
            lpsz_menu_name: std::ptr::null(),
            lpsz_class_name: class_name.as_ptr(),
            h_icon_sm: std::ptr::null_mut(),
        };
        // SAFETY: the class is fully initialised and its name outlives the
        // call; the procedure matches the declared signature.
        if unsafe { win32::RegisterClassExW(&class) } == 0 {
            CLASS.store(false, Ordering::SeqCst);
            // SAFETY: the registration attempt just failed.
            let error = unsafe { win32::GetLastError() };
            return Err(format!("RegisterClassExW failed with error {error}"));
        }
    }
    // The page is laid out in logical units and scaled once at creation; a
    // DPI change while it is open leaves it at the old scale, which is
    // acceptable for a page this small.
    // SAFETY: both calls only read a system metric.
    let dpi = unsafe { win32::GetDpiForSystem() }.max(96);
    let style = win32::WS_OVERLAPPED | win32::WS_CAPTION | win32::WS_SYSMENU;
    let mut rect = win32::Rect {
        left: 0,
        top: 0,
        right: scale(440, dpi),
        bottom: scale(250, dpi),
    };
    // SAFETY: `rect` is a live buffer for the call.
    unsafe {
        win32::AdjustWindowRectExForDpi(&mut rect, style, 0, 0, dpi);
    }
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    // SAFETY: reads two system metrics.
    let (screen_width, screen_height) = unsafe {
        (
            win32::GetSystemMetrics(win32::SM_CXSCREEN),
            win32::GetSystemMetrics(win32::SM_CYSCREEN),
        )
    };
    let x = (screen_width - width) / 2;
    let y = (screen_height - height) / 2;
    let title = win32::wide(WINDOW_TITLE);
    // SAFETY: the class is registered; every pointer is null or a live buffer.
    let window = unsafe {
        win32::CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            x,
            y,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        )
    };
    if window.is_null() {
        // SAFETY: the creation attempt just failed.
        let error = unsafe { win32::GetLastError() };
        return Err(format!("CreateWindowExW failed with error {error}"));
    }
    WINDOW.store(window as usize, Ordering::SeqCst);
    if let Some(shared) = SHARED.get() {
        // SAFETY: called with a live window of this process on the UI thread.
        unsafe {
            refresh_labels(window, shared);
        }
    }
    // SAFETY: the window is this process's and live.
    unsafe {
        win32::ShowWindow(window, win32::SW_SHOWNORMAL);
        win32::SetForegroundWindow(window);
    }
    Ok(())
}

/// A logical length in physical pixels at `dpi`.
fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}

/// The window procedure of the settings window.
unsafe extern "system" fn window_proc(
    window: Hwnd,
    message: u32,
    wparam: Wparam,
    lparam: Lparam,
) -> Lresult {
    match message {
        win32::WM_CREATE => {
            // SAFETY: called by the system during window creation, before the
            // handle is stored, with the window live.
            unsafe { create_controls(window) };
            0
        }
        win32::WM_COMMAND => {
            // The low word is the control id, the high word its notification.
            let id = (wparam & 0xffff) as i32;
            // SAFETY: forwarded from the system for this live window.
            unsafe { on_command(window, id) };
            0
        }
        win32::WM_CLOSE => {
            if MODE.load(Ordering::SeqCst) == Mode::CloseQuits.encode() {
                // SAFETY: the window is live and owned by this process.
                unsafe {
                    win32::DestroyWindow(window);
                }
            } else {
                // SAFETY: as above; hiding only changes visibility.
                unsafe {
                    win32::ShowWindow(window, win32::SW_HIDE);
                }
            }
            0
        }
        win32::WM_CTLCOLORSTATIC => {
            // Static labels draw transparently over the dialog face.
            let _ = (wparam, lparam);
            // SAFETY: reads a documented system colour.
            unsafe { win32::GetSysColorBrush(win32::COLOR_BTNFACE) as Lresult }
        }
        win32::WM_DESTROY => {
            WINDOW.store(0, Ordering::SeqCst);
            let font = FONT.swap(0, Ordering::SeqCst) as Hfont;
            if !font.is_null() {
                // SAFETY: the font was created by this module and is deleted
                // exactly once; every control using it is already gone.
                unsafe {
                    win32::DeleteObject(font);
                }
            }
            if MODE.load(Ordering::SeqCst) == Mode::CloseQuits.encode() {
                // SAFETY: ends this thread's message loop; only called from a
                // window of the UI thread.
                unsafe {
                    win32::PostQuitMessage(0);
                }
            }
            0
        }
        _ => {
            // SAFETY: a plain forward of a message this window does not handle.
            unsafe { win32::DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}

/// Creates every child control of the page.
unsafe fn create_controls(window: Hwnd) {
    // SAFETY: called with a live window; every call below only uses it as a
    // parent handle.
    let dpi = unsafe { win32::GetDpiForWindow(window) }.max(96);
    let font = font_for(dpi);
    let static_class = win32::wide("STATIC");
    let button_class = win32::wide("BUTTON");
    // SAFETY: the class names are live buffers for every call below.
    unsafe {
        let label = |id: i32, text: &str, x: i32, y: i32, w: i32, h: i32, wrap: bool| {
            let style = if wrap {
                win32::SS_LEFT
            } else {
                win32::SS_LEFTNOWORDWRAP
            };
            control(
                window,
                static_class.as_ptr(),
                text,
                style,
                id,
                x,
                y,
                w,
                h,
                font,
                dpi,
            )
        };
        label(ID_VERSION, "Delicium", 16, 14, 400, 18, false);
        label(ID_ENGINE, "Engine: not installed", 16, 32, 400, 18, false);
        label(ID_LAST, "Last check: never", 16, 50, 400, 18, false);
        label(ID_STATUS, "", 16, 72, 400, 32, true);
        label(ID_ERROR, "", 16, 106, 400, 32, true);
        control(
            window,
            button_class.as_ptr(),
            "Character hit regions",
            win32::BS_AUTOCHECKBOX,
            ID_HITTEST,
            16,
            142,
            300,
            20,
            font,
            dpi,
        );
        let button = |id: i32, text: &str, x: i32, y: i32, w: i32| {
            control(
                window,
                button_class.as_ptr(),
                text,
                win32::BS_PUSHBUTTON,
                id,
                x,
                y,
                w,
                26,
                font,
                dpi,
            )
        };
        button(ID_CHECK, "Check for updates", 16, 176, 130);
        button(ID_INSTALL, "Install and restart", 152, 176, 130);
        button(ID_LOGS, "Open logs", 288, 176, 100);
        button(ID_QUIT, "Quit pet", 16, 208, 100);
        button(ID_CLOSE, "Close", 288, 208, 100);
    }
    // SAFETY: the checkbox was created above; its state is read from the
    // engine's configuration file.
    unsafe {
        if let Some(enabled) = paths::config_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| read_pixel_hittest(text.as_str()))
        {
            let check = win32::GetDlgItem(window, ID_HITTEST);
            let state = if enabled {
                win32::BST_CHECKED
            } else {
                win32::BST_UNCHECKED
            };
            win32::SendMessageW(check, win32::BM_SETCHECK, state, 0);
        }
    }
}

/// Creates one child control and gives it the shared font.
#[allow(clippy::too_many_arguments)]
unsafe fn control(
    parent: Hwnd,
    class: *const u16,
    text: &str,
    style: u32,
    id: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    font: Hfont,
    dpi: u32,
) -> Hwnd {
    let text = win32::wide(text);
    // SAFETY: every pointer is null or a live buffer; the parent is live.
    let handle = unsafe {
        win32::CreateWindowExW(
            0,
            class,
            text.as_ptr(),
            win32::WS_CHILD | win32::WS_VISIBLE | style,
            scale(x, dpi),
            scale(y, dpi),
            scale(width, dpi),
            scale(height, dpi),
            parent,
            id as isize as win32::Hmenu,
            win32::module(),
            std::ptr::null_mut(),
        )
    };
    if !font.is_null() && !handle.is_null() {
        // SAFETY: both handles are live; `1` asks for an immediate repaint.
        unsafe {
            win32::SendMessageW(handle, win32::WM_SETFONT, font as usize, 1);
        }
    }
    handle
}

/// The shared control font, created once.
fn font_for(dpi: u32) -> Hfont {
    let existing = FONT.load(Ordering::SeqCst);
    if existing != 0 {
        return existing as Hfont;
    }
    let height = -scale(12, dpi);
    let face = win32::wide("Segoe UI");
    // SAFETY: the face name is NUL-terminated and lives for the call; every
    // scalar is a documented constant.
    let font = unsafe {
        win32::CreateFontW(
            height,
            0,
            0,
            0,
            win32::FW_NORMAL,
            0,
            0,
            0,
            win32::DEFAULT_CHARSET,
            win32::OUT_DEFAULT_PRECIS,
            win32::CLIP_DEFAULT_PRECIS,
            win32::CLEARTYPE_QUALITY,
            win32::DEFAULT_PITCH,
            face.as_ptr(),
        )
    };
    if font.is_null() {
        log::warn!("CreateFontW failed; the settings page uses the system font");
        return std::ptr::null_mut();
    }
    FONT.store(font as usize, Ordering::SeqCst);
    font
}

/// Handles one `WM_COMMAND`.
unsafe fn on_command(window: Hwnd, id: i32) {
    let Some(shared) = SHARED.get() else {
        return;
    };
    match id {
        ID_HITTEST => {
            // SAFETY: the control belongs to this live window.
            let check = unsafe { win32::GetDlgItem(window, ID_HITTEST) };
            // SAFETY: as above; `BM_GETCHECK` only reads the button state.
            let state = unsafe { win32::SendMessageW(check, win32::BM_GETCHECK, 0, 0) };
            let enabled = state == win32::BST_CHECKED as Lresult;
            if let Err(error) = write_pixel_hittest(enabled) {
                log::warn!("Cannot save the hit-test setting: {error}");
                show_error(&format!("Cannot save the setting:\n{error}"));
                // Put the checkbox back where the file says it is.
                let restored = paths::config_path()
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .and_then(|text| read_pixel_hittest(text.as_str()))
                    .unwrap_or(!enabled);
                let state = if restored {
                    win32::BST_CHECKED
                } else {
                    win32::BST_UNCHECKED
                };
                // SAFETY: the control is live.
                unsafe {
                    win32::SendMessageW(check, win32::BM_SETCHECK, state, 0);
                }
            } else {
                log::info!("settings: character hit regions = {enabled}");
            }
            refresh();
        }
        ID_CHECK => {
            shared.request_check(false);
            log::info!("settings: update check requested");
            refresh();
        }
        ID_INSTALL => {
            shared.request_check(true);
            log::info!("settings: update and restart requested");
            refresh();
        }
        ID_LOGS => {
            let path = super::logging::recent_log(shared.root());
            if let Err(error) = open_path(&path) {
                log::warn!("Cannot open {}: {error}", path.display());
                show_error(&format!("Cannot open {}:\n{error}", path.display()));
            }
        }
        ID_QUIT => {
            log::info!("settings: quit pet requested");
            shared.request_quit();
        }
        ID_CLOSE => {
            // Same path as the title bar's close button.
            // SAFETY: the window is live.
            unsafe {
                win32::PostMessageW(window, win32::WM_CLOSE, 0, 0);
            }
        }
        _ => {}
    }
}

/// Opens `path` with the shell's default handler.
fn open_path(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let operation = win32::wide("open");
    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: every string is NUL-terminated and outlives the call; the
    // remaining pointers are documented as optional and are null.
    let result = unsafe {
        win32::ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            win32::SW_SHOWNORMAL,
        )
    };
    // `ShellExecuteW` returns a value greater than 32 on success.
    if result <= 32 {
        return Err(format!("ShellExecuteW returned {result}"));
    }
    Ok(())
}

/// Updates every label from the shared state.
unsafe fn refresh_labels(window: Hwnd, shared: &Shared) {
    let version = crate::update::current_version()
        .map(|version| version.to_string())
        .unwrap_or_else(|_| "unknown".to_owned());
    // SAFETY: the window and its controls are live and on this thread.
    unsafe {
        set_text(window, ID_VERSION, &format!("Delicium launcher {version}"));
        let engine = match shared.target() {
            Some(target) => format!("Engine: {}", target.version),
            None => "Engine: not installed".to_owned(),
        };
        set_text(window, ID_ENGINE, &engine);
        let last = match shared.last_report() {
            Some(report) => format!("Last check: {}", format_time(report.at)),
            None => "Last check: never".to_owned(),
        };
        set_text(window, ID_LAST, &last);
        let status = if shared.phase() == CheckPhase::Running {
            "Checking for updates...".to_owned()
        } else {
            match shared.last_report() {
                Some(report) => report.message,
                None => {
                    if shared.no_update() {
                        "Update checks are disabled (--no-update).".to_owned()
                    } else {
                        "No update check has run yet.".to_owned()
                    }
                }
            }
        };
        set_text(window, ID_STATUS, &status);
        let error = shared
            .last_report()
            .and_then(|report| report.error)
            .unwrap_or_default();
        set_text(window, ID_ERROR, &error);
    }
}

/// Sets the text of one control; a missing control is ignored.
unsafe fn set_text(window: Hwnd, id: i32, text: &str) {
    // SAFETY: `GetDlgItem` only looks up a child of this live window.
    let control = unsafe { win32::GetDlgItem(window, id) };
    if control.is_null() {
        return;
    }
    let text = win32::wide(text);
    // SAFETY: `WM_SETTEXT` copies the NUL-terminated string, which outlives
    // the call.
    unsafe {
        win32::SendMessageW(control, win32::WM_SETTEXT, 0, text.as_ptr() as Lparam);
    }
}

/// Writes the hit-test preference into the engine's configuration.
fn write_pixel_hittest(enabled: bool) -> Result<(), String> {
    let path = paths::config_path()
        .ok_or_else(|| "APPDATA is not set; cannot locate the configuration file".to_owned())?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Cannot read {}: {error}", path.display())),
    };
    let updated = with_pixel_hittest(text.as_deref(), enabled)?;
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("Cannot create {}: {error}", directory.display()))?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, updated)
        .map_err(|error| format!("Cannot write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, &path).map_err(|error| {
        format!(
            "Cannot replace {} with {}: {error}",
            path.display(),
            temporary.display()
        )
    })
}

/// The configuration text with `pixel_hittest` set, every other field kept.
///
/// A missing file becomes a fresh configuration; a file whose format version
/// this build does not know is refused instead of rewritten, because writing
/// it would discard fields this build cannot represent.
fn with_pixel_hittest(text: Option<&str>, enabled: bool) -> Result<String, String> {
    let mut value: serde_json::Value = match text {
        Some(text) => serde_json::from_str(text)
            .map_err(|error| format!("Cannot parse the configuration: {error}"))?,
        None => serde_json::json!({
            "version": CONFIG_VERSION,
            "pixel_hittest": enabled,
            "instances": [],
        }),
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| "The configuration is not a JSON object".to_owned())?;
    match object.get("version").and_then(serde_json::Value::as_u64) {
        Some(CONFIG_VERSION) => {}
        Some(other) => {
            return Err(format!(
                "Configuration version {other} is not supported (this build writes {CONFIG_VERSION})"
            ));
        }
        None => return Err("The configuration has no usable version field".to_owned()),
    }
    object.insert("pixel_hittest".to_owned(), serde_json::Value::Bool(enabled));
    serde_json::to_string_pretty(&value)
        .map_err(|error| format!("Cannot serialize the configuration: {error}"))
}

/// The hit-test preference stored in `text`, when it is readable.
fn read_pixel_hittest(text: &str) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    if object.get("version").and_then(serde_json::Value::as_u64) != Some(CONFIG_VERSION) {
        return None;
    }
    object.get("pixel_hittest")?.as_bool()
}

/// Changes what closing the page does. Used when a standalone page becomes
/// the page of a running pet.
pub fn set_mode(mode: Mode) {
    MODE.store(mode.encode(), Ordering::SeqCst);
}

/// Destroys the page if it exists; called when the launcher ends.
pub fn close() {
    let window = WINDOW.swap(0, Ordering::SeqCst) as Hwnd;
    if window.is_null() {
        return;
    }
    // SAFETY: the handle is this process's live settings window; destroying
    // it runs its `WM_DESTROY`, which also frees the font.
    unsafe {
        win32::DestroyWindow(window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_is_written_without_touching_instances() {
        let before = r#"{
  "version": 1,
  "pixel_hittest": true,
  "instances": [
    { "source": { "kind": "textured_example2d" }, "position": [0.0, 0.0, 0.0] }
  ]
}"#;
        let after = with_pixel_hittest(Some(before), false).unwrap();
        let value: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["pixel_hittest"], false);
        assert_eq!(value["instances"].as_array().unwrap().len(), 1);
        assert_eq!(read_pixel_hittest(&after), Some(false));
        // Unknown fields survive, so a newer engine's file is not truncated.
        let extended = r#"{"version":1,"pixel_hittest":true,"instances":[],"future":7}"#;
        let after = with_pixel_hittest(Some(extended), true).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&after).unwrap()["future"],
            7
        );
    }

    #[test]
    fn a_missing_file_becomes_a_fresh_configuration() {
        let text = with_pixel_hittest(None, true).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["pixel_hittest"], true);
        assert_eq!(value["instances"].as_array().unwrap().len(), 0);
        assert_eq!(read_pixel_hittest(&text), Some(true));
    }

    #[test]
    fn unknown_or_malformed_files_are_refused_not_rewritten() {
        assert!(with_pixel_hittest(Some("{ not json"), true).is_err());
        assert!(with_pixel_hittest(Some(r#"{"version":2,"instances":[]}"#), true).is_err());
        assert!(with_pixel_hittest(Some(r#"{"instances":[]}"#), true).is_err());
        assert!(with_pixel_hittest(Some("[]"), true).is_err());
        // An unreadable file never reports a stale value either.
        assert_eq!(read_pixel_hittest("{ not json"), None);
        assert_eq!(
            read_pixel_hittest(r#"{"version":2,"pixel_hittest":true}"#),
            None
        );
        assert_eq!(read_pixel_hittest(r#"{"version":1,"instances":[]}"#), None);
    }

    #[test]
    fn modes_encode_distinctly() {
        assert_ne!(Mode::Hide.encode(), Mode::CloseQuits.encode());
    }
}
