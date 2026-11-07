//! Keeps the window out of screenshots, screen sharing and Windows Recall, as Signal Desktop's
//! Screen security does (<https://signal.org/blog/signal-doesnt-recall/>), and as the Slint
//! window does (`b33p-slint/src/capture.rs`).

use gpui_kit::Window;

/// Once the window is open: before that it has no native window. Does nothing off Windows.
#[cfg(windows)]
pub fn exclude(window: &Window) {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };
    let handle = match window.window_handle() {
        Ok(handle) => handle.as_raw(),
        Err(error) => {
            tracing::warn!(%error, "no native window to exclude from screen capture");
            return;
        }
    };
    let RawWindowHandle::Win32(win32) = handle else {
        tracing::warn!("not a Win32 window: not excluded from screen capture");
        return;
    };
    let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
    // SAFETY: `hwnd` is the handle of the window that was just given, which this thread (the
    // one that owns it) is still holding.
    if let Err(error) = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
        tracing::warn!(%error, "window not excluded from screen capture");
    }
}

#[cfg(not(windows))]
pub fn exclude(_: &Window) {}
