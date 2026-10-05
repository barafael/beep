//! Quitting properly when the system ends langis rather than the user: at logout, shutdown or
//! `kill`. The backend has to close the database before the process ends, which only happens
//! once the event loop returns, so these ask it to return, the way "Quit" in the tray does.
//!
//! | Platform | How the system asks |
//! |---|---|
//! | Unix | `SIGTERM` (logout, `kill`), `SIGINT` (Ctrl+C), `SIGHUP` (terminal closed) |
//! | Windows | `WM_QUERYENDSESSION` and `WM_ENDSESSION` to every top-level window |

pub struct Watch(platform::Watch);

impl Watch {
    /// The backend is closed. On Windows an ending session waits for this.
    pub fn closed(self) {
        self.0.closed();
    }
}

/// Starts watching. Failing to only means langis can't end cleanly on these occasions.
pub fn watch() -> Watch {
    Watch(platform::watch())
}

#[cfg(unix)]
mod platform {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    pub struct Watch;

    impl Watch {
        pub fn closed(self) {}
    }

    pub fn watch() -> Watch {
        let mut signals = match Signals::new([SIGTERM, SIGINT, SIGHUP]) {
            Ok(signals) => signals,
            Err(error) => {
                tracing::warn!(%error, "langis can't end cleanly when asked to by the system");
                return Watch;
            }
        };
        let spawned = std::thread::Builder::new()
            .name("langis-shutdown".into())
            .spawn(move || {
                let mut signals = signals.forever();
                if let Some(signal) = signals.next() {
                    tracing::info!(signal, "asked to quit");
                    let _ = slint::quit_event_loop();
                }
                // Asked again while closing: whoever asks means it.
                if let Some(signal) = signals.next() {
                    let _ = signal_hook::low_level::emulate_default_handler(signal);
                    std::process::exit(1);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "langis can't end cleanly when asked to by the system");
        }
        Watch
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW,
        TranslateMessage, WINDOW_EX_STYLE, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW,
        WS_OVERLAPPED,
    };
    use windows::core::w;

    /// Whether the backend is closed, for an ending session waiting on it.
    static CLOSED: Mutex<bool> = Mutex::new(false);
    static CLOSED_CHANGED: Condvar = Condvar::new();

    /// How long an ending session waits; Windows itself gives up on apps after about five
    /// seconds.
    const WAIT: Duration = Duration::from_secs(4);

    pub struct Watch;

    impl Watch {
        pub fn closed(self) {
            *CLOSED.lock().unwrap_or_else(|e| e.into_inner()) = true;
            CLOSED_CHANGED.notify_all();
        }
    }

    pub fn watch() -> Watch {
        // A window of its own, never shown, on a thread of its own: the session's messages
        // reach it whatever the UI is doing. (A message-only window wouldn't get them.)
        let spawned = std::thread::Builder::new()
            .name("langis-shutdown".into())
            .spawn(|| {
                if let Err(error) = unsafe { run() } {
                    tracing::warn!(%error, "langis can't end cleanly when the session ends");
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "langis can't end cleanly when the session ends");
        }
        Watch
    }

    unsafe fn run() -> windows::core::Result<()> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: w!("langis-shutdown"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(windows::core::Error::from_thread());
            }
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("langis-shutdown"),
                w!("langis"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            // Nothing to save that would make the session wait.
            WM_QUERYENDSESSION => LRESULT(1),
            // The process ends as soon as this returns, so it returns once the backend is
            // closed.
            WM_ENDSESSION if wparam.0 != 0 => {
                tracing::info!("the session is ending");
                let _ = slint::quit_event_loop();
                let closed = CLOSED.lock().unwrap_or_else(|e| e.into_inner());
                let _ = CLOSED_CHANGED.wait_timeout_while(closed, WAIT, |closed| !*closed);
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    pub struct Watch;

    impl Watch {
        pub fn closed(self) {}
    }

    pub fn watch() -> Watch {
        Watch
    }
}
