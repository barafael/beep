//! Quitting properly when the system ends b33p rather than the user: at logout, shutdown or
//! `kill`. The backend has to close the database before the process ends, which only happens
//! once the event loop returns, so these ask it to return, the way closing the window does.
//!
//! On Unix that is `SIGTERM` (logout, `kill`), `SIGINT` (Ctrl+C) and `SIGHUP` (terminal
//! closed). Windows' session-end messages are not handled here yet.

/// Keeps the watching going; dropping it, after the backend is closed, ends it.
pub struct Watch;

impl Watch {
    /// The backend is closed.
    pub fn closed(self) {}
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
    #[cfg(not(unix))]
    let _ = asked;
    (Watch, quit)
}
