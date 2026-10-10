//! Whether the desktop asks for reduced motion, now and as it changes: through the XDG
//! settings portal on Linux (GNOME's own setting where the portal has none), and the
//! system's accessibility settings on Windows and macOS. The UI's animations follow it unless
//! the user chose otherwise (`Settings::animations`).

/// Keeps the desktop's preference coming; dropping it stops that.
pub struct SystemMotion {
    _subscription: mundy::Subscription,
    /// Whether the desktop asks for reduced motion: once at the start, then on every change.
    pub reduced: async_channel::Receiver<bool>,
}

/// Starts listening. On macOS this has to be called on the main thread.
pub fn watch() -> SystemMotion {
    let (sender, reduced) = async_channel::unbounded();
    let subscription =
        mundy::Preferences::subscribe(mundy::Interest::ReducedMotion, move |preferences| {
            let _ = sender.try_send(preferences.reduced_motion == mundy::ReducedMotion::Reduce);
        });
    SystemMotion {
        _subscription: subscription,
        reduced,
    }
}
