//! Keeps the window out of screenshots, screen sharing and Windows Recall, as Signal Desktop's
//! Screen security does (<https://signal.org/blog/signal-doesnt-recall/>). On unless the
//! setting is turned off; Windows only, the one system that lets a window ask for it.
//!
//! Slint makes the native window only once the event loop runs, after `show()` returns, so
//! asking right after `show()` found no window and the request was never made: the window was
//! capturable on every start. [`apply`] tries again until the window is there.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::generated::AppWindow;

/// Whether this system can keep a window out of captures.
pub const SUPPORTED: bool = cfg!(windows);

/// What the setting says, for the next [`apply`].
static EXCLUDE: AtomicBool = AtomicBool::new(true);

/// Takes the setting, for the window shown next and, given one, the window shown now.
pub fn set(exclude: bool, window: Option<&AppWindow>) {
    EXCLUDE.store(exclude, Ordering::Relaxed);
    if let Some(window) = window {
        apply(window);
    }
}

/// Asks for the window to be kept out of captures, or no longer, as the setting says; once
/// the window is shown.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn apply(window: &AppWindow) {
    #[cfg(windows)]
    platform::attempt(
        slint::ComponentHandle::as_weak(window),
        EXCLUDE.load(Ordering::Relaxed),
        platform::ATTEMPTS,
    );
}

#[cfg(windows)]
mod platform {
    use std::time::Duration;

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::ComponentHandle;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    };

    use crate::generated::AppWindow;

    /// Every 100 ms for five seconds: the window comes with the event loop's first turns.
    pub const ATTEMPTS: u32 = 50;
    const AGAIN: Duration = Duration::from_millis(100);

    pub fn attempt(window: slint::Weak<AppWindow>, exclude: bool, left: u32) {
        let Some(strong) = window.upgrade() else {
            return;
        };
        let Some(hwnd) = hwnd(strong.window()) else {
            if left == 0 {
                tracing::warn!("no native window: screen capture affinity not set");
            } else {
                slint::Timer::single_shot(AGAIN, move || attempt(window, exclude, left - 1));
            }
            return;
        };
        let affinity = if exclude {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        // SAFETY: the handle is the live window's own, asked for on the thread that made it.
        match unsafe { SetWindowDisplayAffinity(hwnd, affinity) } {
            Ok(()) => tracing::info!(exclude, "screen capture affinity set"),
            Err(error) => tracing::warn!(%error, exclude, "screen capture affinity not set"),
        }
    }

    fn hwnd(window: &slint::Window) -> Option<HWND> {
        let handle = window.window_handle().window_handle().ok()?.as_raw();
        let RawWindowHandle::Win32(win32) = handle else {
            return None;
        };
        Some(HWND(win32.hwnd.get() as *mut core::ffi::c_void))
    }
}
