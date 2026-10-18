//! The Win32 surface the launcher uses, declared by hand.
//!
//! The crate already talks to the shell through hand-written FFI
//! (`src/platform/`), and the launcher follows the same rule: no new
//! dependency, only the functions it actually calls. Process creation itself
//! goes through [`std::process::Command`], which builds the command line and
//! the environment block correctly; only what the standard library cannot do
//! is declared here.

#![allow(non_snake_case)]

use core::ffi::c_void;

pub type Hwnd = *mut c_void;
pub type Hinstance = *mut c_void;
pub type Hicon = *mut c_void;
pub type Hcursor = *mut c_void;
pub type Hbrush = *mut c_void;
pub type Hmenu = *mut c_void;
pub type Hfont = *mut c_void;
pub type Hmutex = *mut c_void;
pub type Hevent = *mut c_void;
pub type Hprocess = *mut c_void;
pub type Hmodule = *mut c_void;
pub type Lresult = isize;
pub type Wparam = usize;
pub type Lparam = isize;

/// The pseudo-parent that makes a window message-only: it is never shown,
/// never enumerated as a normal window, and is reachable only through
/// [`FindWindowExW`] with this same parent.
pub const HWND_MESSAGE: Hwnd = -3isize as Hwnd;

/// The desktop window, used to find the engine's top-level windows.
pub const HWND_BROADCAST: Hwnd = 0xffffisize as Hwnd;

/// `WM_APP`: the first message id applications may use freely.
pub const WM_APP: u32 = 0x8000;
pub const WM_NULL: u32 = 0x0000;
pub const WM_CREATE: u32 = 0x0001;
pub const WM_DESTROY: u32 = 0x0002;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_QUIT: u32 = 0x0012;
pub const WM_COMMAND: u32 = 0x0111;
pub const WM_CTLCOLORSTATIC: u32 = 0x0138;
pub const WM_SETTEXT: u32 = 0x000C;
pub const WM_GETTEXT: u32 = 0x000D;
pub const WM_GETTEXTLENGTH: u32 = 0x000E;
pub const WM_SETFONT: u32 = 0x0030;
pub const WM_SIZE: u32 = 0x0005;
pub const WM_KEYDOWN: u32 = 0x0100;

/// `BN_CLICKED`, the notification code in `HIWORD(wParam)` of [`WM_COMMAND`].
pub const BN_CLICKED: u16 = 0;

/// `BM_SETCHECK` / `BM_GETCHECK`.
pub const BM_GETCHECK: u32 = 0x00F0;
pub const BM_SETCHECK: u32 = 0x00F1;
pub const BST_CHECKED: usize = 1;
pub const BST_UNCHECKED: usize = 0;

/// Window styles.
pub const WS_OVERLAPPED: u32 = 0x0000_0000;
pub const WS_CAPTION: u32 = 0x00C0_0000;
pub const WS_SYSMENU: u32 = 0x0008_0000;
pub const WS_MINIMIZEBOX: u32 = 0x0002_0000;
pub const WS_CHILD: u32 = 0x4000_0000;
pub const WS_VISIBLE: u32 = 0x1000_0000;
pub const WS_TABSTOP: u32 = 0x0001_0000;

/// Static-control styles.
pub const SS_LEFT: u32 = 0x0000_0000;
pub const SS_LEFTNOWORDWRAP: u32 = 0x0000_000C;

/// Button styles.
pub const BS_PUSHBUTTON: u32 = 0x0000_0000;
pub const BS_AUTOCHECKBOX: u32 = 0x0000_0003;

/// `ShowWindow` commands.
pub const SW_SHOWNORMAL: i32 = 1;
pub const SW_SHOW: i32 = 5;
pub const SW_HIDE: i32 = 0;

/// `MessageBoxW` flags.
pub const MB_OK: u32 = 0x0000;
pub const MB_ICONERROR: u32 = 0x0010;
pub const MB_ICONINFORMATION: u32 = 0x0040;
pub const MB_SETFOREGROUND: u32 = 0x0001_0000;
pub const MB_TOPMOST: u32 = 0x0004_0000;

/// `MsgWaitForMultipleObjectsEx`.
pub const QS_ALLINPUT: u32 = 0x04FF;
pub const MWMO_INPUTAVAILABLE: u32 = 0x0004;
pub const WAIT_OBJECT_0: u32 = 0;
pub const WAIT_TIMEOUT: u32 = 258;
pub const INFINITE: u32 = u32::MAX;

/// `PeekMessageW`.
pub const PM_REMOVE: u32 = 0x0001;

/// `SetWindowPos`.
pub const SWP_NOSIZE: u32 = 0x0001;
pub const SWP_NOMOVE: u32 = 0x0002;
pub const SWP_NOZORDER: u32 = 0x0004;
pub const SWP_SHOWWINDOW: u32 = 0x0040;

/// `GetSystemMetrics`.
pub const SM_CXSCREEN: i32 = 0;
pub const SM_CYSCREEN: i32 = 1;

/// `ReplaceFileW` flags.
pub const REPLACEFILE_WRITE_THROUGH: u32 = 0x1;
pub const REPLACEFILE_IGNORE_MERGE_ERRORS: u32 = 0x2;

/// `CreateMutexW` / `CreateEventW` last-error value: the object existed.
pub const ERROR_ALREADY_EXISTS: u32 = 183;

/// The system brush drawn as the background of a dialog-like window.
pub const COLOR_BTNFACE: i32 = 15;

/// `LoadCursorW` resource id for the standard arrow.
pub const IDC_ARROW: *const u16 = 32512usize as *const u16;

/// `SetProcessDpiAwarenessContext`: per-monitor v2.
pub const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: isize = -4;

/// `CreateFontW` weight: regular.
pub const FW_NORMAL: i32 = 400;
pub const DEFAULT_CHARSET: u32 = 1;
pub const OUT_DEFAULT_PRECIS: u32 = 0;
pub const CLIP_DEFAULT_PRECIS: u32 = 0;
pub const CLEARTYPE_QUALITY: u32 = 5;
pub const DEFAULT_PITCH: u32 = 0;

/// A Win32 `POINT`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// A Win32 `RECT`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// A Win32 `MSG`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Msg {
    pub hwnd: Hwnd,
    pub message: u32,
    pub wparam: Wparam,
    pub lparam: Lparam,
    pub time: u32,
    pub point: Point,
    pub private: u32,
}

/// A Win32 `WNDCLASSEXW` (80 bytes on x64).
#[repr(C)]
pub struct WndClassExW {
    pub cb_size: u32,
    pub style: u32,
    pub lpfn_wnd_proc: Option<unsafe extern "system" fn(Hwnd, u32, Wparam, Lparam) -> Lresult>,
    pub cb_cls_extra: i32,
    pub cb_wnd_extra: i32,
    pub h_instance: Hinstance,
    pub h_icon: Hicon,
    pub h_cursor: Hcursor,
    pub hbr_background: Hbrush,
    pub lpsz_menu_name: *const u16,
    pub lpsz_class_name: *const u16,
    pub h_icon_sm: Hicon,
}

const _: () = assert!(core::mem::size_of::<WndClassExW>() == 80);
const _: () = assert!(core::mem::size_of::<Msg>() == 48);

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetModuleHandleW(name: *const u16) -> Hmodule;
    pub fn GetLastError() -> u32;
    pub fn CreateMutexW(attributes: *const c_void, owner: i32, name: *const u16) -> Hmutex;
    pub fn CreateEventW(
        attributes: *const c_void,
        manual_reset: i32,
        initial: i32,
        name: *const u16,
    ) -> Hevent;
    pub fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> Hevent;
    pub fn SetEvent(event: Hevent) -> i32;
    pub fn CloseHandle(handle: *mut c_void) -> i32;
    pub fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Hprocess;
    pub fn GetTickCount64() -> u64;
}

#[link(name = "user32")]
unsafe extern "system" {
    pub fn RegisterClassExW(class: *const WndClassExW) -> u16;
    pub fn UnregisterClassW(class_name: *const u16, instance: Hinstance) -> i32;
    pub fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Hwnd,
        menu: Hmenu,
        instance: Hinstance,
        param: *mut c_void,
    ) -> Hwnd;
    pub fn DestroyWindow(window: Hwnd) -> i32;
    pub fn DefWindowProcW(window: Hwnd, message: u32, wparam: Wparam, lparam: Lparam) -> Lresult;
    pub fn RegisterWindowMessageW(text: *const u16) -> u32;
    pub fn FindWindowExW(
        parent: Hwnd,
        after: Hwnd,
        class_name: *const u16,
        window_name: *const u16,
    ) -> Hwnd;
    pub fn PostMessageW(window: Hwnd, message: u32, wparam: Wparam, lparam: Lparam) -> i32;
    pub fn SendMessageW(window: Hwnd, message: u32, wparam: Wparam, lparam: Lparam) -> Lresult;
    pub fn MsgWaitForMultipleObjectsEx(
        count: u32,
        handles: *const Hprocess,
        milliseconds: u32,
        wake_mask: u32,
        flags: u32,
    ) -> u32;
    pub fn PeekMessageW(message: *mut Msg, window: Hwnd, min: u32, max: u32, remove: u32) -> i32;
    pub fn TranslateMessage(message: *const Msg) -> i32;
    pub fn DispatchMessageW(message: *const Msg) -> Lresult;
    pub fn ShowWindow(window: Hwnd, command: i32) -> i32;
    pub fn SetForegroundWindow(window: Hwnd) -> i32;
    pub fn BringWindowToTop(window: Hwnd) -> i32;
    pub fn IsWindowVisible(window: Hwnd) -> i32;
    pub fn IsWindowEnabled(window: Hwnd) -> i32;
    pub fn EnableWindow(window: Hwnd, enabled: i32) -> i32;
    pub fn MessageBoxW(owner: Hwnd, text: *const u16, caption: *const u16, kind: u32) -> i32;
    pub fn GetClientRect(window: Hwnd, rect: *mut Rect) -> i32;
    pub fn GetWindowRect(window: Hwnd, rect: *mut Rect) -> i32;
    pub fn SetWindowPos(
        window: Hwnd,
        after: Hwnd,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        flags: u32,
    ) -> i32;
    pub fn GetDpiForWindow(window: Hwnd) -> u32;
    pub fn GetDpiForSystem() -> u32;
    pub fn AdjustWindowRectExForDpi(
        rect: *mut Rect,
        style: u32,
        menu: i32,
        ex_style: u32,
        dpi: u32,
    ) -> i32;
    pub fn GetSystemMetrics(index: i32) -> i32;
    pub fn GetCursorPos(point: *mut Point) -> i32;
    pub fn EnumWindows(
        callback: Option<unsafe extern "system" fn(Hwnd, Lparam) -> i32>,
        lparam: Lparam,
    ) -> i32;
    pub fn GetWindowThreadProcessId(window: Hwnd, pid: *mut u32) -> u32;
    pub fn SetProcessDpiAwarenessContext(context: isize) -> i32;
    pub fn GetDlgItem(window: Hwnd, id: i32) -> Hwnd;
    pub fn PostQuitMessage(code: i32);
    pub fn LoadCursorW(instance: Hinstance, name: *const u16) -> Hcursor;
    pub fn GetSysColorBrush(index: i32) -> Hbrush;
    pub fn SetFocus(window: Hwnd) -> Hwnd;
    pub fn SetTimer(window: Hwnd, id: usize, milliseconds: u32, callback: usize) -> usize;
    pub fn KillTimer(window: Hwnd, id: usize) -> i32;
    pub fn GetMessageW(message: *mut Msg, window: Hwnd, min: u32, max: u32) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    pub fn ShellExecuteW(
        window: Hwnd,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> isize;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    pub fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strike_out: u32,
        charset: u32,
        output_precision: u32,
        clip_precision: u32,
        quality: u32,
        pitch_and_family: u32,
        face: *const u16,
    ) -> Hfont;
    pub fn DeleteObject(object: *mut c_void) -> i32;
}

/// A null-terminated UTF-16 copy of `text`.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The process module handle, valid for the whole process.
pub fn module() -> Hinstance {
    // SAFETY: a null name asks for this process's module, which always exists.
    let handle = unsafe { GetModuleHandleW(std::ptr::null()) };
    if handle.is_null() {
        // SAFETY: `GetModuleHandleW` just failed and set the error slot.
        let error = unsafe { GetLastError() };
        log::error!("GetModuleHandleW failed with error {error}");
    }
    handle
}

/// Replaces `replaced` with `replacement`, keeping the old content in
/// `backup`.
///
/// This is the one call that can substitute a running executable's image on
/// disk: `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` fails with access denied
/// while the image is mapped, `ReplaceFileW` succeeds (measured on this
/// machine, see the launcher design notes). The replacement file is consumed.
pub fn replace_file(
    replaced: &std::path::Path,
    replacement: &std::path::Path,
    backup: &std::path::Path,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let replaced: Vec<u16> = replaced
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let replacement: Vec<u16> = replacement
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let backup: Vec<u16> = backup
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: every string is NUL-terminated and outlives the call; the
    // optional pointers are documented as optional and are null here.
    let ok = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            backup.as_ptr(),
            REPLACEFILE_WRITE_THROUGH | REPLACEFILE_IGNORE_MERGE_ERRORS,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        // SAFETY: the call just failed and set the thread error slot.
        let error = unsafe { GetLastError() };
        return Err(format!("ReplaceFileW failed with error {error}"));
    }
    Ok(())
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn ReplaceFileW(
        replaced: *const u16,
        replacement: *const u16,
        backup: *const u16,
        flags: u32,
        exclude: *const c_void,
        reserved: *const c_void,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_are_nul_terminated() {
        assert_eq!(wide("ab"), vec![b'a' as u16, b'b' as u16, 0]);
    }
}
