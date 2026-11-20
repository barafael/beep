//! `--jobs N`: the boxes of a `test` or `run` play at once, each on a pod of its own.
//!
//! The units are what a box runs from nothing: the linked box with its phases, the Android box
//! with its, each stress phase, the mesh (with `--fresh`, each phase of the linked and Android
//! boxes alone). A worker takes the next unit when its box is free; a box is the pod
//! `<instance>-j<k>` of the worker `k`, played by a `cargo testbox test` of its own (a child
//! process: its checks, its records, its output are as ever, kept in a log). The images are built
//! once, here, and tagged for each box (`--prebuilt`), so that a box starts as fast as it can.
//!
//! What it doesn't share between boxes: nothing the testbox has in common is written by both. A
//! box's pod has a network namespace and an internal network of its own, volumes, sockets and
//! records are named by the box (`xtask/src/pod.rs`); the emulators share only `/dev/kvm` and
//! the CPUs, so at most [`ANDROID_AT_ONCE`] run at a time.

use std::collections::VecDeque;
use std::process::Command;
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use clap::ValueEnum;
use strum::VariantArray;

use crate::phase::{
    AndroidPhase, FamilyPhase, InteropPhase, LinkedPhase, MeshPhase, Phase, StressPhase,
};
use crate::pod::{Building, Pod};
use crate::{Failure, TestOptions};

/// How many Android emulators may run at once: each takes cores and memory for as long as the box
/// lives, and their boot and Signal-Android's UI are what a loaded machine slows most.
const ANDROID_AT_ONCE: usize = 2;

/// A set of phases that run on one box from nothing.
struct Unit {
    name: String,
    phases: Vec<Phase>,
    android: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("{} of {total} units failed: {}", failed.len(), failed.join(", "))]
struct UnitsFailed {
    failed: Vec<String>,
    total: usize,
}

/// The phases' name on a command line.
fn name(phase: Phase) -> String {
    phase
        .to_possible_value()
        .map(|value| value.get_name().to_owned())
        .unwrap_or_default()
}

/// The units `options` asks for: what `checks::run` would run, grouped by box.
fn units(options: &TestOptions) -> Vec<Unit> {
    let named = &options.phases;
    let everything = named.is_empty();
    let linked: Vec<Phase> = LinkedPhase::VARIANTS
        .iter()
        .copied()
        .map(Phase::Linked)
        .filter(|phase| everything || named.contains(phase))
        .collect();
    let android: Vec<Phase> = AndroidPhase::VARIANTS
        .iter()
        .copied()
        .map(Phase::Android)
        .filter(|phase| everything || named.contains(phase))
        .collect();
    let mut units = Vec::new();
    let mut group = |label: &str, phases: Vec<Phase>, android: bool| {
        if phases.is_empty() {
            return;
        }
        if options.fresh {
            units.extend(phases.into_iter().map(|phase| Unit {
                name: name(phase),
                phases: vec![phase],
                android,
            }));
        } else {
            units.push(Unit {
                name: label.to_owned(),
                phases,
                android,
            });
        }
    };
    // The longest first: what a box that finishes early picks up is short.
    group("linked", linked, false);
    group("android", android, true);
    // Each of these is a box of its own (and the soak is the longest of all).
    let own = |phase: Phase| Unit {
        name: name(phase),
        phases: vec![phase],
        android: false,
    };
    for phase in StressPhase::VARIANTS
        .iter()
        .rev()
        .copied()
        .map(Phase::Stress)
    {
        if named.contains(&phase) {
            units.push(own(phase));
        }
    }
    for phase in MeshPhase::VARIANTS
        .iter()
        .copied()
        .map(Phase::Mesh)
        .chain(InteropPhase::VARIANTS.iter().copied().map(Phase::Interop))
        .chain(FamilyPhase::VARIANTS.iter().copied().map(Phase::Family))
    {
        if named.contains(&phase) {
            units.push(own(phase));
        }
    }
    units
}

/// The Android image, building beside the others and awaited by the first unit that needs it.
enum AndroidImage {
    Building(Building),
    Built,
    Failed(String),
    NotNeeded,
}

struct Slots {
    free: Mutex<usize>,
    freed: Condvar,
}

impl Slots {
    fn take(&self) {
        let mut free = self.free.lock().expect("slots");
        while *free == 0 {
            free = self.freed.wait(free).expect("slots");
        }
        *free -= 1;
    }

    fn give(&self) {
        *self.free.lock().expect("slots") += 1;
        self.freed.notify_one();
    }
}

/// What a unit came to.
struct Outcome {
    unit: String,
    instance: String,
    seconds: f64,
    passed: bool,
    /// The records the unit's box printed (`record: <directory>`).
    records: Vec<String>,
}

/// Plays `options`' units on `options.jobs` boxes at once; fails if a unit failed.
pub(crate) fn run(pod: &Pod, options: &TestOptions) -> Result<(), Failure> {
    let units = units(options);
    if units.is_empty() {
        println!("jobs: no phase to run");
        return Ok(());
    }
    // What `checks::run` builds, once: the server's first, the Android image beside the rest.
    pod.build_server()?;
    let wants_android = units.iter().any(|unit| unit.android);
    let android = Mutex::new(if wants_android {
        AndroidImage::Building(pod.build_beside(options.b33p.android_app.client())?)
    } else {
        AndroidImage::NotNeeded
    });
    let phases: Vec<Phase> = units.iter().flat_map(|unit| unit.phases.clone()).collect();
    pod.build_others(
        &crate::checks::others_needed(&phases),
        options.b33p.optimized,
    )?;

    let workers = usize::from(options.jobs).min(units.len());
    let base = pod
        .pod_name()
        .strip_prefix("b33p-testbox.")
        .unwrap_or("jobs")
        .chars()
        .take(18)
        .collect::<String>();
    let exe = std::env::current_exe()?;
    let total = units.len();
    let queue = Mutex::new(units.into_iter().collect::<VecDeque<_>>());
    let slots = Slots {
        free: Mutex::new(ANDROID_AT_ONCE),
        freed: Condvar::new(),
    };
    let screen = Mutex::new(());
    let outcomes = Mutex::new(Vec::<Outcome>::new());
    let started = Instant::now();
    println!("jobs: {total} units on {workers} boxes ({base}-j1 …)");

    std::thread::scope(|scope| {
        for worker in 1..=workers {
            let instance = format!("{base}-j{worker}");
            let (queue, slots, screen, outcomes, android, exe, pod_name) = (
                &queue,
                &slots,
                &screen,
                &outcomes,
                &android,
                &exe,
                pod.pod_name(),
            );
            scope.spawn(move || {
                let mut clean = true;
                // A box whose unit failed stays as that unit left it, to be looked at: its
                // worker takes no more units, the other workers take the rest.
                while clean {
                    let Some(unit) = queue.lock().expect("queue").pop_front() else {
                        break;
                    };
                    if unit.android {
                        slots.take();
                    }
                    let began = Instant::now();
                    let image_failed = if unit.android {
                        await_android(android)
                    } else {
                        None
                    };
                    let (passed, output) = match image_failed {
                        Some(error) => (false, format!("the Android image: {error}\n")),
                        None => {
                            {
                                let _screen = screen.lock().expect("screen");
                                println!("jobs: {} starts on {instance}", unit.name);
                            }
                            child(exe, &instance, pod_name, &unit, options)
                        }
                    };
                    if unit.android {
                        slots.give();
                    }
                    let seconds = began.elapsed().as_secs_f64();
                    {
                        let _screen = screen.lock().expect("screen");
                        println!(
                            "== {} on {instance}: {} in {seconds:.0} s",
                            unit.name,
                            if passed { "passed" } else { "FAILED" }
                        );
                        print!("{output}");
                    }
                    clean &= passed;
                    let records = output
                        .lines()
                        .filter_map(|line| line.strip_prefix("record: "))
                        .map(str::to_owned)
                        .collect();
                    outcomes.lock().expect("outcomes").push(Outcome {
                        unit: unit.name,
                        instance: instance.clone(),
                        seconds,
                        passed,
                        records,
                    });
                }
                // A box that failed stays up to be looked at, as `test`'s; the others go.
                if clean {
                    let down = Command::new(exe)
                        .args(["--instance", &instance, "down", "--wipe"])
                        .output();
                    if let Err(error) = down {
                        eprintln!("jobs: taking {instance} down: {error}");
                    }
                }
            });
        }
    });

    let outcomes = outcomes.into_inner().expect("outcomes");
    // What no worker was left to run: every box had a unit fail.
    let not_run: Vec<String> = queue
        .into_inner()
        .expect("queue")
        .into_iter()
        .map(|unit| unit.name)
        .collect();
    println!("== jobs summary, {:.0} s", started.elapsed().as_secs_f64());
    for outcome in &outcomes {
        println!(
            "{:<20} {:<8} {:>6.0} s  {}",
            outcome.unit,
            if outcome.passed { "passed" } else { "FAILED" },
            outcome.seconds,
            outcome.instance
        );
        for record in &outcome.records {
            println!("    record: {record}");
        }
    }
    let failed: Vec<String> = outcomes
        .iter()
        .filter(|outcome| !outcome.passed)
        .map(|outcome| format!("{} (box {}, left up)", outcome.unit, outcome.instance))
        .chain(
            not_run
                .iter()
                .map(|unit| format!("{unit} (not run: every box failed)")),
        )
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(UnitsFailed { failed, total }.into())
    }
}

/// Waits for the Android image, if it is building; why it failed, if it did.
fn await_android(image: &Mutex<AndroidImage>) -> Option<String> {
    let mut image = image.lock().expect("android image");
    if let AndroidImage::Building(_) = &*image {
        let AndroidImage::Building(building) =
            std::mem::replace(&mut *image, AndroidImage::NotNeeded)
        else {
            return None;
        };
        *image = match building.finish() {
            Ok(()) => AndroidImage::Built,
            Err(error) => AndroidImage::Failed(testbox_tools::Report(&error).to_string()),
        };
    }
    match &*image {
        AndroidImage::Failed(error) => Some(error.clone()),
        _ => None,
    }
}

/// `unit` as `cargo testbox test` on the box `instance`, its images those `from` built: whether
/// it passed, and its output (also in `target/testbox-<from>-jobs/<unit>.log`).
fn child(
    exe: &std::path::Path,
    instance: &str,
    from: &str,
    unit: &Unit,
    options: &TestOptions,
) -> (bool, String) {
    let mut command = Command::new(exe);
    command.args(["--instance", instance, "test"]);
    command.args(unit.phases.iter().map(|phase| name(*phase)));
    command.args(["--prebuilt", from]);
    if options.screenshots {
        command.arg("--screenshots");
    }
    if options.capture {
        command.arg("--capture");
    }
    if options.b33p.optimized {
        command.arg("--optimized");
    }
    if let Some(app) = options.b33p.android_app.to_possible_value() {
        command.args(["--android-app", app.get_name()]);
    }
    if let Some(mib) = options.b33p.b33p_memory {
        command.arg("--b33p-memory").arg(mib.to_string());
    }
    if let Some(n) = options.b33p.b33ps {
        command.arg("--b33ps").arg(n.to_string());
    }
    match command.output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            let errors = String::from_utf8_lossy(&output.stderr);
            if !output.status.success() {
                text.push_str(&errors);
            }
            (output.status.success(), text)
        }
        Err(error) => (false, format!("starting {}: {error}\n", exe.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::B33pOptions;

    fn options(phases: Vec<Phase>, fresh: bool) -> TestOptions {
        TestOptions {
            phases,
            screenshots: false,
            fresh,
            capture: false,
            jobs: 2,
            prebuilt: None,
            b33p: B33pOptions {
                optimized: false,
                android_app: crate::pod::AndroidApp::Signal,
                b33p_memory: None,
                b33ps: None,
            },
        }
    }

    fn names(units: &[Unit]) -> Vec<String> {
        units.iter().map(|unit| unit.name.clone()).collect()
    }

    #[test]
    fn the_default_phases_are_the_two_boxes() {
        let units = units(&options(Vec::new(), false));
        assert_eq!(names(&units), ["linked", "android"]);
        assert_eq!(units[0].phases.len(), LinkedPhase::VARIANTS.len());
        assert_eq!(units[1].phases.len(), AndroidPhase::VARIANTS.len());
        assert!(!units[0].android && units[1].android);
    }

    #[test]
    fn fresh_makes_a_unit_of_each_phase() {
        let units = units(&options(Vec::new(), true));
        assert_eq!(
            units.len(),
            LinkedPhase::VARIANTS.len() + AndroidPhase::VARIANTS.len()
        );
        assert!(units.iter().all(|unit| unit.phases.len() == 1));
        assert_eq!(
            units.iter().filter(|unit| unit.android).count(),
            AndroidPhase::VARIANTS.len()
        );
    }

    #[test]
    fn stress_and_mesh_phases_are_units_of_their_own_when_named() {
        let named = vec![
            Phase::Stress(StressPhase::Volume),
            Phase::Stress(StressPhase::Soak),
            Phase::Mesh(MeshPhase::Tour),
        ];
        let units = units(&options(named, false));
        // The soak, the longest, first.
        assert_eq!(names(&units), ["stress-soak", "stress-volume", "tour"]);
    }

    #[test]
    fn interop_and_family_phases_are_units_of_their_own_when_named() {
        let named = vec![
            Phase::Family(FamilyPhase::Flare),
            Phase::Interop(InteropPhase::Mixed),
            Phase::Interop(InteropPhase::SignalCli),
        ];
        let units = units(&options(named, false));
        assert_eq!(units.len(), 3);
        assert!(
            units
                .iter()
                .all(|unit| unit.phases.len() == 1 && !unit.android)
        );
        // What they need is built once, for all the boxes.
        let phases: Vec<Phase> = units.iter().flat_map(|unit| unit.phases.clone()).collect();
        let built = crate::checks::others_needed(&phases);
        assert!(!built.contains(&crate::pod::Client::Desktop));
        for client in [
            crate::pod::Client::SignalCli,
            crate::pod::Client::Signalmeow,
            crate::pod::Client::Family,
        ] {
            assert!(built.contains(&client));
        }
    }

    #[test]
    fn named_phases_of_one_box_share_it() {
        let named = vec![
            Phase::Linked(LinkedPhase::Core),
            Phase::Linked(LinkedPhase::Desktop),
        ];
        let units = units(&options(named, false));
        assert_eq!(names(&units), ["linked"]);
        // In the order they run, not as named.
        assert!(units[0].phases[0] == Phase::Linked(LinkedPhase::Desktop));
    }
}
