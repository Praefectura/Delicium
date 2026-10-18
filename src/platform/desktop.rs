//! Physical desktop geometry and the shell's non-fullscreen overlay hint.
//!
//! The scene covers the bounding rectangle of the monitors, including any
//! gaps between them. `NonRudeHWND` tells the shell not to treat this overlay
//! as fullscreen, so covering a monitor does not suppress taskbar auto-hide.

use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::window::Window;

/// A nonempty monitor or desktop rectangle in physical screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl DesktopRect {
    pub fn from_physical(
        position: PhysicalPosition<i32>,
        size: PhysicalSize<u32>,
    ) -> Result<Self, String> {
        let right = i64::from(position.x) + i64::from(size.width);
        let bottom = i64::from(position.y) + i64::from(size.height);
        let rect = Self {
            left: position.x,
            top: position.y,
            right: i32::try_from(right).map_err(|_| "Desktop right coordinate is out of range")?,
            bottom: i32::try_from(bottom)
                .map_err(|_| "Desktop bottom coordinate is out of range")?,
        };
        rect.validate()?;
        Ok(rect)
    }

    fn validate(self) -> Result<(), String> {
        let width = i64::from(self.right) - i64::from(self.left);
        let height = i64::from(self.bottom) - i64::from(self.top);
        if width <= 0 || height <= 0 || width > i64::from(i32::MAX) || height > i64::from(i32::MAX)
        {
            return Err("Desktop rectangle has empty or unrepresentable dimensions".into());
        }
        Ok(())
    }

    pub fn position(self) -> PhysicalPosition<i32> {
        PhysicalPosition::new(self.left, self.top)
    }

    pub fn size(self) -> PhysicalSize<u32> {
        PhysicalSize::new(
            (i64::from(self.right) - i64::from(self.left)) as u32,
            (i64::from(self.bottom) - i64::from(self.top)) as u32,
        )
    }
}

/// The bounding rectangle, not the (possibly disconnected) visible union.
/// Invalid monitor rectangles and unrepresentable spans are rejected.
pub fn desktop_bounding_rect(monitors: &[DesktopRect]) -> Result<DesktopRect, String> {
    let mut bounds = *monitors.first().ok_or("No monitors in desktop layout")?;
    for &rect in monitors {
        rect.validate()?;
        bounds.left = bounds.left.min(rect.left);
        bounds.top = bounds.top.min(rect.top);
        bounds.right = bounds.right.max(rect.right);
        bounds.bottom = bounds.bottom.max(rect.bottom);
    }
    bounds.validate()?;
    Ok(bounds)
}

/// Set before the first ShowWindow: this permanently non-fullscreen overlay
/// must not make the shell lower the taskbar. Failure leaves the scene hidden.
#[cfg(target_os = "windows")]
pub fn mark_non_fullscreen(window: &Window) -> Result<(), String> {
    use core::ffi::c_void;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetPropW(window: *mut c_void, name: *const u16, value: *mut c_void) -> i32;
    }

    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("The desktop overlay is not a Win32 window".into());
    };
    let name: Vec<u16> = "NonRudeHWND\0".encode_utf16().collect();
    // SAFETY: the window borrow keeps HWND alive, the name is nul-terminated,
    // and the property value is the documented opaque HANDLE(TRUE), not a
    // pointer the shell dereferences. The property lives with the HWND.
    let result = unsafe {
        SetPropW(
            handle.hwnd.get() as *mut c_void,
            name.as_ptr(),
            std::ptr::without_provenance_mut::<c_void>(1),
        )
    };
    if result == 0 {
        return Err(format!(
            "Cannot mark the scene as non-fullscreen: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn mark_non_fullscreen(_window: &Window) -> Result<(), String> {
    Err("Desktop overlays require Windows".into())
}

#[cfg(test)]
mod tests {
    use super::{DesktopRect, desktop_bounding_rect};
    use winit::dpi::{PhysicalPosition, PhysicalSize};

    fn rect(x: i32, y: i32, w: u32, h: u32) -> DesktopRect {
        DesktopRect::from_physical(PhysicalPosition::new(x, y), PhysicalSize::new(w, h)).unwrap()
    }

    #[test]
    fn bounding_rect_preserves_single_monitor_and_negative_coordinates() {
        let monitor = rect(-1920, -200, 1920, 1080);
        assert_eq!(desktop_bounding_rect(&[monitor]).unwrap(), monitor);
        assert_eq!(monitor.position(), PhysicalPosition::new(-1920, -200));
        assert_eq!(monitor.size(), PhysicalSize::new(1920, 1080));
    }

    #[test]
    fn staggered_monitors_keep_their_full_bounding_rect() {
        let monitors = [rect(0, 0, 2560, 1600), rect(2560, -511, 1600, 2560)];
        let expected = rect(0, -511, 4160, 2560);
        assert_eq!(desktop_bounding_rect(&monitors).unwrap(), expected);
        assert_eq!(
            desktop_bounding_rect(&[monitors[1], monitors[0]]).unwrap(),
            expected
        );
    }

    #[test]
    fn disconnected_and_vertical_monitors_do_not_collapse_gaps() {
        assert_eq!(
            desktop_bounding_rect(&[rect(-800, 0, 640, 480), rect(0, -800, 640, 480)]).unwrap(),
            rect(-800, -800, 1440, 1280)
        );
    }

    #[test]
    fn empty_overflowing_and_invalid_rectangles_are_rejected() {
        assert!(desktop_bounding_rect(&[]).is_err());
        for (x, y, w, h) in [
            (0, 0, 0, 10),
            (0, 0, 10, 0),
            (i32::MAX, 0, 1, 10),
            (0, i32::MAX, 10, 1),
        ] {
            assert!(
                DesktopRect::from_physical(PhysicalPosition::new(x, y), PhysicalSize::new(w, h))
                    .is_err()
            );
        }
        assert!(desktop_bounding_rect(&[rect(i32::MIN, 0, 10, 10), rect(0, 0, 10, 10)]).is_err());
        assert!(
            desktop_bounding_rect(&[DesktopRect {
                left: 10,
                top: 0,
                right: 0,
                bottom: 10
            }])
            .is_err()
        );
    }
}
