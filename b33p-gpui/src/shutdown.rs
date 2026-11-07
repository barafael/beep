//! Quitting properly when the system ends b33p rather than the user: at logout, shutdown or
//! `kill`. The backend has to close the database before the process ends, which only happens
//! once the event loop returns, so these ask it to return, the way closing the window does.
//!
//! On Unix that is `SIGTERM` (logout, `kill`), `SIGINT` (Ctrl+C) and `SIGHUP` (terminal
//! closed); on Windows `WM_QUERYENDSESSION` and `WM_ENDSESSION`, sent to every top-level window.

/// Keeps the watching going; dropping it, after the backend is closed, ends it.
pub struct Watch;

impl Watch {
    /// The backend is closed. On Windows an ending session waits for this.
    pub fn closed(self) {
        #[cfg(windows)]
        windows_session::closed();
    }
}

/// Starts watching. The receiver gets a message when the system asks b33p to quit. Failing to
/// start only means b33p can't end cleanly on these occasions.
pub fn watch() -> (Watch, async_channel::Receiver<()>) {
    let (asked, quit) = async_channel::bounded(1);
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;
        let mut signals = match Signals::new([SIGTERM, SIGINT, SIGHUP]) {
            Ok(signals) => signals,
            Err(error) => {
                tracing::warn!(%error, "b33p can't end cleanly when asked to by the system");
                return (Watch, quit);
            }
        };
        let spawned = std::thread::Builder::new()
            .name("b33p-shutdown".into())
            .spawn(move || {
                let mut signals = signals.forever();
                if let Some(signal) = signals.next() {
                    tracing::info!(signal, "asked to quit");
                    // Fails only once nobody listens: quitting already.
                    asked.try_send(()).ok();
                }
                // Asked again while closing: whoever asks means it.
                if let Some(signal) = signals.next() {
                    // Ends the process as the signal would have; if it can't, exit does.
                    signal_hook::low_level::emulate_default_handler(signal).ok();
                    std::process::exit(1);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "b33p can't end cleanly when asked to by the system");
        }
    }
    #[cfg(windows)]
    windows_session::watch(asked);
    #[cfg(not(any(unix, windows)))]
    let _ = asked;
    (Watch, quit)
}

/// The session-end messages of Windows, which `b33p-slint/src/shutdown.rs` hears the same way.
/// Written without a Windows machine to run it on: it is compile-checked only.
#[cfg(windows)]
mod windows_session {
    use std::sync::{Condvar, Mutex, OnceLock};
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
    /// Where the window procedure, which can't be given anything, tells the app to quit.
    static ASKED: OnceLock<async_channel::Sender<()>> = OnceLock::new();

    /// How long an ending session waits; Windows itself gives up on apps after about five
    /// seconds.
    const WAIT: Duration = Duration::from_secs(4);

    /// See [`super::Watch::closed`].
    pub fn closed() {
        *CLOSED.lock().unwrap_or_else(|e| e.into_inner()) = true;
        CLOSED_CHANGED.notify_all();
    }

    pub fn watch(asked: async_channel::Sender<()>) {
        if ASKED.set(asked).is_err() {
            return;
        }
        // A window of its own, never shown, on a thread of its own: the session's messages
        // reach it whatever the UI is doing. (A message-only window wouldn't get them.)
        let spawned = std::thread::Builder::new()
            .name("b33p-shutdown".into())
            .spawn(|| {
                if let Err(error) = unsafe { run() } {
                    tracing::warn!(%error, "b33p can't end cleanly when the session ends");
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "b33p can't end cleanly when the session ends");
        }
    }

    unsafe fn run() -> windows::core::Result<()> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: w!("b33p-shutdown"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(windows::core::Error::from_thread());
            }
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("b33p-shutdown"),
                w!("b33p"),
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
                // Says only whether the message was a key to translate.
                let _translated = TranslateMessage(&message);
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
                if let Some(asked) = ASKED.get() {
                    // Fails only once nobody listens: quitting already.
                    asked.try_send(()).ok();
                }
                let closed = CLOSED.lock().unwrap_or_else(|e| e.into_inner());
                let (_closed, waited) = CLOSED_CHANGED
                    .wait_timeout_while(closed, WAIT, |closed| !*closed)
                    .unwrap_or_else(|e| e.into_inner());
                if waited.timed_out() {
                    tracing::warn!("the session ends before b33p has closed");
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        }
    }
}
