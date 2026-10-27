//! `cargo testbox test`'s checks, against Signal-Server, by phase. The phases on one fixture
//! (setup.rs) share a fresh box, set up once, and run one after the other; with `--fresh`, and
//! always the stress phases, each phase has a box of its own.
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

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use strum::VariantArray;

use crate::error::{Error, Result};
use crate::phase::{AndroidPhase, LinkedPhase, Phase, StressPhase};
use crate::pod::{self, Client, Pod, Service};
use crate::{Sockets, TestOptions};

use self::record::Record;

/// How long a client may take to say it listens: the emulator's first boot is the slowest.
const START: Duration = Duration::from_secs(60 * 60);
/// How long a step may take; the drivers take it in milliseconds.
const PATIENCE_MS: u64 = 120_000;
const PATIENCE: Duration = Duration::from_millis(PATIENCE_MS);
/// Exporting, uploading, downloading and importing a message history.
const HISTORY_PATIENCE: Duration = Duration::from_secs(600);

/// Runs the phases `options` names, every phase but the stress phases if none, each fixture's on
/// a box from nothing (with `--fresh`, each phase on a box of its own), each box's record in
/// `target/testbox-record/<records>/<box>/`; fails if a check failed.
pub(crate) fn run(
    containers: &Pod,
    repo: &Path,
    records: &str,
    options: &TestOptions,
) -> Result<()> {
    let TestOptions {
        phases,
        screenshots,
        fresh,
        b33p,
    } = options;
    let sockets = Sockets::new(repo);
    let files = repo.join(pod::FILES);
    let linked = chosen(phases, Phase::Linked);
    let android = chosen(phases, Phase::Android);
    let linked_clients = [Client::Desktop];
    let android_clients = [Client::Desktop, Client::Android];
    containers.build_server()?;

    // The Android image's build takes longest, most of an hour when Signal-Android changed: it
    // goes on while the rest build and the linked box runs, and stops if the run ends first. It
    // needs only the server's.
    let mut android_image = if android.is_empty() {
        None
    } else {
        Some(containers.build_beside(Client::Android)?)
    };
    containers.build_others(&linked_clients, b33p.optimized)?;
    let play = |clients: &[Client]| {
        containers.fresh(clients)?;
        match b33p.b33p_memory {
            Some(mib) => containers.limit_memory(Service::B33p, mib),
            None => Ok(()),
        }
    };

    let mut tally = Tally {
        containers,
        repo,
        records,
        screenshots: *screenshots,
        fresh: *fresh,
        failed: Vec::new(),
        not_run: 0,
        reported: Reported::default(),
    };
    on_fixture(
        &mut tally,
        "linked",
        &linked,
        || play(&linked_clients),
        |checks| setup::linked_box(checks, containers, &sockets),
        |phase, checks, fixture| match phase {
            LinkedPhase::Desktop => desktop::desktop(checks, fixture),
            LinkedPhase::Attachments => attachments::attachments(
                checks,
                fixture,
                containers,
                &sockets,
                &files,
                // Any picture will do; this one is in the repository already.
                &repo.join("docs/screenshots/01-chats.png"),
            ),
            LinkedPhase::Core => core::core(checks, fixture, containers),
        },
    );
    on_fixture(
        &mut tally,
        "android",
        &android,
        || {
            if let Some(build) = android_image.take() {
                build.finish()?;
            }
            play(&android_clients)
        },
        |checks| setup::android_box(checks, containers, &sockets),
        |phase, checks, fixture| match phase {
            AndroidPhase::Android => android::android(checks, fixture),
            AndroidPhase::Groups => groups::groups(checks, fixture, containers),
            AndroidPhase::Storage => storage::storage(checks, fixture),
        },
    );
    for phase in StressPhase::VARIANTS
        .iter()
        .copied()
        .filter(|&phase| phases.contains(&Phase::Stress(phase)))
    {
        let clients: &[Client] = match phase {
            StressPhase::Concurrency => &linked_clients,
            StressPhase::Volume
            | StressPhase::Interrupt
            | StressPhase::Content
            | StressPhase::Soak => &[],
        };
        on_own_box(
            &mut tally,
            phase.into(),
            || play(clients),
            |checks| match phase {
                StressPhase::Volume => stress::volume(checks, containers, &sockets),
                StressPhase::Interrupt => stress::interrupt(checks, containers, &sockets, &files),
                StressPhase::Content => stress::content(checks, containers, &sockets, &files),
                StressPhase::Concurrency => stress::concurrency(checks, containers, &sockets),
                StressPhase::Soak => stress::soak(checks, containers, &sockets, &files),
            },
        );
    }
    tally.finish()
}

/// Brings a box up from nothing for the phase `name` alone (`ready`), which sets it up itself, and
/// runs it, its record that of a box of its own.
fn on_own_box(
    tally: &mut Tally<'_>,
    name: &str,
    ready: impl FnOnce() -> Result<()>,
    phase: impl FnOnce(&mut Checks) -> Result<()>,
) {
    let record = match ready().and_then(|()| tally.record(name)) {
        Ok(record) => record,
        Err(error) => {
            println!("{name}: {error}");
            tally.failed.push(format!("{name}: {error}"));
            tally.not_run += 1;
            return;
        }
    };
    tally.part(name, &record, name, phase);
    record.keep_logs(tally.containers);
}

/// The phases of one box (`Each`, which `phase` makes a [`Phase`]) that `phases` names, every
/// one if it names none, in the order they run.
fn chosen<Each: VariantArray + Copy>(phases: &[Phase], phase: fn(Each) -> Phase) -> Vec<Each> {
    Each::VARIANTS
        .iter()
        .copied()
        .filter(|&each| phases.is_empty() || phases.contains(&phase(each)))
        .collect()
}

/// Runs a fixture's `phases` on the box `record` names, or with `--fresh` each on a box of its
/// own, named after it ([`on_box`]).
fn on_fixture<Fixture, Each: Copy + Into<&'static str>>(
    tally: &mut Tally<'_>,
    record: &'static str,
    phases: &[Each],
    mut ready: impl FnMut() -> Result<()>,
    mut fixture: impl FnMut(&mut Checks) -> Result<Fixture>,
    mut phase: impl FnMut(Each, &mut Checks, &mut Fixture) -> Result<()>,
) {
    let per_box = if tally.fresh { 1 } else { phases.len().max(1) };
    for group in phases.chunks(per_box) {
        let record = match group {
            [only] if tally.fresh => (*only).into(),
            _ => record,
        };
        on_box(tally, record, group, &mut ready, &mut fixture, &mut phase);
    }
}

/// Brings the box up from nothing (`ready`: its images built, the pod played), sets the fixture
/// up, and runs `phases` on it one after the other: each part with a directory of its own in the
/// box's record (`record`), the containers' logs in it at the end. If the box doesn't come up,
/// they count as not run.
fn on_box<Fixture, Each: Copy + Into<&'static str>>(
    tally: &mut Tally<'_>,
    record: &str,
    phases: &[Each],
    ready: impl FnOnce() -> Result<()>,
    fixture: impl FnOnce(&mut Checks) -> Result<Fixture>,
    mut phase: impl FnMut(Each, &mut Checks, &mut Fixture) -> Result<()>,
) {
    let name = format!("the {record} box");
    let record = match ready().and_then(|()| tally.record(record)) {
        Ok(record) => record,
        Err(error) => {
            println!("{name}: {error}");
            tally.failed.push(format!("{name}: {error}"));
            tally.not_run += phases.len();
            return;
        }
    };
    match tally.part(&name, &record, "setup", fixture) {
        Some(mut fixture) => {
            for &each in phases {
                let part: &'static str = each.into();
                tally.part(part, &record, part, |checks| {
                    phase(each, checks, &mut fixture)
                });
            }
        }
        None => tally.not_run += phases.len(),
    }
    record.keep_logs(tally.containers);
}

/// What a run found: each part's failed checks, under the part's name.
struct Tally<'run> {
    containers: &'run Pod,
    repo: &'run Path,
    /// Where the boxes' records go, under target/testbox-record.
    records: &'run str,
    screenshots: bool,
    /// Each phase on a box of its own ([`on_fixture`]).
    fresh: bool,
    failed: Vec<String>,
    /// Phases a box that didn't come up or wasn't set up left without a box to run on.
    not_run: usize,
    reported: Reported,
}

impl Tally<'_> {
    /// The box `name`'s record, emptied of a former run's.
    fn record(&self, name: &str) -> Result<Record> {
        let path = if self.records.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.records)
        };
        Record::new(self.containers, self.repo, &path, self.screenshots)
    }

    /// Runs `part` (a fixture's setup or a phase) on checks of its own, its record `directory`
    /// within the box's, and prints what it found; its value if it ran to its end. A failure
    /// outside a check counts as the part's. Last, whatever came before: that no client lost a
    /// message ([`lost`]) or crashed ([`crashed`]) since the part before.
    fn part<T>(
        &mut self,
        name: &str,
        record: &Record,
        directory: &str,
        part: impl FnOnce(&mut Checks) -> Result<T>,
    ) -> Option<T> {
        println!("{name}:");
        let started = Instant::now();
        let kept = match record.part(directory) {
            Ok(kept) => kept,
            Err(error) => {
                println!("{name}: {error}");
                self.failed.push(format!("{name}: {error}"));
                return None;
            }
        };
        let mut checks = Checks {
            passed: 0,
            failed: Vec::new(),
            kept,
        };
        let outcome = part(&mut checks);
        // An unmet check is tallied already.
        if let Err(error) = &outcome
            && !matches!(error, Error::Unmet(_))
        {
            println!("{name} ended: {error}");
            checks.failed.push(error.to_string());
        }
        checks.record("no client lost a message", || {
            self.reported.none_new("lost", lost::found(self.containers))
        });
        checks.record("no client crashed", || {
            self.reported
                .none_new("crashed", Ok(crashed::found(self.containers)))
        });
        println!(
            "{name}: {} passed, {} failed, in {:.0} s",
            checks.passed,
            checks.failed.len(),
            started.elapsed().as_secs_f64()
        );
        self.failed.extend(
            checks
                .failed
                .into_iter()
                .map(|check| format!("{name}: {check}")),
        );
        outcome.ok()
    }

    fn finish(self) -> Result<()> {
        let mut outcome = Vec::new();
        if !self.failed.is_empty() {
            let failed: String = self
                .failed
                .iter()
                .map(|check| format!("\n- {check}"))
                .collect();
            outcome.push(format!("failed:{failed}"));
        }
        if self.not_run > 0 {
            outcome.push(format!("{} phases not run", self.not_run));
        }
        if outcome.is_empty() {
            Ok(())
        } else {
            Err(Error::Failed(outcome.join("\n")))
        }
    }
}

/// What the checks after every part found already: a box's logs keep what its earlier parts
/// had.
#[derive(Default)]
struct Reported(HashSet<String>);

impl Reported {
    /// Fails, listing them as `what`, if `found` has lines not reported before.
    fn none_new(&mut self, what: &str, found: Result<Vec<String>>) -> Result<()> {
        let mut new = found?;
        new.retain(|line| self.0.insert(line.clone()));
        if new.is_empty() {
            return Ok(());
        }
        let count = new.len();
        new.truncate(LISTED);
        Err(Error::Failed(format!(
            "{count} {what}:\n{}",
            new.join("\n")
        )))
    }
}

/// How many lines such a failure lists.
const LISTED: usize = 10;

pub(crate) struct Checks {
    passed: usize,
    failed: Vec<String>,
    kept: Record,
}

impl Checks {
    /// A check later checks depend on: its failure ends the phase or the fixture (the caller's `?`).
    fn require<Found>(
        &mut self,
        name: &str,
        check: impl FnOnce() -> Result<Found>,
    ) -> Result<Found> {
        self.report(name, check)
            .ok_or_else(|| Error::Unmet(name.to_owned()))
    }

    /// A check nothing depends on: a failure is tallied and the phase goes on.
    fn record(&mut self, name: &str, check: impl FnOnce() -> Result<()>) {
        self.report(name, check);
    }

    /// Runs `check` and prints its outcome and how long it took; a failure is tallied and
    /// yields `None`.
    fn report<Found>(
        &mut self,
        name: &str,
        check: impl FnOnce() -> Result<Found>,
    ) -> Option<Found> {
        let started = Instant::now();
        let outcome = check();
        let took = started.elapsed().as_secs_f64();
        self.kept.shoot(name);
        match outcome {
            Ok(found) => {
                println!("check {name}: passed ({took:.1} s)");
                self.passed += 1;
                Some(found)
            }
            Err(reason) => {
                println!("check {name}: failed ({took:.1} s): {reason}");
                self.failed.push(name.to_owned());
                None
            }
        }
    }
}
