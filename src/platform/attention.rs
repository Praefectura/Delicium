//! Read-only desktop attention queries: the global cursor position, monitor
//! work areas, and other applications' visible top-level window rectangles.
//!
//! The interaction layer only observes. Nothing here activates, moves,
//! resizes, captures or manipulates a window, reads window titles, classes or
//! content, installs hooks, or touches another process. Every failure path
//! reports no data (or skips the window), so optional attention state simply
//! clears; hit testing and fallback authorization never depend on these
//! results.
//!
//! All coordinates are physical screen pixels, the coordinate space of
//! [`super::DesktopRect`].

/// The global cursor position in physical screen coordinates, read once with
/// `GetCursorPos`.
///
/// Read-only: the cursor is never moved, clipped or captured. `None` when the
/// query fails or on a platform without Windows, so callers keep cursor
/// attention cleared.
pub fn cursor_position() -> Option<[i32; 2]> {
    #[cfg(target_os = "windows")]
    {
        win32::cursor_position()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Every monitor's work area - the monitor rectangle minus taskbars and other
/// reserved appbar space, as the shell reports it - in physical screen
/// coordinates.
///
/// At most 64 monitors are visited and at most 32 work areas are returned;
/// degenerate or unrepresentable rectangles are skipped. `None` when the
/// enumeration or `GetMonitorInfoW` fails, when no work area survives, or on
/// a platform without Windows.
pub fn work_area_rects() -> Option<Vec<super::DesktopRect>> {
    #[cfg(target_os = "windows")]
    {
        win32::work_area_rects()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// The rectangles of other applications' visible top-level windows, in
/// physical screen coordinates.
///
/// Windows owned by this process (the control window and the scene overlay),
/// invisible, minimized and DWM-cloaked windows, windows whose process id,
/// cloak state or rectangle cannot be read, rectangles below 32 pixels on
/// either side, and rectangles that do not intersect `desktop` are excluded.
/// At most 256 windows are visited and at most 32 rectangles are returned,
/// sorted by `(left, top, right, bottom)` with duplicates removed. Empty on
/// enumeration failure, when `desktop` is itself degenerate, or on a platform
/// without Windows.
///
/// Read-only: no window is activated, queried for title, class or content,
/// moved or otherwise changed.
pub fn visible_window_rects(desktop: super::DesktopRect) -> Vec<super::DesktopRect> {
    #[cfg(target_os = "windows")]
    {
        win32::visible_window_rects(desktop)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = desktop;
        Vec::new()
    }
}

/// Minimal hand-written Win32 FFI for the read-only desktop queries,
/// deliberately free of new dependencies. Every call reads only: window
/// handles are enumerated and queried, never activated, moved or modified.
#[cfg(target_os = "windows")]
mod win32 {
    use core::ffi::c_void;

    use super::super::DesktopRect;

    /// A window handle (Win32 `HWND`).
    type Hwnd = *mut c_void;
    /// A monitor handle (Win32 `HMONITOR`).
    type Hmonitor = *mut c_void;
    /// A device-context handle (Win32 `HDC`); only ever passed as null.
    type Hdc = *mut c_void;
    /// A Win32 `BOOL`.
    type Bool = i32;
    /// A Win32 `LPARAM`.
    type Lparam = isize;
    /// A monitor enumeration callback (Win32 `MONITORENUMPROC`).
    type MonitorEnumProc = unsafe extern "system" fn(Hmonitor, Hdc, *mut Rect, Lparam) -> Bool;
    /// A window enumeration callback (Win32 `WNDENUMPROC`).
    type WindowEnumProc = unsafe extern "system" fn(Hwnd, Lparam) -> Bool;

    /// Monitors visited by one work-area query.
    const WORK_AREA_MAX_VISITS: usize = 64;
    /// Work areas returned by one query.
    const WORK_AREA_MAX_RECTS: usize = 32;
    /// Top-level windows visited by one window query.
    const WINDOW_MAX_VISITS: usize = 256;
    /// Window rectangles returned by one query.
    const WINDOW_MAX_RECTS: usize = 32;
    /// Windows narrower or shorter than this are not reported as edges.
    const MIN_WINDOW_EXTENT: i64 = 32;
    /// `DWMWA_CLOAKED`: nonzero for windows DWM hides (other virtual
    /// desktops, suspended shell surfaces).
    const DWMWA_CLOAKED: u32 = 14;

    /// The Win32 `POINT`: two `LONG` (`i32`) coordinates.
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    /// The Win32 `RECT`; `right` and `bottom` are exclusive.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    /// The Win32 `MONITORINFO` (the `W` functions share this layout).
    #[repr(C)]
    struct MonitorInfoW {
        cb_size: u32,
        rc_monitor: Rect,
        rc_work: Rect,
        flags: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetCursorPos(point: *mut Point) -> Bool;
        fn EnumDisplayMonitors(
            dc: Hdc,
            clip: *const Rect,
            callback: Option<MonitorEnumProc>,
            param: Lparam,
        ) -> Bool;
        fn GetMonitorInfoW(monitor: Hmonitor, info: *mut MonitorInfoW) -> Bool;
        fn EnumWindows(callback: Option<WindowEnumProc>, param: Lparam) -> Bool;
        fn IsWindowVisible(window: Hwnd) -> Bool;
        fn IsIconic(window: Hwnd) -> Bool;
        fn GetWindowThreadProcessId(window: Hwnd, process_id: *mut u32) -> u32;
        fn GetWindowRect(window: Hwnd, rect: *mut Rect) -> Bool;
    }

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmGetWindowAttribute(
            window: Hwnd,
            attribute: u32,
            value: *mut c_void,
            size: u32,
        ) -> i32;
    }

    /// `rect` when it has a positive, `i32`-representable width and height.
    /// Every edge is already an `i32`; the subtraction is done in `i64` so a
    /// hostile rectangle cannot overflow.
    fn validated(rect: DesktopRect) -> Option<DesktopRect> {
        let width = i64::from(rect.right) - i64::from(rect.left);
        let height = i64::from(rect.bottom) - i64::from(rect.top);
        let limit = i64::from(i32::MAX);
        if width > 0 && height > 0 && width <= limit && height <= limit {
            Some(rect)
        } else {
            None
        }
    }

    /// Whether two validated rectangles overlap with a nonzero area. Edges
    /// that only touch do not count.
    fn intersects(a: DesktopRect, b: DesktopRect) -> bool {
        i64::from(a.left) < i64::from(b.right)
            && i64::from(a.right) > i64::from(b.left)
            && i64::from(a.top) < i64::from(b.bottom)
            && i64::from(a.bottom) > i64::from(b.top)
    }

    pub(super) fn cursor_position() -> Option<[i32; 2]> {
        let mut point = Point { x: 0, y: 0 };
        // SAFETY: `point` is a live POINT buffer that the call initializes;
        // GetCursorPos only reads the cursor state.
        let found = unsafe { GetCursorPos(&mut point) };
        if found == 0 {
            None
        } else {
            Some([point.x, point.y])
        }
    }

    /// Enumeration state shared with [`work_area_visit`] through `LPARAM`.
    struct WorkAreaSink {
        rects: Vec<DesktopRect>,
        visits: usize,
        /// The callback stopped the enumeration early.
        stopped: bool,
        /// A monitor could not be queried; the whole query fails.
        failed: bool,
    }

    pub(super) fn work_area_rects() -> Option<Vec<DesktopRect>> {
        let mut sink = WorkAreaSink {
            rects: Vec::new(),
            visits: 0,
            stopped: false,
            failed: false,
        };
        // The callback never allocates: the output vector is pre-reserved to
        // its cap and every push below is gated on `len() < cap`.
        if sink.rects.try_reserve_exact(WORK_AREA_MAX_RECTS).is_err() {
            return None;
        }
        // SAFETY: `work_area_visit` is the documented monitor enumeration
        // callback; `sink` outlives the synchronous enumeration and is its
        // only context pointer. A null DC and clip rectangle enumerate every
        // virtual monitor.
        let enumerated = unsafe {
            EnumDisplayMonitors(
                core::ptr::null_mut(),
                core::ptr::null(),
                Some(work_area_visit),
                &mut sink as *mut WorkAreaSink as isize,
            )
        };
        if sink.failed || sink.rects.is_empty() || (enumerated == 0 && !sink.stopped) {
            return None;
        }
        Some(sink.rects)
    }

    /// Records one monitor's work area. A callback cannot unwind: every path
    /// here returns a `BOOL`, and the only allocations happen in
    /// [`work_area_rects`] before enumeration starts.
    unsafe extern "system" fn work_area_visit(
        monitor: Hmonitor,
        _dc: Hdc,
        _clip: *mut Rect,
        param: Lparam,
    ) -> Bool {
        if param == 0 {
            return 0;
        }
        // SAFETY: `param` is the `&mut WorkAreaSink` passed by
        // `work_area_rects`; the enumeration is synchronous on this thread,
        // so the borrow is unique and live for the whole callback.
        let sink = unsafe { &mut *(param as *mut WorkAreaSink) };
        if sink.visits >= WORK_AREA_MAX_VISITS || sink.rects.len() >= WORK_AREA_MAX_RECTS {
            sink.stopped = true;
            return 0;
        }
        sink.visits += 1;
        let mut info = MonitorInfoW {
            cb_size: core::mem::size_of::<MonitorInfoW>() as u32,
            rc_monitor: Rect::default(),
            rc_work: Rect::default(),
            flags: 0,
        };
        // SAFETY: `monitor` is the handle this enumeration callback was
        // given, `info` is a live, fully initialized MONITORINFO and its size
        // is set; GetMonitorInfoW only reads the handle and fills the struct.
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            sink.failed = true;
            sink.stopped = true;
            return 0;
        }
        if let Some(rect) = validated(DesktopRect {
            left: info.rc_work.left,
            top: info.rc_work.top,
            right: info.rc_work.right,
            bottom: info.rc_work.bottom,
        }) {
            sink.rects.push(rect);
        }
        1
    }

    /// Enumeration state shared with [`visible_window_visit`] through
    /// `LPARAM`.
    struct WindowSink {
        rects: Vec<DesktopRect>,
        visits: usize,
        /// The callback stopped the enumeration early.
        stopped: bool,
        own_pid: u32,
        desktop: DesktopRect,
    }

    pub(super) fn visible_window_rects(desktop: DesktopRect) -> Vec<DesktopRect> {
        let Some(desktop) = validated(desktop) else {
            return Vec::new();
        };
        let mut sink = WindowSink {
            rects: Vec::new(),
            visits: 0,
            stopped: false,
            own_pid: std::process::id(),
            desktop,
        };
        // The callback never allocates: the output vector is pre-reserved to
        // its cap and every push below is gated on `len() < cap`.
        if sink.rects.try_reserve_exact(WINDOW_MAX_RECTS).is_err() {
            return Vec::new();
        }
        // SAFETY: `visible_window_visit` is the documented window enumeration
        // callback; `sink` outlives the synchronous enumeration and is its
        // only context pointer.
        let enumerated = unsafe {
            EnumWindows(
                Some(visible_window_visit),
                &mut sink as *mut WindowSink as isize,
            )
        };
        if enumerated == 0 && !sink.stopped {
            return Vec::new();
        }
        let mut rects = sink.rects;
        rects.sort_unstable_by_key(|rect| (rect.left, rect.top, rect.right, rect.bottom));
        rects.dedup();
        rects
    }

    /// Records one foreign top-level window's rectangle when it is visible,
    /// not minimized, not DWM-cloaked, at least 32 pixels on both sides, and
    /// intersects the desktop. A callback cannot unwind: every path returns a
    /// `BOOL`, and the only allocations happen in [`visible_window_rects`]
    /// before enumeration starts.
    unsafe extern "system" fn visible_window_visit(window: Hwnd, param: Lparam) -> Bool {
        if param == 0 {
            return 0;
        }
        // SAFETY: `param` is the `&mut WindowSink` passed by
        // `visible_window_rects`; the enumeration is synchronous on this
        // thread, so the borrow is unique and live for the whole callback.
        let sink = unsafe { &mut *(param as *mut WindowSink) };
        if sink.visits >= WINDOW_MAX_VISITS || sink.rects.len() >= WINDOW_MAX_RECTS {
            sink.stopped = true;
            return 0;
        }
        sink.visits += 1;
        let mut process_id = 0_u32;
        // SAFETY: `window` is the handle this callback was given and
        // `process_id` is a live out-parameter; the call only reads.
        let thread = unsafe { GetWindowThreadProcessId(window, &mut process_id) };
        if thread == 0 || process_id == sink.own_pid {
            return 1;
        }
        // SAFETY: read-only visibility and minimization queries on the handle
        // this callback was given.
        if unsafe { IsWindowVisible(window) } == 0 || unsafe { IsIconic(window) } != 0 {
            return 1;
        }
        let mut cloaked = 0_u32;
        // SAFETY: `cloaked` is a live out-parameter of the documented size;
        // the call only reads the window's composition state.
        let cloak_query = unsafe {
            DwmGetWindowAttribute(
                window,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut c_void,
                core::mem::size_of::<u32>() as u32,
            )
        };
        // A failed cloak query cannot classify the window; skip it rather than
        // report a surface that may be hidden on another virtual desktop.
        if cloak_query != 0 || cloaked != 0 {
            return 1;
        }
        let mut rect = Rect::default();
        // SAFETY: `rect` is a live RECT out-parameter; the call only reads.
        if unsafe { GetWindowRect(window, &mut rect) } == 0 {
            return 1;
        }
        let Some(rect) = validated(DesktopRect {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }) else {
            return 1;
        };
        let width = i64::from(rect.right) - i64::from(rect.left);
        let height = i64::from(rect.bottom) - i64::from(rect.top);
        if width < MIN_WINDOW_EXTENT || height < MIN_WINDOW_EXTENT {
            return 1;
        }
        if !intersects(rect, sink.desktop) {
            return 1;
        }
        sink.rects.push(rect);
        1
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn rectangle_validation_handles_negative_origins_and_extreme_extents() {
            let negative = DesktopRect {
                left: -1920,
                top: -511,
                right: 0,
                bottom: 1049,
            };
            assert_eq!(validated(negative), Some(negative));
            for invalid in [
                DesktopRect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 1,
                },
                DesktopRect {
                    left: 0,
                    top: 1,
                    right: 1,
                    bottom: 0,
                },
                DesktopRect {
                    left: i32::MIN,
                    top: 0,
                    right: i32::MAX,
                    bottom: 1,
                },
                DesktopRect {
                    left: 0,
                    top: i32::MIN,
                    right: 1,
                    bottom: i32::MAX,
                },
            ] {
                assert_eq!(validated(invalid), None);
            }
            let largest = DesktopRect {
                left: i32::MIN,
                top: 0,
                right: -1,
                bottom: i32::MAX,
            };
            assert_eq!(validated(largest), Some(largest));
        }

        #[test]
        fn only_positive_area_intersection_is_an_edge_candidate() {
            let desktop = DesktopRect {
                left: -1920,
                top: -511,
                right: 2560,
                bottom: 1600,
            };
            let inside = DesktopRect {
                left: -2000,
                top: -600,
                right: -1800,
                bottom: -400,
            };
            assert!(intersects(desktop, inside));
            let touching = DesktopRect {
                left: 2560,
                top: 0,
                right: 3000,
                bottom: 400,
            };
            assert!(!intersects(desktop, touching));
            let outside = DesktopRect {
                left: i32::MIN,
                top: -100,
                right: i32::MIN + 100,
                bottom: 100,
            };
            assert!(!intersects(desktop, outside));
        }
    }
}
