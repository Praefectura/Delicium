use winit::window::{Window, WindowAttributes, WindowLevel};

pub fn overlay_attributes(attributes: WindowAttributes) -> WindowAttributes {
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::WindowAttributesExtWindows;
        // The scene overlay presents through a DirectComposition visual
        // (DX12 `DxgiFromVisual`). Without `WS_EX_NOREDIRECTIONBITMAP` the
        // composition content is drawn over an opaque, never-painted
        // redirection surface, which reads as a white backdrop instead of
        // the desktop.
        attributes
            .with_skip_taskbar(true)
            .with_no_redirection_bitmap(true)
    }
    #[cfg(not(target_os = "windows"))]
    {
        attributes
    }
}

pub fn raise_controls(control: &Window) {
    // Force a native z-order update even if the cached level is already topmost.
    control.set_window_level(WindowLevel::Normal);
    control.set_window_level(WindowLevel::AlwaysOnTop);
}

pub fn set_click_through(overlay: &Window, control: &Window, enabled: bool) -> Result<(), String> {
    overlay
        .set_cursor_hittest(!enabled)
        .map_err(|error| format!("Cannot change scene hit testing: {error}"))?;
    raise_controls(control);
    control.request_redraw();
    Ok(())
}
