//! Desktop-span scene geometry: the scene window covers the primary monitor
//! minus a small gap on the taskbar's edge.
//!
//! A window whose rect is exactly the monitor rectangle is a fullscreen
//! window to the shell, and while one exists the shell suppresses the
//! auto-hide taskbar reveal. Measured 2026-10-16 on the target machine: with
//! the scene covering the monitor exactly, a pointer dwell on the taskbar
//! edge never revealed it; shrinking the window by one pixel un-suppressed
//! the reveal. The scene therefore always leaves [`TASKBAR_GAP_PX`] physical
//! pixels uncovered on the edge the main taskbar is docked to, which also
//! hides the exposed strip behind the taskbar itself whenever the taskbar is
//! visible.

use winit::dpi::{PhysicalPosition, PhysicalSize};

/// Screen edge the main taskbar is docked to (`ABE_LEFT`..`ABE_BOTTOM`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskbarEdge {
    /// The taskbar is docked to the left edge.
    Left,
    /// The taskbar is docked to the top edge.
    Top,
    /// The taskbar is docked to the right edge.
    Right,
    /// The taskbar is docked to the bottom edge.
    Bottom,
}

/// Physical-pixel strip the desktop-span scene leaves uncovered on the
/// taskbar's edge.
///
/// Any deviation from the exact monitor rect keeps the shell from treating
/// the scene as a fullscreen window; two pixels leave a margin under
/// fractional DPI rounding without a visible seam.
pub const TASKBAR_GAP_PX: u32 = 2;

/// The scene window rect for desktop-span mode: the monitor rect with `gap`
/// physical pixels left uncovered on `edge`.
pub fn desktop_scene_rect(
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    edge: TaskbarEdge,
    gap: u32,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let gap = gap.min(i32::MAX as u32);
    let (mut x, mut y, mut width, mut height) = (position.x, position.y, size.width, size.height);
    match edge {
        TaskbarEdge::Left => {
            x = x.saturating_add(gap as i32);
            width = width.saturating_sub(gap);
        }
        TaskbarEdge::Top => {
            y = y.saturating_add(gap as i32);
            height = height.saturating_sub(gap);
        }
        TaskbarEdge::Right => width = width.saturating_sub(gap),
        TaskbarEdge::Bottom => height = height.saturating_sub(gap),
    }
    (
        PhysicalPosition::new(x, y),
        PhysicalSize::new(width, height),
    )
}

/// The edge the main taskbar is docked to, queried from the shell
/// (`SHAppBarMessage(ABM_GETTASKBARPOS)`).
///
/// Returns `None` when the query fails or reports an edge this module does
/// not know; the caller substitutes its documented default.
#[cfg(target_os = "windows")]
pub fn taskbar_edge() -> Option<TaskbarEdge> {
    win32::taskbar_edge()
}

/// Windows-only API; `None` everywhere else, matching the scene's
/// Windows-only desktop mode.
#[cfg(not(target_os = "windows"))]
pub fn taskbar_edge() -> Option<TaskbarEdge> {
    None
}

#[cfg(target_os = "windows")]
mod win32 {
    use core::ffi::c_void;

    use super::TaskbarEdge;

    /// `SHAppBarMessage(ABM_GETTASKBARPOS)`: position of the main taskbar.
    const ABM_GETTASKBARPOS: u32 = 5;

    /// Win32 `RECT`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    /// Win32 `APPBARDATA` (x64 layout; asserted by the layout test below).
    #[repr(C)]
    struct AppBarData {
        cb_size: u32,
        h_wnd: *mut c_void,
        u_callback_message: u32,
        u_edge: u32,
        rc: Rect,
        l_param: isize,
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHAppBarMessage(message: u32, data: *mut AppBarData) -> usize;
    }

    pub(super) fn taskbar_edge() -> Option<TaskbarEdge> {
        let mut data = AppBarData {
            cb_size: core::mem::size_of::<AppBarData>() as u32,
            h_wnd: core::ptr::null_mut(),
            u_callback_message: 0,
            u_edge: 0,
            rc: Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            l_param: 0,
        };
        // SAFETY: `data.cb_size` describes this live struct exactly and the
        // call only reads and fills it; no pointer field is dereferenced by
        // the shell for this message.
        let result = unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) };
        if result == 0 {
            return None;
        }
        match data.u_edge {
            0 => Some(TaskbarEdge::Left),
            1 => Some(TaskbarEdge::Top),
            2 => Some(TaskbarEdge::Right),
            3 => Some(TaskbarEdge::Bottom),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use core::mem::{offset_of, size_of};

        #[test]
        fn app_bar_data_matches_the_win32_x64_layout() {
            assert_eq!(size_of::<super::AppBarData>(), 48);
            assert_eq!(offset_of!(super::AppBarData, cb_size), 0);
            assert_eq!(offset_of!(super::AppBarData, h_wnd), 8);
            assert_eq!(offset_of!(super::AppBarData, u_callback_message), 16);
            assert_eq!(offset_of!(super::AppBarData, u_edge), 20);
            assert_eq!(offset_of!(super::AppBarData, rc), 24);
            assert_eq!(offset_of!(super::AppBarData, l_param), 40);
        }
    }
}

#[cfg(test)]
mod tests {
    use winit::dpi::{PhysicalPosition, PhysicalSize};

    use super::{TASKBAR_GAP_PX, TaskbarEdge, desktop_scene_rect};

    #[test]
    fn the_gap_lands_on_the_taskbar_edge_alone() {
        let position = PhysicalPosition::new(10, 20);
        let size = PhysicalSize::new(100, 200);
        assert_eq!(
            desktop_scene_rect(position, size, TaskbarEdge::Left, TASKBAR_GAP_PX),
            (PhysicalPosition::new(12, 20), PhysicalSize::new(98, 200))
        );
        assert_eq!(
            desktop_scene_rect(position, size, TaskbarEdge::Top, TASKBAR_GAP_PX),
            (PhysicalPosition::new(10, 22), PhysicalSize::new(100, 198))
        );
        assert_eq!(
            desktop_scene_rect(position, size, TaskbarEdge::Right, TASKBAR_GAP_PX),
            (PhysicalPosition::new(10, 20), PhysicalSize::new(98, 200))
        );
        assert_eq!(
            desktop_scene_rect(position, size, TaskbarEdge::Bottom, TASKBAR_GAP_PX),
            (PhysicalPosition::new(10, 20), PhysicalSize::new(100, 198))
        );
    }

    #[test]
    fn a_zero_gap_is_the_monitor_rect_everywhere() {
        let position = PhysicalPosition::new(-100, 40);
        let size = PhysicalSize::new(320, 240);
        for edge in [
            TaskbarEdge::Left,
            TaskbarEdge::Top,
            TaskbarEdge::Right,
            TaskbarEdge::Bottom,
        ] {
            assert_eq!(
                desktop_scene_rect(position, size, edge, 0),
                (position, size)
            );
        }
    }

    #[test]
    fn an_oversized_gap_saturates_instead_of_wrapping() {
        let position = PhysicalPosition::new(0, 0);
        let size = PhysicalSize::new(3, 3);
        assert_eq!(
            desktop_scene_rect(position, size, TaskbarEdge::Bottom, u32::MAX),
            (PhysicalPosition::new(0, 0), PhysicalSize::new(3, 0))
        );
        assert_eq!(
            desktop_scene_rect(position, size, TaskbarEdge::Left, 5),
            (PhysicalPosition::new(5, 0), PhysicalSize::new(0, 3))
        );
    }
}
