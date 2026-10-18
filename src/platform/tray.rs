//! System tray icon for the control window, built on hand-written Win32 FFI.
//!
//! The icon lives on a hidden top-level window owned by the event-loop
//! thread; winit's message pump dispatches the shell's callbacks to the
//! window procedure below, which only records action bits. The application
//! polls [`TrayIcon::take_action`] once per frame and never blocks on the
//! shell.
//!
//! [`install`] must run on the same thread as the event loop. On platforms
//! without the Windows shell it fails, so callers fall back to the plain
//! close-to-quit behaviour.

/// What the user asked for through the tray icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// Left click, or the "Show/Hide controls" menu entry.
    ToggleControls,
    /// The "Quit" menu entry.
    Quit,
    /// Explorer restarted; the icon must be added again.
    Reinstall,
    /// The "Settings..." menu entry; present only when a launcher owns this
    /// engine.
    Settings,
}

/// An installed tray icon; removing it happens on drop.
#[cfg(target_os = "windows")]
pub struct TrayIcon {
    window: win32::TrayWindow,
}

/// Installs the tray icon and its hidden window.
///
/// Must be called from the thread that owns the event loop. The returned
/// value removes the icon in its `Drop`.
#[cfg(target_os = "windows")]
pub fn install() -> Result<TrayIcon, String> {
    win32::install().map(|window| TrayIcon { window })
}

/// Asks the launcher that started this engine to show its settings page.
///
/// Fails when no launcher is running; the menu entry is only offered when
/// one is, so a failure here means the launcher went away between the menu
/// opening and the click.
#[cfg(target_os = "windows")]
pub fn open_settings() -> Result<(), String> {
    win32::open_settings()
}

#[cfg(not(target_os = "windows"))]
pub fn open_settings() -> Result<(), String> {
    Err("The system tray is not supported on this platform".to_owned())
}

#[cfg(target_os = "windows")]
impl TrayIcon {
    /// Takes the next pending tray action, in priority order: toggle, quit,
    /// settings, reinstall. Consumes at most one action per call.
    pub fn take_action(&self) -> Option<TrayAction> {
        win32::take_action()
    }

    /// Adds the icon back after the shell restarted.
    pub fn reinstall_icon(&self) -> Result<(), String> {
        self.window.reinstall_icon()
    }
}

/// Non-Windows stub; a tray needs the Windows shell, so `install` fails and
/// callers fall back to close-to-quit, mirroring `set_hit_region`.
#[cfg(not(target_os = "windows"))]
pub struct TrayIcon;

#[cfg(not(target_os = "windows"))]
pub fn install() -> Result<TrayIcon, String> {
    Err("The system tray is not supported on this platform".to_owned())
}

#[cfg(not(target_os = "windows"))]
impl TrayIcon {
    pub fn take_action(&self) -> Option<TrayAction> {
        None
    }

    pub fn reinstall_icon(&self) -> Result<(), String> {
        Err("The system tray is not supported on this platform".to_owned())
    }
}

/// Minimal hand-written Win32 FFI for the notification area, deliberately
/// free of new dependencies. The hidden window receives `WM_APP`-based
/// callbacks from the shell and writes action bits into one atomic; the
/// application polls those bits and performs the actual work.
#[cfg(target_os = "windows")]
mod win32 {
    use core::ffi::c_void;
    use core::ptr;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::TrayAction;

    /// A window handle (Win32 `HWND`).
    type Hwnd = *mut c_void;
    /// An icon handle (Win32 `HICON`).
    type Hicon = *mut c_void;
    /// A bitmap handle (Win32 `HBITMAP`).
    type Hbitmap = *mut c_void;
    /// A menu handle (Win32 `HMENU`).
    type Hmenu = *mut c_void;
    /// A module handle (Win32 `HMODULE` / `HINSTANCE`).
    type Hinstance = *mut c_void;
    /// A device-context handle (Win32 `HDC`); only ever passed as null.
    type Hdc = *mut c_void;
    /// A cursor handle (Win32 `HCURSOR`); only ever passed as null.
    type Hcursor = *mut c_void;
    /// A brush handle (Win32 `HBRUSH`); only ever passed as null.
    type Hbrush = *mut c_void;
    /// A window-procedure return value (Win32 `LRESULT`).
    type Lresult = isize;
    /// A window-procedure message parameter (Win32 `WPARAM`).
    type Wparam = usize;
    /// A window-procedure message parameter (Win32 `LPARAM`).
    type Lparam = isize;

    /// `WM_APP`: the first message id applications may use freely.
    const WM_APP: u32 = 0x8000;
    /// The message the shell posts to the icon's window for its events.
    const TRAY_CALLBACK: u32 = WM_APP + 1;
    /// A benign message used to close the context-menu dance.
    const WM_NULL: u32 = 0x0000;
    const WM_CONTEXTMENU: u32 = 0x007B;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_LBUTTONDBLCLK: u32 = 0x0203;
    const WM_RBUTTONUP: u32 = 0x0205;
    /// `WM_USER + 0` / `+ 1`: version-4 select notifications.
    const NIN_SELECT: u32 = 0x0400;
    const NIN_KEYSELECT: u32 = 0x0401;

    /// Action bits written by the window procedure.
    const ACTION_TOGGLE: u32 = 1;
    const ACTION_QUIT: u32 = 2;
    const ACTION_REINSTALL: u32 = 4;
    const ACTION_SETTINGS: u32 = 8;
    /// The window procedure's atomic mailbox; the application polls it.
    static ACTIONS: AtomicU32 = AtomicU32::new(0);
    /// `RegisterWindowMessageW("TaskbarCreated")`, set once at install.
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

    /// `Shell_NotifyIconW` message ids.
    const NIM_ADD: u32 = 0;
    const NIM_DELETE: u32 = 2;
    const NIM_SETVERSION: u32 = 4;
    /// `NOTIFYICONDATAW` data flags.
    const NIF_MESSAGE: u32 = 0x01;
    const NIF_ICON: u32 = 0x02;
    const NIF_TIP: u32 = 0x04;
    /// `NOTIFYICON_VERSION_4`: select/context events in `LOWORD(lParam)`.
    const NOTIFYICON_VERSION_4: u32 = 4;

    /// Styles of the hidden message window.
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;

    /// Menu flags.
    const MF_STRING: u32 = 0x0000;
    const TPM_RIGHTBUTTON: u32 = 0x0002;
    const TPM_NONOTIFY: u32 = 0x0080;
    const TPM_RETURNCMD: u32 = 0x0100;
    /// The context-menu command ids.
    const MENU_SHOW_HIDE: i32 = 1;
    const MENU_QUIT: i32 = 2;
    const MENU_SETTINGS: i32 = 3;

    /// `CreateDIBSection` usage: the colour table holds literal RGB values.
    const DIB_RGB_COLORS: u32 = 0;

    /// Geometry of the programmatic 32x32 icon.
    const ICON_SIZE: i32 = 32;
    const ICON_RADIUS: f32 = 12.0;
    /// Face colour, in the BGRA byte order of the DIB memory.
    const FACE_COLOR: [u8; 4] = [240, 232, 232, 255];
    /// Eye colour, in the BGRA byte order of the DIB memory.
    const EYE_COLOR: [u8; 4] = [40, 32, 32, 255];
    /// Eye rectangle left edges and their size.
    const EYE_LEFT: [i32; 2] = [10, 18];
    const EYE_TOP: i32 = 12;
    const EYE_WIDTH: i32 = 4;
    const EYE_HEIGHT: i32 = 4;

    /// Window class, window title and tooltip text of the tray icon.
    const CLASS_NAME: &str = "DeliciumTrayWindowClass";
    const WINDOW_TITLE: &str = "Delicium Tray";
    const TOOLTIP: &str = "Delicium";

    /// A Win32 `POINT`: two `LONG` (`i32`) coordinates.
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    /// A Win32 `GUID`; always left zeroed (no icon GUID is used).
    #[repr(C)]
    #[derive(Default)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    /// The Win32 `NOTIFYICONDATAW` with the version-3 layout (976 bytes on
    /// x64), matching the offset assertions in the tests below.
    #[repr(C)]
    struct NotifyIconDataW {
        cb_size: u32,
        hwnd: Hwnd,
        u_id: u32,
        u_flags: u32,
        u_callback_message: u32,
        h_icon: Hicon,
        sz_tip: [u16; 128],
        dw_state: u32,
        dw_state_mask: u32,
        sz_info: [u16; 256],
        u_version_or_timeout: u32,
        sz_info_title: [u16; 64],
        dw_info_flags: u32,
        guid: Guid,
        h_balloon_icon: Hicon,
    }

    /// The Win32 `BITMAPINFOHEADER`.
    #[repr(C)]
    struct BitmapInfoHeader {
        bi_size: u32,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: u32,
        bi_size_image: u32,
        bi_x_pels_per_meter: i32,
        bi_y_pels_per_meter: i32,
        bi_clr_used: u32,
        bi_clr_important: u32,
    }

    /// One `RGBQUAD`; only present because `BITMAPINFO` requires it.
    #[repr(C)]
    struct RgbQuad {
        blue: u8,
        green: u8,
        red: u8,
        reserved: u8,
    }

    /// The Win32 `BITMAPINFO` with the smallest colour table.
    #[repr(C)]
    struct BitmapInfo {
        header: BitmapInfoHeader,
        colors: [RgbQuad; 1],
    }

    /// The Win32 `ICONINFO`.
    #[repr(C)]
    struct IconInfo {
        f_icon: i32,
        x_hotspot: u32,
        y_hotspot: u32,
        hbm_mask: Hbitmap,
        hbm_color: Hbitmap,
    }

    /// The Win32 `WNDCLASSEXW` (80 bytes on x64).
    #[repr(C)]
    struct WndClassExW {
        cb_size: u32,
        style: u32,
        lpfn_wnd_proc: Option<unsafe extern "system" fn(Hwnd, u32, Wparam, Lparam) -> Lresult>,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: Hinstance,
        h_icon: Hicon,
        h_cursor: Hcursor,
        hbr_background: Hbrush,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
        h_icon_sm: Hicon,
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn Shell_NotifyIconW(message: u32, data: *const NotifyIconDataW) -> i32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassExW(class: *const WndClassExW) -> u16;
        fn UnregisterClassW(class_name: *const u16, instance: Hinstance) -> i32;
        fn CreateWindowExW(
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
        fn DestroyWindow(window: Hwnd) -> i32;
        fn DefWindowProcW(window: Hwnd, message: u32, wparam: Wparam, lparam: Lparam) -> Lresult;
        fn RegisterWindowMessageW(text: *const u16) -> u32;
        fn CreatePopupMenu() -> Hmenu;
        fn AppendMenuW(menu: Hmenu, flags: u32, id: usize, text: *const u16) -> i32;
        fn TrackPopupMenu(
            menu: Hmenu,
            flags: u32,
            x: i32,
            y: i32,
            reserved: i32,
            window: Hwnd,
            rect: *const c_void,
        ) -> i32;
        fn DestroyMenu(menu: Hmenu) -> i32;
        fn SetForegroundWindow(window: Hwnd) -> i32;
        fn GetCursorPos(point: *mut Point) -> i32;
        fn PostMessageW(window: Hwnd, message: u32, wparam: Wparam, lparam: Lparam) -> i32;
        fn FindWindowExW(
            parent: Hwnd,
            after: Hwnd,
            class_name: *const u16,
            window_name: *const u16,
        ) -> Hwnd;
        fn CreateIconIndirect(info: *const IconInfo) -> Hicon;
        fn DestroyIcon(icon: Hicon) -> i32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateDIBSection(
            dc: Hdc,
            info: *const BitmapInfo,
            usage: u32,
            bits: *mut *mut c_void,
            section: *mut c_void,
            offset: u32,
        ) -> Hbitmap;
        fn CreateBitmap(
            width: i32,
            height: i32,
            planes: u32,
            bit_count: u32,
            bits: *const c_void,
        ) -> Hbitmap;
        fn DeleteObject(object: *mut c_void) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> Hinstance;
    }

    /// A null-terminated UTF-16 copy of `text`.
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The icon's 32x32 BGRA pixel buffer (top-down rows, DIB order): a round
    /// face with two dark eyes on a fully transparent surround.
    fn icon_pixels() -> Vec<u8> {
        let mut pixels = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
        for y in 0..ICON_SIZE {
            for x in 0..ICON_SIZE {
                let dx = x as f32 + 0.5 - ICON_SIZE as f32 * 0.5;
                let dy = y as f32 + 0.5 - ICON_SIZE as f32 * 0.5;
                let in_face = (dx * dx + dy * dy).sqrt() <= ICON_RADIUS;
                let in_eye = (EYE_TOP..EYE_TOP + EYE_HEIGHT).contains(&y)
                    && EYE_LEFT
                        .iter()
                        .any(|left| x >= *left && x < left + EYE_WIDTH);
                let color = match (in_eye, in_face) {
                    (true, _) => EYE_COLOR,
                    (false, true) => FACE_COLOR,
                    (false, false) => [0, 0, 0, 0],
                };
                let offset = ((y * ICON_SIZE + x) * 4) as usize;
                pixels[offset..offset + 4].copy_from_slice(&color);
            }
        }
        pixels
    }

    /// Builds the tray icon from the programmatic pixel buffer.
    fn create_icon() -> Result<Hicon, String> {
        let pixels = icon_pixels();
        let info = BitmapInfo {
            header: BitmapInfoHeader {
                bi_size: core::mem::size_of::<BitmapInfoHeader>() as u32,
                bi_width: ICON_SIZE,
                bi_height: -ICON_SIZE, // top-down rows
                bi_planes: 1,
                bi_bit_count: 32,
                bi_compression: 0, // BI_RGB
                bi_size_image: 0,
                bi_x_pels_per_meter: 0,
                bi_y_pels_per_meter: 0,
                bi_clr_used: 0,
                bi_clr_important: 0,
            },
            colors: [RgbQuad {
                blue: 0,
                green: 0,
                red: 0,
                reserved: 0,
            }],
        };
        let mut bits: *mut c_void = ptr::null_mut();
        // SAFETY: `info` is a fully initialised BITMAPINFO; a null device
        // context is documented for CreateDIBSection (the DIB lives in
        // memory) and `bits` receives the pixel pointer.
        let color = unsafe {
            CreateDIBSection(
                ptr::null_mut(),
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                ptr::null_mut(),
                0,
            )
        };
        if color.is_null() || bits.is_null() {
            // A bitmap without its pixel pointer cannot be filled; release it
            // so the failure path owns nothing.
            if !color.is_null() {
                // SAFETY: `color` was just created here and is not used again.
                unsafe { DeleteObject(color) };
            }
            return Err("CreateDIBSection failed to create the tray icon bitmap".to_owned());
        }
        // SAFETY: `bits` points at the DIB's pixel buffer, which holds
        // ICON_SIZE * ICON_SIZE 32-bit pixels owned by `color`, and `pixels`
        // has exactly that many bytes.
        unsafe { ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len()) };
        // The mask is all zeroes: monochrome rows are WORD-aligned
        // (ICON_SIZE / 8 = 4 bytes per row), and with a 32-bit colour bitmap
        // the alpha channel is authoritative.
        let mask_bits = vec![0u8; (ICON_SIZE * ICON_SIZE / 8) as usize];
        // SAFETY: `mask_bits` holds the full monochrome bitmap and stays
        // alive for the call; CreateBitmap only reads it.
        let mask = unsafe { CreateBitmap(ICON_SIZE, ICON_SIZE, 1, 1, mask_bits.as_ptr().cast()) };
        let icon = if mask.is_null() {
            ptr::null_mut()
        } else {
            let icon_info = IconInfo {
                f_icon: 1,
                x_hotspot: 0,
                y_hotspot: 0,
                hbm_mask: mask,
                hbm_color: color,
            };
            // SAFETY: both bitmaps are live; CreateIconIndirect copies them
            // into the new icon and does not take ownership.
            unsafe { CreateIconIndirect(&icon_info) }
        };
        // SAFETY: the icon, if any, copied both bitmaps; the originals are
        // still owned here and deleted exactly once.
        unsafe {
            DeleteObject(color);
            if !mask.is_null() {
                DeleteObject(mask);
            }
        }
        if icon.is_null() {
            Err("CreateIconIndirect failed to build the tray icon".to_owned())
        } else {
            Ok(icon)
        }
    }

    /// Registers the hidden window class used by the tray window.
    fn register_window_class(class_name: &[u16], instance: Hinstance) -> Result<(), String> {
        let class = WndClassExW {
            cb_size: core::mem::size_of::<WndClassExW>() as u32,
            style: 0,
            lpfn_wnd_proc: Some(window_proc),
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: instance,
            h_icon: ptr::null_mut(),
            h_cursor: ptr::null_mut(),
            hbr_background: ptr::null_mut(),
            lpsz_menu_name: ptr::null(),
            lpsz_class_name: class_name.as_ptr(),
            h_icon_sm: ptr::null_mut(),
        };
        // SAFETY: `class` and `class_name` are fully initialised and outlive
        // the call; RegisterClassExW only reads them.
        if unsafe { RegisterClassExW(&class) } == 0 {
            return Err("RegisterClassExW failed for the tray window class".to_owned());
        }
        Ok(())
    }

    /// Destroys the hidden window and unregisters its class.
    fn destroy_window(hwnd: Hwnd, class_name: &[u16], instance: Hinstance) {
        // SAFETY: `hwnd` is ours, `class_name` outlives the call, and the
        // class has no other windows.
        unsafe {
            DestroyWindow(hwnd);
            UnregisterClassW(class_name.as_ptr(), instance);
        }
    }

    /// A fully initialised `NOTIFYICONDATAW` for the hidden window and icon.
    fn notify_data(hwnd: Hwnd, icon: Hicon) -> NotifyIconDataW {
        let mut data = NotifyIconDataW {
            cb_size: core::mem::size_of::<NotifyIconDataW>() as u32,
            hwnd,
            u_id: 1,
            u_flags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            u_callback_message: TRAY_CALLBACK,
            h_icon: icon,
            sz_tip: [0; 128],
            dw_state: 0,
            dw_state_mask: 0,
            sz_info: [0; 256],
            u_version_or_timeout: 0,
            sz_info_title: [0; 64],
            dw_info_flags: 0,
            guid: Guid::default(),
            h_balloon_icon: ptr::null_mut(),
        };
        let tip = wide(TOOLTIP);
        data.sz_tip[..tip.len()].copy_from_slice(&tip);
        data
    }

    /// The hidden tray window: owns the shell icon and the message window.
    pub(super) struct TrayWindow {
        hwnd: Hwnd,
        icon: Hicon,
    }

    impl TrayWindow {
        /// Adds the icon back after the shell restarted.
        pub(super) fn reinstall_icon(&self) -> Result<(), String> {
            let mut data = notify_data(self.hwnd, self.icon);
            // SAFETY: the window and icon handles are still alive and owned
            // by `self`; Shell_NotifyIconW only reads `data`.
            if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } == 0 {
                return Err("Shell_NotifyIconW failed to re-add the tray icon".to_owned());
            }
            data.u_version_or_timeout = NOTIFYICON_VERSION_4;
            if unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) } == 0 {
                log::warn!(
                    "tray icon version 4 unavailable after reinstall; using legacy callbacks"
                );
            }
            Ok(())
        }
    }

    impl Drop for TrayWindow {
        fn drop(&mut self) {
            let data = notify_data(self.hwnd, self.icon);
            // SAFETY: the icon entry, window and icon handles are still
            // owned by this value and released exactly once.
            unsafe {
                let _removed = Shell_NotifyIconW(NIM_DELETE, &data);
                DestroyIcon(self.icon);
                DestroyWindow(self.hwnd);
            }
            let class_name = wide(CLASS_NAME);
            // SAFETY: the module handle never changes for this process.
            let instance = unsafe { GetModuleHandleW(ptr::null()) };
            // SAFETY: no window of the class remains, so it can go.
            unsafe {
                UnregisterClassW(class_name.as_ptr(), instance);
            }
        }
    }

    /// The parent that makes a window message-only.
    const HWND_MESSAGE: Hwnd = -3isize as Hwnd;

    /// The launcher's message-only window, when a launcher is running.
    ///
    /// The launcher's class and title come from the shared contract, so the
    /// two binaries cannot disagree about them.
    fn launcher_window() -> Option<Hwnd> {
        let class_name = wide(crate::launcher::contract::WINDOW_CLASS);
        let title = wide(crate::launcher::contract::WINDOW_TITLE);
        // SAFETY: both strings are null-terminated and live for the call; the
        // search only reads the window list.
        let window = unsafe {
            FindWindowExW(
                HWND_MESSAGE,
                ptr::null_mut(),
                class_name.as_ptr(),
                title.as_ptr(),
            )
        };
        (!window.is_null()).then_some(window)
    }

    /// Posts the settings request to the launcher.
    pub(super) fn open_settings() -> Result<(), String> {
        let window = launcher_window().ok_or_else(|| "The launcher is not running".to_owned())?;
        // SAFETY: the handle belongs to another process's live window;
        // posting only enqueues a message.
        let posted =
            unsafe { PostMessageW(window, crate::launcher::contract::WM_OPEN_SETTINGS, 0, 0) };
        if posted == 0 {
            return Err("PostMessageW failed to reach the launcher".to_owned());
        }
        Ok(())
    }

    /// Installs the hidden window and registers the icon with the shell.
    pub(super) fn install() -> Result<TrayWindow, String> {
        // SAFETY: a null name asks for the process module, which always
        // exists.
        let instance = unsafe { GetModuleHandleW(ptr::null()) };
        if instance.is_null() {
            return Err("GetModuleHandleW failed to find the process module".to_owned());
        }
        let class_name = wide(CLASS_NAME);
        register_window_class(&class_name, instance)?;
        let window_title = wide(WINDOW_TITLE);
        // SAFETY: the class was just registered from `class_name`; the
        // remaining parameters describe a hidden top-level popup with no
        // parent, menu or extra data. The name buffers outlive the call.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class_name.as_ptr(),
                window_title.as_ptr(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                ptr::null_mut(),
            )
        };
        if hwnd.is_null() {
            destroy_window(ptr::null_mut(), &class_name, instance);
            return Err("CreateWindowExW failed to create the tray window".to_owned());
        }
        let taskbar_created = {
            let name = wide("TaskbarCreated");
            // SAFETY: `name` is a null-terminated string buffer alive for the
            // call.
            unsafe { RegisterWindowMessageW(name.as_ptr()) }
        };
        if taskbar_created != 0 {
            TASKBAR_CREATED.store(taskbar_created, Ordering::SeqCst);
        } else {
            log::warn!(
                "RegisterWindowMessageW(TaskbarCreated) failed; tray reinstall is unavailable"
            );
        }
        let icon = match create_icon() {
            Ok(icon) => icon,
            Err(error) => {
                destroy_window(hwnd, &class_name, instance);
                return Err(error);
            }
        };
        let mut data = notify_data(hwnd, icon);
        // SAFETY: `data` is fully initialised; Shell_NotifyIconW only reads.
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } == 0 {
            // SAFETY: the icon and window are still owned here.
            unsafe {
                DestroyIcon(icon);
            }
            destroy_window(hwnd, &class_name, instance);
            return Err("Shell_NotifyIconW failed to add the tray icon".to_owned());
        }
        data.u_version_or_timeout = NOTIFYICON_VERSION_4;
        // A failed version negotiation only changes how the shell reports
        // events (the procedure below accepts both shapes), so warn.
        if unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) } == 0 {
            log::warn!("tray icon version 4 unavailable; using legacy callbacks");
        }
        Ok(TrayWindow { hwnd, icon })
    }

    /// Takes the next pending action, in priority order: toggle, quit,
    /// settings, reinstall.
    pub(super) fn take_action() -> Option<TrayAction> {
        const FLAGS: [(u32, TrayAction); 4] = [
            (ACTION_TOGGLE, TrayAction::ToggleControls),
            (ACTION_QUIT, TrayAction::Quit),
            (ACTION_SETTINGS, TrayAction::Settings),
            (ACTION_REINSTALL, TrayAction::Reinstall),
        ];
        for (flag, action) in FLAGS {
            if ACTIONS.fetch_and(!flag, Ordering::SeqCst) & flag != 0 {
                return Some(action);
            }
        }
        None
    }

    /// The hidden window's message procedure: records action bits for shell
    /// callbacks and shows the context menu on right click. Never posts a
    /// quit message, so it cannot interfere with the winit event loop.
    unsafe extern "system" fn window_proc(
        hwnd: Hwnd,
        message: u32,
        wparam: Wparam,
        lparam: Lparam,
    ) -> Lresult {
        if message == TRAY_CALLBACK {
            // Version 4 packs the event id into LOWORD(lParam); the version-0
            // shapes (WM_LBUTTONUP, WM_CONTEXTMENU, ...) read the same way.
            let event = (lparam as u32) & 0xFFFF;
            match event {
                NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    ACTIONS.fetch_or(ACTION_TOGGLE, Ordering::SeqCst);
                }
                WM_CONTEXTMENU | WM_RBUTTONUP => show_menu(hwnd),
                _ => {}
            }
            return 0;
        }
        let taskbar_created = TASKBAR_CREATED.load(Ordering::SeqCst);
        if message != 0 && message == taskbar_created {
            ACTIONS.fetch_or(ACTION_REINSTALL, Ordering::SeqCst);
            return 0;
        }
        // SAFETY: default handling for every other message; parameters are
        // forwarded unchanged.
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }

    /// Shows the tray context menu at the cursor and records the choice.
    fn show_menu(hwnd: Hwnd) {
        // SAFETY: every handle created here is also released here, and the
        // calls run on the window's own thread.
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            let show_hide = wide("Show/Hide controls");
            let quit = wide("Quit");
            AppendMenuW(menu, MF_STRING, MENU_SHOW_HIDE as usize, show_hide.as_ptr());
            // Only a launcher can show a settings page; a bare engine run
            // (acceptance scripts, `cargo run`) has nowhere to send it.
            if launcher_window().is_some() {
                let settings = wide("Settings...");
                AppendMenuW(menu, MF_STRING, MENU_SETTINGS as usize, settings.as_ptr());
            }
            AppendMenuW(menu, MF_STRING, MENU_QUIT as usize, quit.as_ptr());
            let mut point = Point { x: 0, y: 0 };
            if GetCursorPos(&mut point) == 0 {
                DestroyMenu(menu);
                return;
            }
            // The classic dance that dismisses the menu when the user clicks
            // elsewhere: foreground the owner first, then post a benign
            // message once the modal loop returns.
            SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                ptr::null(),
            );
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            match command {
                MENU_SHOW_HIDE => {
                    ACTIONS.fetch_or(ACTION_TOGGLE, Ordering::SeqCst);
                }
                MENU_SETTINGS => {
                    ACTIONS.fetch_or(ACTION_SETTINGS, Ordering::SeqCst);
                }
                MENU_QUIT => {
                    ACTIONS.fetch_or(ACTION_QUIT, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn notify_icon_data_layout_matches_the_win32_struct() {
            assert_eq!(core::mem::size_of::<NotifyIconDataW>(), 976);
            assert_eq!(core::mem::offset_of!(NotifyIconDataW, hwnd), 8);
            assert_eq!(
                core::mem::offset_of!(NotifyIconDataW, u_callback_message),
                24
            );
            assert_eq!(core::mem::offset_of!(NotifyIconDataW, sz_tip), 40);
            assert_eq!(core::mem::offset_of!(NotifyIconDataW, sz_info), 304);
            assert_eq!(
                core::mem::offset_of!(NotifyIconDataW, u_version_or_timeout),
                816
            );
            assert_eq!(core::mem::offset_of!(NotifyIconDataW, guid), 952);
            assert_eq!(core::mem::offset_of!(NotifyIconDataW, h_balloon_icon), 968);
            assert_eq!(core::mem::size_of::<WndClassExW>(), 80);
        }

        #[test]
        fn icon_pixels_are_a_face_with_eyes_out_of_transparency() {
            let pixels = icon_pixels();
            let pixel = |x: i32, y: i32| {
                let offset = ((y * ICON_SIZE + x) * 4) as usize;
                [
                    pixels[offset],
                    pixels[offset + 1],
                    pixels[offset + 2],
                    pixels[offset + 3],
                ]
            };
            // Corners and points outside the r=12 circle are transparent.
            assert_eq!(pixel(0, 0)[3], 0);
            assert_eq!(pixel(31, 31)[3], 0);
            assert_eq!(pixel(16, 1)[3], 0);
            // The centre and an off-centre point are face pixels.
            assert_eq!(pixel(16, 16), FACE_COLOR);
            assert_eq!(pixel(16, 5), FACE_COLOR);
            // Both eyes are dark.
            assert_eq!(pixel(11, 13), EYE_COLOR);
            assert_eq!(pixel(20, 13), EYE_COLOR);
        }

        #[test]
        fn tray_window_class_registers_and_unregisters() {
            // SAFETY: a null name asks for the process module.
            let instance = unsafe { GetModuleHandleW(ptr::null()) };
            assert!(!instance.is_null());
            let name = wide(&format!("{CLASS_NAME}Test{}", std::process::id()));
            register_window_class(&name, instance).unwrap();
            let title = wide(WINDOW_TITLE);
            // SAFETY: the class is registered from `name`; everything else is
            // null or zero, and the buffers outlive the call.
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    name.as_ptr(),
                    title.as_ptr(),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    instance,
                    ptr::null_mut(),
                )
            };
            assert!(!hwnd.is_null());
            // SAFETY: `hwnd` was just created above.
            assert_eq!(unsafe { DestroyWindow(hwnd) }, 1);
            // SAFETY: the only window of the class is gone.
            assert_eq!(unsafe { UnregisterClassW(name.as_ptr(), instance) }, 1);
        }

        #[test]
        fn take_action_reports_and_clears_in_priority_order() {
            ACTIONS.store(
                ACTION_TOGGLE | ACTION_QUIT | ACTION_SETTINGS | ACTION_REINSTALL,
                Ordering::SeqCst,
            );
            assert_eq!(take_action(), Some(TrayAction::ToggleControls));
            assert_eq!(take_action(), Some(TrayAction::Quit));
            assert_eq!(take_action(), Some(TrayAction::Settings));
            assert_eq!(take_action(), Some(TrayAction::Reinstall));
            assert_eq!(take_action(), None);
            ACTIONS.store(ACTION_QUIT, Ordering::SeqCst);
            assert_eq!(take_action(), Some(TrayAction::Quit));
            assert_eq!(take_action(), None);
        }
    }
}
