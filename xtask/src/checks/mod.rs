//! `cargo testbox test <name>`'s checks, against Signal-Server. Each needs a fresh box
//! (`cargo testbox down --wipe`, then `up`): b33p must start unlinked.
//!
//! Against Signal-Desktop or Signal-Android, b33p's side and the other share only libsignal, at
//! different versions, so a defect there can pass unseen. Against presage-cli both sides are
//! presage and libsignal-service, so those checks speak only for the server's side: transport,
//! reconnects, fan-out to linked devices. Out of reach: contact discovery and SVR (enclaves), a
//! production phone's history, Signal's own servers.

mod android;
mod attachments;
mod b33p;
mod clients;
mod core;
mod crashed;
mod desktop;
mod groups;
mod lost;
mod presage;
mod record;
mod setup;
mod storage;
mod stress;

use std::time::Duration;

use crate::error::{Error, Result};
use crate::pod::Pod;

pub(crate) use self::android::android;
pub(crate) use self::attachments::attachments;
pub(crate) use self::core::core;
pub(crate) use self::desktop::desktop;
pub(crate) use self::groups::groups;
pub(crate) use self::record::Record;
pub(crate) use self::storage::storage;
pub(crate) use self::stress::{
    concurrency as stress_concurrency, content as stress_content, interrupt as stress_interrupt,
    soak as stress_soak, volume as stress_volume,
};

/// How long a client may take to say it listens: the emulator's first boot is the slowest.
const START: Duration = Duration::from_secs(60 * 60);
/// How long a step may take; the drivers take it in milliseconds.
const PATIENCE_MS: u64 = 120_000;
const PATIENCE: Duration = Duration::from_millis(PATIENCE_MS);
/// Exporting, uploading, downloading and importing a message history.
const HISTORY_PATIENCE: Duration = Duration::from_secs(600);

/// Runs a phase's checks on a fresh tally, then fails if any of them failed. Last, also after
/// a check the rest depend on failed: that no client lost a message ([`lost`]) or crashed
/// ([`crashed`]); then the logs go into the phase's [`Record`].
pub(crate) fn phase(
    containers: &Pod,
    record: Record,
    run: impl FnOnce(&mut Checks) -> Result<()>,
) -> Result<()> {
    let mut checks = Checks {
        failed: Vec::new(),
        kept: record,
    };
    let ran = run(&mut checks);
    checks.record("no client lost a message", || lost::none(containers));
    checks.record("no client crashed", || crashed::none(containers));
    checks.kept.keep_logs(containers);
    ran?;
    checks.finish()
}

pub(crate) struct Checks {
    failed: Vec<String>,
    kept: Record,
}

impl Checks {
    /// A check later checks depend on: its failure ends the run (the caller's `?`).
    fn require<Found>(
        &mut self,
        name: &str,
        check: impl FnOnce() -> Result<Found>,
    ) -> Result<Found> {
        self.report(name, check())
            .ok_or_else(|| Error::Failed(format!("{name} failed")))
    }

    /// A check nothing depends on: a failure is tallied for [`Self::finish`] and the run goes on.
    fn record(&mut self, name: &str, check: impl FnOnce() -> Result<()>) {
        self.report(name, check());
    }

    /// Prints the outcome; a failure is tallied and yields `None`.
    fn report<Found>(&mut self, name: &str, outcome: Result<Found>) -> Option<Found> {
        self.kept.shoot(name);
        match outcome {
            Ok(found) => {
                println!("check {name}: passed");
                Some(found)
            }
            Err(reason) => {
                println!("check {name}: failed: {reason}");
                self.failed.push(name.to_owned());
                None
            }
        }
    }

    fn finish(self) -> Result<()> {
        if self.failed.is_empty() {
            Ok(())
        } else {
            Err(Error::Failed(format!("failed: {}", self.failed.join(", "))))
        }
    }
}
