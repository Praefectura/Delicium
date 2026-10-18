//! Single-instance control and the message-only window.
//!
//! Only one launcher may own an installation at a time: a second double
//! click must not start a second pet, and an update must not run twice
//! against the same `state.json`. Ownership is a named mutex under the
//! `Local\` namespace (per session, no privilege needed). A second launcher
//! that has something to ask finds the first one's message window and posts
//! to it.

use std::path::Path;

use super::contract;
use super::win32::wide;
use super::win32::{self, Hinstance, Hmutex, Hwnd};

/// Owns the single-instance mutex until dropped.
pub struct InstanceGuard(Hmutex);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `claim` and is closed once.
        unsafe {
            win32::CloseHandle(self.0);
        }
    }
}

/// Claims the installation. `Ok(None)` means another launcher already
/// holds it.
pub fn claim(key: &str) -> Result<Option<InstanceGuard>, String> {
    let name = wide(&format!("Local\\Delicium.Launcher.{key}"));
    // SAFETY: the name is NUL-terminated and outlives the call; a null
    // security descriptor means the default one.
    let handle = unsafe { win32::CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        // SAFETY: the call just failed and set the thread error slot.
        let error = unsafe { win32::GetLastError() };
        return Err(format!("CreateMutexW failed with error {error}"));
    }
    // SAFETY: the call just ran; the error slot is read immediately.
    let already = unsafe { win32::GetLastError() == win32::ERROR_ALREADY_EXISTS };
    if already {
        // SAFETY: the handle belongs to this call and is not kept.
        unsafe {
            win32::CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(InstanceGuard(handle)))
}

/// A window procedure for the message-only window.
pub type WindowProc =
    unsafe extern "system" fn(Hwnd, u32, win32::Wparam, win32::Lparam) -> win32::Lresult;

/// Registers the launcher window class and creates its message-only window.
///
/// A message-only window (`HWND_MESSAGE` parent) is never shown, never
/// appears in the taskbar or in Alt-Tab, and receives posted messages
/// exactly like a normal window. The class is unregistered when the process
/// ends, which the system does anyway.
pub fn create_message_window(proc: WindowProc) -> Result<Hwnd, String> {
    let instance: Hinstance = win32::module();
    if instance.is_null() {
        return Err("the process module handle is unavailable".to_owned());
    }
    let class_name = wide(contract::WINDOW_CLASS);
    let class = win32::WndClassExW {
        cb_size: core::mem::size_of::<win32::WndClassExW>() as u32,
        style: 0,
        lpfn_wnd_proc: Some(proc),
        cb_cls_extra: 0,
        cb_wnd_extra: 0,
        h_instance: instance,
        h_icon: std::ptr::null_mut(),
        h_cursor: std::ptr::null_mut(),
        hbr_background: std::ptr::null_mut(),
        lpsz_menu_name: std::ptr::null(),
        lpsz_class_name: class_name.as_ptr(),
        h_icon_sm: std::ptr::null_mut(),
    };
    // SAFETY: the class and its name are fully initialised and outlive the
    // call; the procedure matches the declared signature.
    if unsafe { win32::RegisterClassExW(&class) } == 0 {
        // SAFETY: the registration attempt just failed.
        let error = unsafe { win32::GetLastError() };
        return Err(format!("RegisterClassExW failed with error {error}"));
    }
    let title = wide(contract::WINDOW_TITLE);
    // SAFETY: the class is registered; the parent is the message-only
    // pseudo-window; every pointer is either null or a live buffer.
    let window = unsafe {
        win32::CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            win32::WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            win32::HWND_MESSAGE,
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        )
    };
    if window.is_null() {
        // SAFETY: the creation attempt just failed.
        let error = unsafe { win32::GetLastError() };
        // SAFETY: no window of the class exists now.
        unsafe {
            win32::UnregisterClassW(class_name.as_ptr(), instance);
        }
        return Err(format!("CreateWindowExW failed with error {error}"));
    }
    Ok(window)
}

/// Removes the message-only window and its class.
///
/// # Safety
///
/// `window` must be a live window of this process created by
/// [`create_message_window`], and no other window of its class may remain.
pub unsafe fn destroy_message_window(window: Hwnd) {
    if window.is_null() {
        return;
    }
    let class_name = wide(contract::WINDOW_CLASS);
    let instance = win32::module();
    // SAFETY: the window belongs to this process; the class name outlives
    // the call and no other window of the class remains.
    unsafe {
        win32::DestroyWindow(window);
        win32::UnregisterClassW(class_name.as_ptr(), instance);
    }
}

/// The first launcher's message window, when one exists.
pub fn find_primary() -> Option<Hwnd> {
    let class_name = wide(contract::WINDOW_CLASS);
    let title = wide(contract::WINDOW_TITLE);
    // SAFETY: both strings are NUL-terminated and live for the call.
    let window = unsafe {
        win32::FindWindowExW(
            win32::HWND_MESSAGE,
            std::ptr::null_mut(),
            class_name.as_ptr(),
            title.as_ptr(),
        )
    };
    (!window.is_null()).then_some(window)
}

/// Posts [`contract::WM_WAKE`] to the first launcher.
pub fn wake_primary(open_settings: bool) -> Result<(), String> {
    let Some(window) = find_primary() else {
        return Ok(());
    };
    let wparam = if open_settings {
        contract::WAKE_SETTINGS
    } else {
        0
    };
    // SAFETY: the window handle was just found and belongs to another
    // process; `PostMessageW` only enqueues a message.
    if unsafe { win32::PostMessageW(window, contract::WM_WAKE, wparam, 0) } == 0 {
        // SAFETY: the post just failed.
        let error = unsafe { win32::GetLastError() };
        return Err(format!("PostMessageW failed with error {error}"));
    }
    Ok(())
}

/// The launcher's own path, for the environment it hands to the engine.
pub fn launcher_path(root: &Path) -> std::path::PathBuf {
    super::paths::launcher_exe(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_is_exclusive_within_the_process() {
        let key = format!("test{}", std::process::id());
        let first = claim(&key).unwrap();
        assert!(first.is_some());
        // A second claim on the same name reports the existing owner.
        assert!(claim(&key).unwrap().is_none());
        drop(first);
        // After the owner goes away the name is free again.
        assert!(claim(&key).unwrap().is_some());
    }
}
