//! Keeps the window out of screenshots, screen sharing and Windows Recall, as Signal Desktop's
//! Screen security does (<https://signal.org/blog/signal-doesnt-recall/>).

/// Once the window is shown: until then it has no native window.
#[cfg(windows)]
pub fn exclude(window: &slint::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };
    let handle = match window.window_handle().window_handle() {
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
    if let Err(error) = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
        tracing::warn!(%error, "window not excluded from screen capture");
    }
}

#[cfg(not(windows))]
pub fn exclude(_: &slint::Window) {}
