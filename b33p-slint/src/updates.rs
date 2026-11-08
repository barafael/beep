//! "Check for updates": asks GitHub for a newer b33p once the chats are there, then once a
//! day, and offers it (a line above the chat list, and in Settings) with "Update and restart".
//! The checking, the signature and the swap are `b33p_core::update`'s.
//!
//! Only a Windows build with an update key (`B33P_UPDATE_PUBLIC_KEY` at build time), running
//! as the installed copy (`install.rs`), offers any of it; elsewhere the setting is hidden.
//! It is off unless chosen: the check is traffic b33p wouldn't make otherwise.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use crate::Shared;

/// How often a check is due, by the wall clock.
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the clock is looked at: a timer's clock stops while the computer sleeps, so a
/// day-long timer could take days.
const TICK: Duration = Duration::from_secs(60 * 60);

#[cfg(feature = "signal")]
use b33p_core::update::Offer;

/// Without the real backend there is nothing to update with.
#[cfg(not(feature = "signal"))]
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct Offer {
    version: String,
}

/// Whether this build, as it runs, can update itself; see the module's doc.
pub fn supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED
        .get_or_init(|| has_key() && cfg!(windows) && crate::install::installed_self().is_some())
}

fn has_key() -> bool {
    #[cfg(feature = "signal")]
    return b33p_core::update::PUBLIC_KEY.is_some();
    #[cfg(not(feature = "signal"))]
    false
}

/// Whether a check is due, `last` being when the last one succeeded.
fn due(last: Option<SystemTime>, now: SystemTime) -> bool {
    match last {
        None => true,
        // A clock set back counts as a day gone.
        Some(last) => now.duration_since(last).map_or(true, |since| since >= DAY),
    }
}

/// Asks GitHub; blocks.
fn check() -> Result<Option<Offer>, String> {
    #[cfg(feature = "signal")]
    return b33p_core::update::latest_blocking()
        .map_err(|error| b33p_core::Chain(&error).to_string());
    #[cfg(not(feature = "signal"))]
    Ok(None)
}

/// Downloads, checks and swaps in `offer`; blocks.
fn install(offer: &Offer) -> Result<(), String> {
    let installed = crate::install::installed_self()
        .ok_or_else(|| "this isn't the installed b33p".to_owned())?;
    #[cfg(feature = "signal")]
    return b33p_core::update::install_blocking(
        offer,
        &installed,
        &installed.with_file_name("b33p.exe.old"),
    )
    .map_err(|error| b33p_core::Chain(&error).to_string());
    #[cfg(not(feature = "signal"))]
    {
        drop((offer, installed));
        Err("this build can't update itself".into())
    }
}

/// What checking has found and is doing.
#[derive(Default)]
pub struct Updates {
    /// Checking has started: the chats were there once.
    started: Cell<bool>,
    /// When the last check succeeded.
    last_check: Cell<Option<SystemTime>>,
    checking: Cell<bool>,
    /// Downloading and swapping in.
    updating: Cell<bool>,
    offer: RefCell<Option<Offer>>,
    /// While downloading, or why the update failed.
    status: RefCell<String>,
    timer: slint::Timer,
}

impl Shared {
    /// The chats are there: the first time, checking starts (the network works, and the
    /// start-up load is over).
    pub(crate) fn chats_shown(&self) {
        if !supported() || self.updates.started.replace(true) {
            return;
        }
        self.check_for_update();
        let me = self.me.clone();
        self.updates
            .timer
            .start(slint::TimerMode::Repeated, TICK, move || {
                if let Some(shared) = me.upgrade() {
                    shared.check_for_update();
                }
            });
    }

    /// Asks GitHub, if the setting is on and a day has gone by since the last answer. A failed
    /// check is tried again at the next tick, an hour later.
    pub(crate) fn check_for_update(&self) {
        let updates = &self.updates;
        if !self.settings.borrow().check_updates {
            // Turned off: what was found isn't offered any more.
            if updates.offer.replace(None).is_some() {
                self.show_update();
            }
            return;
        }
        if !updates.started.get()
            || updates.checking.get()
            || updates.updating.get()
            || !due(updates.last_check.get(), SystemTime::now())
        {
            return;
        }
        updates.checking.set(true);
        let (sender, answer) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("b33p-update-check".into())
            .spawn(move || {
                // Only fails if b33p is ending.
                sender.send_blocking(check()).ok();
            });
        if let Err(error) = spawned {
            tracing::warn!(error = %b33p_core::scrub::chain(&error), "can't check for updates");
            updates.checking.set(false);
            return;
        }
        let me = self.me.clone();
        let received = slint::spawn_local(async move {
            let answer = answer.recv().await;
            let Some(shared) = me.upgrade() else { return };
            shared.updates.checking.set(false);
            match answer {
                Ok(Ok(offer)) => {
                    shared.updates.last_check.set(Some(SystemTime::now()));
                    match &offer {
                        Some(offer) => {
                            tracing::info!(version = offer.version, "an update is available")
                        }
                        None => tracing::debug!("b33p is up to date"),
                    }
                    shared.updates.offer.replace(offer);
                    shared.show_update();
                }
                Ok(Err(error)) => {
                    tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "checking for updates failed");
                }
                Err(_) => {}
            }
        });
        if let Err(error) = received {
            tracing::warn!(error = %b33p_core::scrub::chain(&error), "can't check for updates");
            updates.checking.set(false);
        }
    }

    /// "Update and restart": downloads and swaps in what was offered, then starts it.
    pub(crate) fn update_now(&self) {
        let updates = &self.updates;
        let Some(offer) = updates.offer.borrow().clone() else {
            return;
        };
        if updates.updating.replace(true) {
            return;
        }
        updates
            .status
            .replace(format!("Downloading b33p {}…", offer.version));
        self.show_update();
        let (sender, answer) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("b33p-update".into())
            .spawn(move || {
                sender.send_blocking(install(&offer)).ok();
            });
        if let Err(error) = spawned {
            self.update_failed(&b33p_core::Chain(&error).to_string());
            return;
        }
        let me = self.me.clone();
        let received = slint::spawn_local(async move {
            let answer = answer.recv().await;
            let Some(shared) = me.upgrade() else { return };
            match answer {
                Ok(Ok(())) => shared.restart(),
                Ok(Err(error)) => shared.update_failed(&error),
                Err(_) => {}
            }
        });
        if let Err(error) = received {
            self.update_failed(&b33p_core::Chain(&error).to_string());
        }
    }

    fn update_failed(&self, error: &str) {
        tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "updating failed");
        self.updates.updating.set(false);
        self.updates
            .status
            .replace(format!("The update failed ({error}). Try again later."));
        self.show_update();
    }

    /// Starts the new exe, which waits for this process to end (`--after`), and quits.
    fn restart(&self) {
        let started = std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe)
                .arg(AFTER)
                .arg(std::process::id().to_string())
                .spawn()
        });
        match started {
            Ok(_) => {
                tracing::info!("updated; restarting");
                // Fails only once the event loop has stopped: quitting already.
                slint::quit_event_loop().ok();
            }
            Err(error) => {
                // The new version is in place; it starts next time.
                self.update_failed(&format!(
                    "installed, but not restarted: {}",
                    b33p_core::Chain(&error)
                ));
            }
        }
    }

    fn show_update(&self) {
        let Some(ui) = self.ui() else { return };
        let offer = self.updates.offer.borrow();
        ui.show_update(
            offer.as_ref().map_or("", |offer| offer.version.as_str()),
            &self.updates.status.borrow(),
            self.updates.updating.get(),
        );
    }
}

/// `b33p --after <pid>`: started by an update, to wait for the process that started it.
pub const AFTER: &str = "--after";

/// Waits, a little, for the b33p that started this one for an update to end, so that its
/// single-instance lock and database are free.
pub fn wait_for_previous() {
    let mut args = std::env::args().skip_while(|arg| arg != AFTER).skip(1);
    let Some(pid) = args.next().and_then(|pid| pid.parse::<u32>().ok()) else {
        return;
    };
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        // SAFETY: a handle opened, waited on and closed here.
        unsafe {
            if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                WaitForSingleObject(process, 30_000);
                CloseHandle(process).ok();
            }
        }
    }
    #[cfg(not(windows))]
    let _: u32 = pid;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_is_due_once_a_day() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(due(None, t));
        assert!(!due(Some(t), t + Duration::from_secs(23 * 60 * 60)));
        assert!(due(Some(t), t + DAY));
        // The clock went back.
        assert!(due(Some(t), t - Duration::from_secs(1)));
    }

    #[test]
    fn this_build_offers_no_updates() {
        // Tests are built without an update key, and never run as the installed copy.
        assert!(!supported());
    }
}
