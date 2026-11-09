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
mod errors;
mod groups;
mod large_group;
mod lost;
mod network;
mod presage;
pub(crate) mod record;
mod rng;
mod setup;
mod storage;
mod stress;
mod tour;

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use strum::VariantArray;

use crate::phase::{AndroidPhase, LinkedPhase, MeshPhase, Phase, StressPhase};
use crate::pod::{Client, Pod};
use crate::{Sockets, TestOptions};

use self::record::{Record, RecordError};
use testbox_tools::Report;

/// How long a client may take to say it listens: the emulator's first boot is the slowest.
const START: Duration = Duration::from_secs(60 * 60);
/// How long a step may take; the drivers take it in milliseconds.
const PATIENCE_MS: u64 = 120_000;
const PATIENCE: Duration = Duration::from_millis(PATIENCE_MS);
/// Exporting, uploading, downloading and importing a message history.
const HISTORY_PATIENCE: Duration = Duration::from_secs(600);

/// Why a check, a fixture's setup or a phase failed: whichever step's own error it was, kept whole
/// for the report, which prints it with its causes. A check is a sequence of steps across the pod,
/// b33p's harness, the drivers and the check's own expectations; each step's error says what
/// failed, and the check's name says what it was for.
pub(crate) type Failure = Box<dyn std::error::Error + Send + Sync>;

/// A check others depend on failed, and was reported as it failed ([`Checks::require`]): what
/// comes after it doesn't run.
#[derive(Debug, thiserror::Error)]
#[error("{check} failed")]
pub(crate) struct Unmet {
    pub(crate) check: String,
}

/// A run that can't start, or that found failures.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RunError {
    /// `--capture` with a stress phase, whose volume would fill the disk.
    #[error("--capture is not for the stress phases ({phase})")]
    CaptureWithStress { phase: &'static str },
    /// The Android image didn't build, so no box that needs it can play.
    #[error("the Android image didn't build (above)")]
    AndroidImage,
    /// The checks that failed, each under its part's name, and how many phases didn't run.
    #[error("{}", failures(failed, *not_run))]
    Failed { failed: Vec<String>, not_run: usize },
}

/// The summary [`RunError::Failed`] prints: the failed checks, one a line, and the phases not run.
fn failures(failed: &[String], not_run: usize) -> String {
    let mut outcome = Vec::new();
    if !failed.is_empty() {
        let failed: String = failed.iter().map(|check| format!("\n- {check}")).collect();
        outcome.push(format!("failed:{failed}"));
    }
    if not_run > 0 {
        outcome.push(format!("{not_run} phases not run"));
    }
    outcome.join("\n")
}

/// What the checks after every part found new on the box ([`Reported::none_new`]).
#[derive(Debug, thiserror::Error)]
#[error("{count} {what}:\n{}", listed.join("\n"))]
pub(crate) struct NewLines {
    /// What the lines are: lost messages, crashes, unexpected errors.
    what: &'static str,
    count: usize,
    /// The first [`LISTED`] of them.
    listed: Vec<String>,
}

/// An environment variable that should hold a number ([`env_number`]).
#[derive(Debug, thiserror::Error)]
pub(crate) enum EnvNumberError {
    #[error("{name}={value:?} isn't a number")]
    NotANumber { name: String, value: String },
    #[error("the environment variable {name}")]
    Unreadable {
        name: String,
        #[source]
        source: std::env::VarError,
    },
}

/// Runs the phases `options` names, every phase but the stress phases if none, each fixture's on
/// a box from nothing (with `--fresh`, each phase on a box of its own), each box's record in
/// `target/testbox-record/<records>/<box>/`; fails if a check failed.
pub(crate) fn run(
    containers: &Pod,
    repo: &Path,
    records: &str,
    options: &TestOptions,
) -> Result<(), Failure> {
    let TestOptions {
        phases,
        screenshots,
        fresh,
        capture,
        b33p,
    } = options;
    if *capture
        && let Some(stress) = phases.iter().find_map(|phase| match phase {
            Phase::Stress(stress) => Some(<&'static str>::from(*stress)),
            _ => None,
        })
    {
        return Err(RunError::CaptureWithStress { phase: stress }.into());
    }
    containers.capture(*capture);
    containers.set_ui(b33p.ui);
    let sockets = Sockets::new(containers);
    let files = containers.files_dir();
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
    // Once it failed, every box that needs it fails (with `--fresh`, each Android phase has one):
    // the image there is an earlier build's.
    let mut android_failed = false;
    // Desktop's image only for a box that runs Desktop: a tour's has none.
    let desktop = !linked.is_empty()
        || !android.is_empty()
        || phases.contains(&Phase::Stress(StressPhase::Concurrency));
    let built: &[Client] = if desktop { &linked_clients } else { &[] };
    containers.build_others(built, b33p.optimized)?;
    let play = |clients: &[Client], b33ps: u8| -> Result<(), Failure> {
        containers.fresh(clients, b33ps)?;
        Ok(b33p.limit_memory(containers, b33ps)?)
    };
    let b33ps = b33p.b33ps.unwrap_or(1);
    // Only when named, as the stress phases.
    let mesh: Vec<MeshPhase> = MeshPhase::VARIANTS
        .iter()
        .copied()
        .filter(|&phase| phases.contains(&Phase::Mesh(phase)))
        .collect();
    let mesh_b33ps = b33p.b33ps.unwrap_or(3);

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
        || play(&linked_clients, b33ps),
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
            LinkedPhase::Network => network::network(
                checks,
                fixture,
                containers,
                &files,
                &repo.join("docs/screenshots/01-chats.png"),
            ),
            LinkedPhase::Core => core::core(checks, fixture, containers, &sockets),
        },
    );
    on_fixture(
        &mut tally,
        "android",
        &android,
        || {
            if let Some(build) = android_image.take() {
                build.finish().inspect_err(|_| android_failed = true)?;
            }
            if android_failed {
                return Err(RunError::AndroidImage.into());
            }
            play(&android_clients, b33ps)
        },
        |checks| setup::android_box(checks, containers, &sockets),
        |phase, checks, fixture| match phase {
            AndroidPhase::Android => android::android(checks, fixture),
            AndroidPhase::Groups => groups::groups(checks, fixture, containers, &sockets),
            AndroidPhase::Storage => storage::storage(checks, fixture),
            AndroidPhase::LargeGroup => large_group::large_group(checks, fixture, containers),
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
            || play(clients, b33ps),
            |checks| match phase {
                StressPhase::Volume => stress::volume(checks, containers, &sockets),
                StressPhase::Interrupt => stress::interrupt(checks, containers, &sockets, &files),
                StressPhase::Content => stress::content(checks, containers, &sockets, &files),
                StressPhase::Concurrency => stress::concurrency(checks, containers, &sockets),
                StressPhase::Soak => stress::soak(checks, containers, &sockets, &files),
            },
        );
    }
    on_fixture(
        &mut tally,
        "mesh",
        &mesh,
        || play(&[], mesh_b33ps),
        |checks| setup::mesh_box(checks, containers, &sockets, mesh_b33ps),
        |phase, checks, fixture| match phase {
            MeshPhase::Tour => tour::tour(
                checks,
                fixture,
                containers,
                &sockets,
                &files,
                &repo.join("docs/screenshots/01-chats.png"),
            ),
        },
    );
    Ok(tally.finish()?)
}

/// Brings a box up from nothing for the phase `name` alone (`ready`), which sets it up itself, and
/// runs it, its record that of a box of its own.
fn on_own_box(
    tally: &mut Tally<'_>,
    name: &str,
    ready: impl FnOnce() -> Result<(), Failure>,
    phase: impl FnOnce(&mut Checks) -> Result<(), Failure>,
) {
    let record = match ready().and_then(|()| Ok(tally.record(name)?)) {
        Ok(record) => record,
        Err(error) => {
            let error = Report(&*error);
            println!("{name}: {error}");
            tally.failed.push(format!("{name}: {error}"));
            tally.not_run += 1;
            return;
        }
    };
    tally.reported = Reported::default();
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
    mut ready: impl FnMut() -> Result<(), Failure>,
    mut fixture: impl FnMut(&mut Checks) -> Result<Fixture, Failure>,
    mut phase: impl FnMut(Each, &mut Checks, &mut Fixture) -> Result<(), Failure>,
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
    ready: impl FnOnce() -> Result<(), Failure>,
    fixture: impl FnOnce(&mut Checks) -> Result<Fixture, Failure>,
    mut phase: impl FnMut(Each, &mut Checks, &mut Fixture) -> Result<(), Failure>,
) {
    let name = format!("the {record} box");
    let record = match ready().and_then(|()| Ok(tally.record(record)?)) {
        Ok(record) => record,
        Err(error) => {
            let error = Report(&*error);
            println!("{name}: {error}");
            tally.failed.push(format!("{name}: {error}"));
            tally.not_run += phases.len();
            return;
        }
    };
    tally.reported = Reported::default();
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
    /// The current box's: each box's logs start empty.
    reported: Reported,
}

impl Tally<'_> {
    /// The box `name`'s record, emptied of a former run's.
    fn record(&self, name: &str) -> Result<Record, RecordError> {
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
        part: impl FnOnce(&mut Checks) -> Result<T, Failure>,
    ) -> Option<T> {
        println!("{name}:");
        let started = Instant::now();
        let kept = match record.part(directory) {
            Ok(kept) => kept,
            Err(error) => {
                let error = Report(&error);
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
            && !error.is::<Unmet>()
        {
            let error = Report(&**error);
            println!("{name} ended: {error}");
            checks.failed.push(error.to_string());
        }
        checks.record("no client lost a message", || {
            let lost = lost::found(self.containers)?;
            Ok(self.reported.none_new("lost", lost)?)
        });
        checks.record("no client crashed", || {
            Ok(self
                .reported
                .none_new("crashed", crashed::found(self.containers))?)
        });
        checks.record("b33p logged no unexpected error", || {
            Ok(self
                .reported
                .none_new("unexpected errors", errors::found(self.containers))?)
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

    /// Fails if a check failed or a phase didn't run.
    fn finish(self) -> Result<(), RunError> {
        if self.failed.is_empty() && self.not_run == 0 {
            Ok(())
        } else {
            Err(RunError::Failed {
                failed: self.failed,
                not_run: self.not_run,
            })
        }
    }
}

/// What the checks after every part found already on the box, each line with how often: a box's
/// logs keep what its earlier parts had. A line can come again, as a client killed the same way
/// twice: then it is new again.
#[derive(Default)]
struct Reported(HashMap<String, usize>);

impl Reported {
    /// Fails, listing them as `what`, if `found` has lines not reported before, or a line more
    /// often than before.
    fn none_new(&mut self, what: &'static str, found: Vec<String>) -> Result<(), NewLines> {
        let mut counted: HashMap<String, usize> = HashMap::new();
        let mut new = Vec::new();
        for line in found {
            let count = counted.entry(line.clone()).or_default();
            *count += 1;
            if *count > self.0.get(&line).copied().unwrap_or_default() {
                new.push(line);
            }
        }
        for (line, count) in counted {
            let reported = self.0.entry(line).or_default();
            *reported = (*reported).max(count);
        }
        if new.is_empty() {
            return Ok(());
        }
        let count = new.len();
        new.truncate(LISTED);
        Err(NewLines {
            what,
            count,
            listed: new,
        })
    }
}

/// How many lines such a failure lists.
const LISTED: usize = 10;

/// The number in the environment variable `name`, if it is set: one that doesn't parse fails
/// rather than being passed over, as a seed mistyped to replay a run would be.
fn env_number<T: std::str::FromStr>(name: &str) -> Result<Option<T>, EnvNumberError> {
    match std::env::var(name) {
        Ok(value) => match value.trim().parse() {
            Ok(number) => Ok(Some(number)),
            Err(_) => Err(EnvNumberError::NotANumber {
                name: name.to_owned(),
                value,
            }),
        },
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(source) => Err(EnvNumberError::Unreadable {
            name: name.to_owned(),
            source,
        }),
    }
}

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
        check: impl FnOnce() -> Result<Found, Failure>,
    ) -> Result<Found, Unmet> {
        self.report(name, check).ok_or_else(|| Unmet {
            check: name.to_owned(),
        })
    }

    /// A check nothing depends on: a failure is tallied and the phase goes on.
    fn record(&mut self, name: &str, check: impl FnOnce() -> Result<(), Failure>) {
        self.report(name, check);
    }

    /// Runs `check` and prints its outcome and how long it took; a failure is tallied and
    /// yields `None`.
    fn report<Found>(
        &mut self,
        name: &str,
        check: impl FnOnce() -> Result<Found, Failure>,
    ) -> Option<Found> {
        let started = Instant::now();
        let started_at = SystemTime::now();
        let outcome = check();
        let took = started.elapsed().as_secs_f64();
        self.kept
            .span(name, started_at, SystemTime::now(), outcome.is_ok());
        self.kept.shoot(name);
        match outcome {
            Ok(found) => {
                println!("check {name}: passed ({took:.1} s)");
                self.passed += 1;
                Some(found)
            }
            Err(reason) => {
                println!("check {name}: failed ({took:.1} s): {}", Report(&*reason));
                self.failed.push(name.to_owned());
                None
            }
        }
    }
}

/// `line` without its ANSI escape sequences, each up to its final letter: the colours of a log.
fn without_colours(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            chars.by_ref().find(char::is_ascii_alphabetic);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::Reported;

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|&line| line.to_owned()).collect()
    }

    /// A line the part before reported isn't new; the same line once more is, as b33p killed
    /// for its memory twice.
    #[test]
    fn a_line_again_is_new_again() {
        let killed = "b33p: with-display: b33p exited: signal: 9 (SIGKILL)";
        let (once, twice) = (&[killed][..], &[killed, killed][..]);
        let mut reported = Reported::default();
        assert!(reported.none_new("crashed", lines(once)).is_err());
        assert!(reported.none_new("crashed", lines(once)).is_ok());
        assert!(reported.none_new("crashed", lines(twice)).is_err());
        assert!(reported.none_new("crashed", lines(twice)).is_ok());
    }
}
