//! The settings page: one native window with native controls.
//!
//! The launcher deliberately does not link the renderer: a settings page that
//! needs a GPU context would fail on exactly the machines where the pet
//! cannot start, and it would pull the whole egui stack into the stable entry
//! point that must keep working while the engine is broken. Native controls
//! need nothing but `user32` and `comctl32`'s built-in button, edit and
//! combo-box classes.
//!
//! The page edits the preferences in `%APPDATA%\Delicium\config.json` by
//! rewriting that file in place: the engine watches the file's modification
//! time and applies everything it can to the running pet (hit-test, wandering,
//! language, chat settings), so no IPC channel is needed. The default
//! character is read at the next launch. Everything else in the file (the
//! saved instances, fields written by newer builds) is carried through
//! untouched.
//!
//! Every label comes from [`crate::i18n`] and follows the configured
//! language; the control font is a CJK-capable Windows face so both languages
//! render.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::i18n::{self, Text};
use crate::preferences::{self, Language, Preferences, Resolved};

use super::win32::{self, Hfont, Hwnd, Lparam, Lresult, Wparam};
use super::{CheckPhase, Shared, format_time};

/// Class of the settings window.
const WINDOW_CLASS: &str = "DeliciumSettingsWindowClass";

/// Logical size of the page. The page has no scroll bar, so every control has
/// to fit at the smallest supported DPI scale.
const WIDTH: i32 = 440;
const HEIGHT: i32 = 574;

// The layout is checkable at compile time: the bottom row (buttons at y=532,
// 26 logical pixels tall) plus the 16-pixel margin must fit the page, and the
// widest control (an edit field at x=16, 400 wide) must fit the width.
const _: () = assert!(532 + 26 + 16 <= HEIGHT);
const _: () = assert!(16 + 400 <= WIDTH);

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
const ID_WANDER: i32 = 1015;
const ID_LANGUAGE_LABEL: i32 = 1016;
const ID_LANGUAGE: i32 = 1017;
const ID_MODEL_LABEL: i32 = 1018;
const ID_MODEL: i32 = 1019;
const ID_MODEL_HINT: i32 = 1020;
const ID_CHAT_TITLE: i32 = 1021;
const ID_CHAT_ENABLED: i32 = 1022;
const ID_CHAT_URL_LABEL: i32 = 1023;
const ID_CHAT_URL: i32 = 1024;
const ID_CHAT_MODEL_LABEL: i32 = 1025;
const ID_CHAT_MODEL: i32 = 1026;
const ID_CHAT_KEY_LABEL: i32 = 1027;
const ID_CHAT_KEY: i32 = 1028;
const ID_CHAT_SAVE: i32 = 1029;
const ID_CHAT_STATUS: i32 = 1030;

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
/// The language the page is currently drawn in (`0` English, `1` Chinese).
static PAGE_LANGUAGE: AtomicUsize = AtomicUsize::new(0);
/// The library ids shown in the default-character combo, in item order.
static MODEL_IDS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
/// The shared state the window's commands act on.
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

/// Shows the settings page, creating it on first use.
pub fn show(shared: &Arc<Shared>, mode: Mode) -> Result<(), String> {
    let _ = SHARED.set(Arc::clone(shared));
    let existing = WINDOW.load(Ordering::SeqCst);
    if existing != 0 {
        let window = existing as Hwnd;
        // SAFETY: `window` is this process's live settings window; the calls
        // only change its z-order and visibility.
        unsafe {
            win32::ShowWindow(window, win32::SW_SHOWNORMAL);
            win32::BringWindowToTop(window);
            win32::SetForegroundWindow(window);
        }
        refresh();
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
    let window = window as Hwnd;
    // A language written by another process (or the engine) re-localizes the
    // page the next time it refreshes.
    let configured = configured_language();
    if configured != page_language() {
        // SAFETY: called on the UI thread with a live window of this process.
        unsafe {
            localize(window, configured);
        }
    }
    // SAFETY: called on the UI thread with a live window of this process.
    unsafe {
        refresh_labels(window, shared);
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

/// The language the page draws, as of the last localize.
fn page_language() -> Resolved {
    match PAGE_LANGUAGE.load(Ordering::SeqCst) {
        1 => Resolved::ZhHans,
        _ => Resolved::En,
    }
}

fn store_page_language(language: Resolved) {
    PAGE_LANGUAGE.store(
        match language {
            Resolved::En => 0,
            Resolved::ZhHans => 1,
        },
        Ordering::SeqCst,
    );
}

/// The language configured on disk right now.
fn configured_language() -> Resolved {
    read_preferences().language.resolved()
}

/// The preferences, with defaults when the file is missing or unreadable.
///
/// An unreadable file still shows defaults; every write goes through
/// [`preferences::update`], which refuses a version this build cannot
/// represent instead of overwriting it.
fn read_preferences() -> Preferences {
    preferences::read().unwrap_or_default()
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
        right: scale(WIDTH, dpi),
        bottom: scale(HEIGHT, dpi),
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
    let title = win32::wide(i18n::text(configured_language()).settings_title);
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
            let notification = ((wparam >> 16) & 0xffff) as u16;
            // SAFETY: forwarded from the system for this live window.
            unsafe { on_command(window, id, notification) };
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
    let edit_class = win32::wide("EDIT");
    let combo_class = win32::wide("COMBOBOX");
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
        let checkbox = |id: i32, text: &str, x: i32, y: i32| {
            control(
                window,
                button_class.as_ptr(),
                text,
                win32::BS_AUTOCHECKBOX,
                id,
                x,
                y,
                400,
                20,
                font,
                dpi,
            )
        };
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
        let edit = |id: i32, style: u32, x: i32, y: i32, w: i32| {
            control(
                window,
                edit_class.as_ptr(),
                "",
                win32::WS_TABSTOP | win32::WS_BORDER | win32::ES_AUTOHSCROLL | style,
                id,
                x,
                y,
                w,
                22,
                font,
                dpi,
            )
        };
        let combo = |id: i32, x: i32, y: i32, w: i32| {
            control(
                window,
                combo_class.as_ptr(),
                "",
                win32::WS_TABSTOP | win32::CBS_DROPDOWNLIST,
                id,
                x,
                y,
                w,
                200,
                font,
                dpi,
            )
        };

        label(ID_VERSION, "", 16, 14, 400, 18, false);
        label(ID_ENGINE, "", 16, 32, 400, 18, false);
        label(ID_LAST, "", 16, 50, 400, 18, false);
        label(ID_STATUS, "", 16, 72, 400, 30, true);
        label(ID_ERROR, "", 16, 104, 400, 30, true);
        checkbox(ID_HITTEST, "", 16, 138);
        checkbox(ID_WANDER, "", 16, 160);
        label(ID_LANGUAGE_LABEL, "", 16, 186, 200, 18, false);
        combo(ID_LANGUAGE, 16, 206, 200);
        label(ID_MODEL_LABEL, "", 16, 236, 300, 18, false);
        combo(ID_MODEL, 16, 256, 300);
        // The hint names a path long enough to wrap in both languages, so it
        // gets two lines rather than being clipped.
        label(ID_MODEL_HINT, "", 16, 284, 400, 32, true);
        label(ID_CHAT_TITLE, "", 16, 322, 400, 18, false);
        checkbox(ID_CHAT_ENABLED, "", 16, 344);
        label(ID_CHAT_URL_LABEL, "", 16, 368, 400, 18, false);
        edit(ID_CHAT_URL, 0, 16, 388, 400);
        label(ID_CHAT_MODEL_LABEL, "", 16, 414, 120, 18, false);
        edit(ID_CHAT_MODEL, 0, 142, 412, 274);
        label(ID_CHAT_KEY_LABEL, "", 16, 440, 120, 18, false);
        edit(ID_CHAT_KEY, win32::ES_PASSWORD, 142, 438, 274);
        button(ID_CHAT_SAVE, "", 16, 466, 200);
        label(ID_CHAT_STATUS, "", 228, 470, 180, 18, false);
        button(ID_CHECK, "", 16, 500, 130);
        button(ID_INSTALL, "", 152, 500, 130);
        button(ID_LOGS, "", 288, 500, 100);
        button(ID_QUIT, "", 16, 532, 100);
        button(ID_CLOSE, "", 288, 532, 100);
        // Edit fields take the same limit the preference reader enforces, so
        // a value typed here is never silently discarded later.
        for (id, limit) in [
            (ID_CHAT_URL, preferences::MAX_URL_LENGTH),
            (ID_CHAT_MODEL, preferences::MAX_MODEL_LENGTH),
            (ID_CHAT_KEY, preferences::MAX_KEY_LENGTH),
        ] {
            let field = win32::GetDlgItem(window, id);
            if !field.is_null() {
                win32::SendMessageW(field, win32::EM_SETLIMITTEXT, limit, 0);
            }
        }
    }
    // SAFETY: every control above exists and belongs to this live window.
    unsafe {
        fill_controls(window);
    }
}

/// Fills every control from the configuration. Runs once, when the page is
/// created; later refreshes only touch the dynamic status labels so text
/// being typed is never overwritten.
unsafe fn fill_controls(window: Hwnd) {
    let preferences = read_preferences();
    let language = preferences.language.resolved();
    // SAFETY: the controls belong to this live window.
    unsafe {
        set_check(window, ID_HITTEST, preferences.pixel_hittest);
        set_check(window, ID_WANDER, preferences.wander);
        set_check(window, ID_CHAT_ENABLED, preferences.chat.enabled);
        set_text(window, ID_CHAT_URL, &preferences.chat.base_url);
        set_text(window, ID_CHAT_MODEL, &preferences.chat.model);
        set_text(window, ID_CHAT_KEY, &preferences.chat.api_key);
        let models = crate::library::list(crate::library::user_directory().as_deref());
        let ids = MODEL_IDS.get_or_init(|| Mutex::new(Vec::new()));
        let mut ids = ids.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        ids.clear();
        let mut items = Vec::with_capacity(models.len());
        for model in &models {
            ids.push(model.id.clone());
            items.push(format!("{} ({})", model.name, model.id));
        }
        let selected = ids
            .iter()
            .position(|id| id == &preferences.default_model)
            .unwrap_or(0);
        drop(ids);
        fill_combo(window, ID_MODEL, &items, selected);
        localize(window, language);
    }
}

/// The language combo's item order.
fn language_index(language: Language) -> usize {
    match language {
        Language::System => 0,
        Language::ZhHans => 1,
        Language::En => 2,
    }
}

/// The language for a combo-box selection; an unknown selection is `System`.
fn language_at(index: usize) -> Language {
    match index {
        1 => Language::ZhHans,
        2 => Language::En,
        _ => Language::System,
    }
}

/// Applies every localized string to the page and rebuilds the language
/// choices. The chat edit fields are left alone.
unsafe fn localize(window: Hwnd, language: Resolved) {
    store_page_language(language);
    let text: &'static Text = i18n::text(language);
    let title = win32::wide(text.settings_title);
    // SAFETY: every call below targets this live window and its live children.
    unsafe {
        win32::SendMessageW(window, win32::WM_SETTEXT, 0, title.as_ptr() as Lparam);
        set_text(window, ID_HITTEST, text.hit_regions);
        set_text(window, ID_WANDER, text.wander);
        set_text(window, ID_LANGUAGE_LABEL, text.language);
        set_text(window, ID_MODEL_LABEL, text.default_character);
        set_text(window, ID_MODEL_HINT, text.library_hint);
        set_text(window, ID_CHAT_TITLE, text.chat_section);
        set_text(window, ID_CHAT_ENABLED, text.chat_enabled);
        set_text(window, ID_CHAT_URL_LABEL, text.chat_base_url);
        set_text(window, ID_CHAT_MODEL_LABEL, text.chat_model);
        set_text(window, ID_CHAT_KEY_LABEL, text.chat_api_key);
        set_text(window, ID_CHAT_SAVE, text.chat_save);
        set_text(window, ID_CHAT_STATUS, "");
        set_text(window, ID_CHECK, text.check_updates);
        set_text(window, ID_INSTALL, text.install_restart);
        set_text(window, ID_LOGS, text.open_logs);
        set_text(window, ID_QUIT, text.quit_pet);
        set_text(window, ID_CLOSE, text.close);
        let items = [
            text.language_system.to_owned(),
            "简体中文".to_owned(),
            "English".to_owned(),
        ];
        let selected = language_index(read_preferences().language);
        fill_combo(window, ID_LANGUAGE, &items, selected);
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
    // Microsoft YaHei UI is the Windows 8+ Chinese UI face and also carries
    // Latin glyphs, so one font serves both languages. GDI substitutes a
    // default face when it is missing, which keeps English readable.
    let face = win32::wide("Microsoft YaHei UI");
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
unsafe fn on_command(window: Hwnd, id: i32, notification: u16) {
    let Some(shared) = SHARED.get() else {
        return;
    };
    let language = page_language();
    let text: &'static Text = i18n::text(language);
    // A combo box reports several notifications per selection; only the
    // deliberate choice is a setting change.
    if matches!(id, ID_LANGUAGE | ID_MODEL) && notification != win32::CBN_SELCHANGE {
        return;
    }
    match id {
        ID_HITTEST => {
            // SAFETY: the control belongs to this live window.
            let enabled = unsafe { check_state(window, ID_HITTEST) };
            match write_preferences(|preferences| preferences.pixel_hittest = enabled) {
                Ok(()) => log::info!("settings: character hit regions = {enabled}"),
                Err(error) => {
                    log::warn!("Cannot save the hit-test setting: {error}");
                    show_error(&format!("{} {error}", text.settings_error));
                    // Put the checkbox back where the file says it is.
                    let restored = read_preferences().pixel_hittest;
                    // SAFETY: the control is live.
                    unsafe {
                        set_check(window, ID_HITTEST, restored);
                    }
                }
            }
            refresh();
        }
        ID_WANDER => {
            // SAFETY: the control belongs to this live window.
            let enabled = unsafe { check_state(window, ID_WANDER) };
            match write_preferences(|preferences| preferences.wander = enabled) {
                Ok(()) => log::info!("settings: autonomous wandering = {enabled}"),
                Err(error) => {
                    log::warn!("Cannot save the wander setting: {error}");
                    show_error(&format!("{} {error}", text.settings_error));
                    let restored = read_preferences().wander;
                    // SAFETY: the control is live.
                    unsafe {
                        set_check(window, ID_WANDER, restored);
                    }
                }
            }
        }
        ID_LANGUAGE => {
            // SAFETY: the control belongs to this live window.
            let selected = unsafe { combo_selection(window, ID_LANGUAGE) };
            let language = language_at(selected);
            match write_preferences(|preferences| preferences.language = language) {
                Ok(()) => {
                    log::info!("settings: language = {}", language.as_str());
                    // SAFETY: the window and its controls are live.
                    unsafe {
                        localize(window, language.resolved());
                        refresh_labels(window, shared);
                    }
                }
                Err(error) => {
                    log::warn!("Cannot save the language: {error}");
                    show_error(&format!("{} {error}", text.settings_error));
                    let restored = language_index(read_preferences().language);
                    // SAFETY: the control is live.
                    unsafe {
                        set_combo_selection(window, ID_LANGUAGE, restored);
                    }
                }
            }
        }
        ID_MODEL => {
            // SAFETY: the control belongs to this live window.
            let Some(model) = (unsafe { selected_model(window) }) else {
                return;
            };
            if let Err(error) =
                write_preferences(|preferences| preferences.default_model = model.clone())
            {
                log::warn!("Cannot save the default character: {error}");
                show_error(&format!("{} {error}", text.settings_error));
            } else {
                log::info!("settings: default character = {model}");
            }
        }
        ID_CHAT_ENABLED => {
            // SAFETY: the controls are live.
            unsafe {
                save_chat(window);
            }
        }
        ID_CHAT_SAVE => {
            // SAFETY: the controls are live.
            unsafe {
                save_chat(window);
            }
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

/// Reads the chat controls, validates them, and saves the whole chat group.
unsafe fn save_chat(window: Hwnd) {
    let text = i18n::text(page_language());
    // SAFETY: the controls belong to this live window.
    let enabled = unsafe { check_state(window, ID_CHAT_ENABLED) };
    // SAFETY: as above; the edit controls belong to this window.
    let base_url = unsafe { window_text(window, ID_CHAT_URL) }
        .trim()
        .to_owned();
    // SAFETY: as above.
    let model = unsafe { window_text(window, ID_CHAT_MODEL) }
        .trim()
        .to_owned();
    // SAFETY: as above.
    let api_key = unsafe { window_text(window, ID_CHAT_KEY) };
    if enabled && (base_url.is_empty() || model.is_empty()) {
        // SAFETY: the status label and the checkbox are live.
        unsafe {
            set_text(window, ID_CHAT_STATUS, text.chat_incomplete);
            // The checkbox must reflect what is stored, not what was clicked.
            set_check(window, ID_CHAT_ENABLED, read_preferences().chat.enabled);
        }
        return;
    }
    if !base_url.is_empty()
        && let Err(error) = crate::app::chat::parse_endpoint(&base_url)
    {
        let message = match error {
            crate::app::chat::EndpointError::InsecureRemote => text.chat_requires_https.to_owned(),
            other => other.to_string(),
        };
        log::warn!("settings: refusing the chat URL: {message}");
        // SAFETY: the status label and the checkbox are live.
        unsafe {
            set_text(window, ID_CHAT_STATUS, &message);
            set_check(window, ID_CHAT_ENABLED, read_preferences().chat.enabled);
        }
        return;
    }
    let settings = preferences::ChatSettings {
        enabled,
        base_url,
        model,
        api_key,
    };
    match write_preferences(|preferences| preferences.chat = settings.clone()) {
        Ok(()) => {
            log::info!(
                "settings: chat enabled={} base_url={} model={}",
                settings.enabled,
                settings.base_url,
                settings.model
            );
            // SAFETY: the status label is live.
            unsafe {
                set_text(window, ID_CHAT_STATUS, text.chat_saved);
            }
        }
        Err(error) => {
            log::warn!("Cannot save the chat settings: {error}");
            show_error(&format!("{} {error}", text.settings_error));
            // SAFETY: the status label is live.
            unsafe {
                set_text(window, ID_CHAT_STATUS, &error);
            }
        }
    }
}

/// Writes preferences through [`preferences::update`], which keeps every
/// unknown field and refuses a format version this build cannot represent.
fn write_preferences(mutate: impl FnOnce(&mut Preferences)) -> Result<(), String> {
    preferences::update(|object| apply_preferences(object, mutate))
}

/// The pure half of [`write_preferences`]: updates the page's fields in an
/// already-parsed configuration object.
///
/// Each field is written on its own, and the chat group is merged rather than
/// replaced, so a key this build does not know survives even inside `chat`.
fn apply_preferences(
    object: &mut serde_json::Map<String, serde_json::Value>,
    mutate: impl FnOnce(&mut Preferences),
) -> Result<(), String> {
    let mut current: Preferences =
        serde_json::from_value(serde_json::Value::Object(object.clone()))
            .map_err(|error| format!("Cannot read the configuration: {error}"))?;
    mutate(&mut current);
    preferences::set(object, "pixel_hittest", current.pixel_hittest);
    preferences::set(object, "wander", current.wander);
    preferences::set(object, "language", current.language.as_str());
    preferences::set(object, "default_model", current.default_model.clone());
    let mut chat = object
        .get("chat")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    preferences::set(&mut chat, "enabled", current.chat.enabled);
    preferences::set(&mut chat, "base_url", current.chat.base_url.clone());
    preferences::set(&mut chat, "model", current.chat.model.clone());
    preferences::set(&mut chat, "api_key", current.chat.api_key.clone());
    object.insert("chat".to_owned(), serde_json::Value::Object(chat));
    Ok(())
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

/// Updates the dynamic labels from the shared state.
unsafe fn refresh_labels(window: Hwnd, shared: &Shared) {
    let text = i18n::text(page_language());
    let version = crate::update::current_version()
        .map(|version| version.to_string())
        .unwrap_or_else(|_| "unknown".to_owned());
    // SAFETY: the window and its controls are live and on this thread.
    unsafe {
        set_text(
            window,
            ID_VERSION,
            &format!("{} {version}", text.launcher_version),
        );
        let engine = match shared.target() {
            Some(target) => format!("{}: {}", text.engine_version, target.version),
            None => format!("{}: {}", text.engine_version, text.engine_missing),
        };
        set_text(window, ID_ENGINE, &engine);
        let last = match shared.last_report() {
            Some(report) => format!("{}: {}", text.last_check, format_time(report.at)),
            None => text.last_check_never.to_owned(),
        };
        set_text(window, ID_LAST, &last);
        let status = if shared.phase() == CheckPhase::Running {
            text.checking.to_owned()
        } else {
            match shared.last_report() {
                Some(report) => report.message,
                None => {
                    if shared.no_update() {
                        text.updates_disabled.to_owned()
                    } else {
                        text.no_check_yet.to_owned()
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
        let preferences = read_preferences();
        set_check(window, ID_HITTEST, preferences.pixel_hittest);
        set_check(window, ID_WANDER, preferences.wander);
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

/// Reads the visible text of a control.
unsafe fn window_text(window: Hwnd, id: i32) -> String {
    // SAFETY: `GetDlgItem` only looks up a child of this live window.
    let control = unsafe { win32::GetDlgItem(window, id) };
    if control.is_null() {
        return String::new();
    }
    // SAFETY: the length call only reads the control.
    let length = unsafe { win32::GetWindowTextLengthW(control) };
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    // SAFETY: `buffer` has room for `length + 1` UTF-16 units including the
    // terminator; the call only writes into it.
    let copied =
        unsafe { win32::GetWindowTextW(control, buffer.as_mut_ptr(), buffer.len() as i32) };
    if copied <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..copied as usize])
}

/// The state of a checkbox, `false` when the control is missing.
unsafe fn check_state(window: Hwnd, id: i32) -> bool {
    // SAFETY: `GetDlgItem` only looks up a child of this live window.
    let control = unsafe { win32::GetDlgItem(window, id) };
    if control.is_null() {
        return false;
    }
    // SAFETY: `BM_GETCHECK` only reads the button state.
    unsafe {
        win32::SendMessageW(control, win32::BM_GETCHECK, 0, 0) == win32::BST_CHECKED as Lresult
    }
}

/// Sets a checkbox without emitting a `WM_COMMAND`.
unsafe fn set_check(window: Hwnd, id: i32, checked: bool) {
    // SAFETY: `GetDlgItem` only looks up a child of this live window.
    let control = unsafe { win32::GetDlgItem(window, id) };
    if control.is_null() {
        return;
    }
    let state = if checked {
        win32::BST_CHECKED
    } else {
        win32::BST_UNCHECKED
    };
    // SAFETY: `BM_SETCHECK` only changes the button state.
    unsafe {
        win32::SendMessageW(control, win32::BM_SETCHECK, state, 0);
    }
}

/// Replaces the items of a combo box and selects one.
unsafe fn fill_combo(window: Hwnd, id: i32, items: &[String], selected: usize) {
    // SAFETY: `GetDlgItem` only looks up a child of this live window.
    let combo = unsafe { win32::GetDlgItem(window, id) };
    if combo.is_null() {
        return;
    }
    // SAFETY: every string lives for its call; the combo belongs to this
    // live window.
    unsafe {
        win32::SendMessageW(combo, win32::CB_RESETCONTENT, 0, 0);
        for item in items {
            let text = win32::wide(item);
            win32::SendMessageW(combo, win32::CB_ADDSTRING, 0, text.as_ptr() as Lparam);
        }
        win32::SendMessageW(combo, win32::CB_SETCURSEL, selected, 0);
    }
}

/// The selected combo-box index, `0` when nothing or nothing valid is
/// selected.
unsafe fn combo_selection(window: Hwnd, id: i32) -> usize {
    // SAFETY: `GetDlgItem` only looks up a child of this live window.
    let combo = unsafe { win32::GetDlgItem(window, id) };
    if combo.is_null() {
        return 0;
    }
    // SAFETY: `CB_GETCURSEL` only reads the selection.
    let selected = unsafe { win32::SendMessageW(combo, win32::CB_GETCURSEL, 0, 0) };
    usize::try_from(selected).unwrap_or(0)
}

/// Selects one combo-box item.
unsafe fn set_combo_selection(window: Hwnd, id: i32, index: usize) {
    // SAFETY: `GetDlgItem` only looks up a child of this live window.
    let combo = unsafe { win32::GetDlgItem(window, id) };
    if combo.is_null() {
        return;
    }
    // SAFETY: `CB_SETCURSEL` only changes the selection.
    unsafe {
        win32::SendMessageW(combo, win32::CB_SETCURSEL, index, 0);
    }
}

/// The library id selected in the default-character combo.
unsafe fn selected_model(window: Hwnd) -> Option<String> {
    // SAFETY: the control belongs to this live window.
    let index = unsafe { combo_selection(window, ID_MODEL) };
    let ids = MODEL_IDS.get()?;
    let ids = ids.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    ids.get(index).cloned()
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
    use serde_json::json;

    /// Runs [`apply_preferences`] on a fresh stub, the way a first launch
    /// does.
    fn write_fresh(mutate: impl FnOnce(&mut Preferences)) -> serde_json::Value {
        let mut object = preferences::stub()
            .as_object()
            .expect("the stub is an object")
            .clone();
        apply_preferences(&mut object, mutate).unwrap();
        serde_json::Value::Object(object)
    }

    /// Runs [`apply_preferences`] on an explicit document.
    fn write_into(
        document: serde_json::Value,
        mutate: impl FnOnce(&mut Preferences),
    ) -> serde_json::Value {
        let mut object = document.as_object().expect("an object").clone();
        apply_preferences(&mut object, mutate).unwrap();
        serde_json::Value::Object(object)
    }

    #[test]
    fn every_page_field_is_written_without_touching_instances() {
        let before = json!({
            "version": 1,
            "pixel_hittest": true,
            "instances": [
                { "source": { "kind": "textured_example2d" }, "position": [0.0, 0.0, 0.0] }
            ]
        });
        let after = write_into(before, |preferences| {
            preferences.pixel_hittest = false;
            preferences.wander = false;
            preferences.language = Language::ZhHans;
            preferences.default_model = "robot3d".to_owned();
        });
        assert_eq!(after["version"], 1);
        assert_eq!(after["pixel_hittest"], false);
        assert_eq!(after["wander"], false);
        assert_eq!(after["language"], "zh_hans");
        assert_eq!(after["default_model"], "robot3d");
        assert_eq!(after["instances"].as_array().unwrap().len(), 1);
        // The reader sees exactly what the page wrote.
        let parsed = preferences::parse(&after.to_string()).unwrap();
        assert!(!parsed.pixel_hittest);
        assert!(!parsed.wander);
        assert_eq!(parsed.language, Language::ZhHans);
        assert_eq!(parsed.default_model, "robot3d");
    }

    #[test]
    fn unknown_fields_survive_at_every_level() {
        let before = json!({
            "version": 1,
            "instances": [],
            "future": 7,
            "chat": { "enabled": false, "base_url": "", "model": "", "api_key": "", "future": "kept" }
        });
        let after = write_into(before, |preferences| {
            preferences.chat = preferences::ChatSettings {
                enabled: true,
                base_url: "https://api.example.test/v1".to_owned(),
                model: "m".to_owned(),
                api_key: "sk-test".to_owned(),
            };
        });
        assert_eq!(after["future"], 7);
        assert_eq!(after["chat"]["future"], "kept");
        assert_eq!(after["chat"]["enabled"], true);
        assert_eq!(after["chat"]["base_url"], "https://api.example.test/v1");
        assert_eq!(after["chat"]["model"], "m");
        assert_eq!(after["chat"]["api_key"], "sk-test");
    }

    #[test]
    fn a_missing_file_becomes_a_fresh_configuration() {
        let after = write_fresh(|preferences| preferences.language = Language::En);
        assert_eq!(after["version"], 1);
        assert_eq!(after["language"], "en");
        assert_eq!(after["instances"].as_array().unwrap().len(), 0);
        let parsed = preferences::parse(&after.to_string()).unwrap();
        assert_eq!(parsed.language, Language::En);
        assert!(parsed.pixel_hittest);
    }

    #[test]
    fn the_page_fields_match_the_stored_document() {
        // Every field the page edits must be readable again through the
        // shared reader: a typo in a key name would otherwise only show up as
        // a setting that silently does nothing.
        let after = write_fresh(|preferences| {
            preferences.pixel_hittest = false;
            preferences.wander = false;
            preferences.language = Language::En;
            preferences.default_model = "puppet2d".to_owned();
            preferences.chat.enabled = true;
            preferences.chat.base_url = "http://127.0.0.1:8080/v1".to_owned();
            preferences.chat.model = "local".to_owned();
            preferences.chat.api_key = "k".to_owned();
        });
        let parsed =
            preferences::parse(&after.to_string()).expect("the page wrote a valid document");
        assert!(!parsed.pixel_hittest);
        assert!(!parsed.wander);
        assert_eq!(parsed.language, Language::En);
        assert_eq!(parsed.default_model, "puppet2d");
        assert!(parsed.chat.is_configured());
        assert_eq!(parsed.chat.model, "local");
        assert_eq!(parsed.chat.api_key, "k");
    }

    #[test]
    fn an_invalid_character_id_is_still_sanitized_by_the_reader() {
        // The page only offers ids from the library, but the merge must not
        // be the only guard: hand-edited files go through the same reader.
        let after = write_fresh(|preferences| preferences.default_model = "../evil".to_owned());
        let parsed = preferences::parse(&after.to_string()).unwrap();
        assert_eq!(parsed.default_model, crate::preferences::DEFAULT_PET);
    }

    #[test]
    fn language_choices_round_trip() {
        for language in [Language::System, Language::ZhHans, Language::En] {
            assert_eq!(language_at(language_index(language)), language);
        }
        // An impossible selection is treated as the documented default, never
        // as a panic.
        assert_eq!(language_at(99), Language::System);
    }

    #[test]
    fn the_language_combo_lists_system_first() {
        assert_eq!(language_index(Language::System), 0);
        assert_eq!(language_index(Language::ZhHans), 1);
        assert_eq!(language_index(Language::En), 2);
    }

    #[test]
    fn modes_encode_distinctly() {
        assert_ne!(Mode::Hide.encode(), Mode::CloseQuits.encode());
    }
}
