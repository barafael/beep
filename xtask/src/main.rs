//! `cargo testbox`, a Cargo alias of this xtask (.cargo/config.toml).

// Every outcome is handled: a discarded `Result` would hide a failed step.
#![deny(
    clippy::let_underscore_must_use,
    clippy::let_underscore_untyped,
    clippy::disallowed_types
)]

#[cfg(unix)]
mod checks;
#[cfg(unix)]
mod error;
#[cfg(unix)]
mod harness;
mod pins;
mod pod;
mod process;
#[cfg(unix)]
mod vnc;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use error::{Context, Result};
use pod::{Client, Pod};

/// Checks b33p against Signal's own server and clients: Signal-Server with its stores and CDNs,
/// Signal-Desktop and Signal-Android, in one Podman pod.
///
/// The pod's network is internal: nothing in it reaches Signal or anything else outside. Every
/// upstream is pinned, and images take network only while building. Android needs `/dev/kvm`.
#[derive(Parser)]
#[command(bin_name = "cargo testbox")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Builds the images and plays the pod.
    Up {
        /// Signal-Desktop, which the checks link to the peer's account.
        #[arg(long)]
        desktop: bool,
        /// Signal-Android on an emulator.
        #[arg(long)]
        android: bool,
    },
    /// Runs a phase's checks against the pod.
    ///
    /// `up --desktop --android` brings up every client a phase drives. A pass shows agreement
    /// with Signal-Server's test mode at its pin, not with Signal's servers.
    Test {
        phase: Phase,
        /// Also takes each client's display after each check, into the phase's record
        /// (target/testbox-record/<phase>/) beside the containers' logs.
        #[arg(long)]
        screenshots: bool,
    },
    /// Runs phases each on a fresh box with the clients it drives (`down --wipe`, `up`, `test`),
    /// in rounds, and sums up; each run's record is in target/testbox-record/run/.
    Run {
        /// The phases, in order; all of them if none.
        phases: Vec<Phase>,
        /// How many times each.
        #[arg(long, default_value_t = 1)]
        rounds: u32,
        /// As for `test`.
        #[arg(long)]
        screenshots: bool,
    },
    /// Shows the clients' displays in a window.
    Vnc {
        /// Lets the window's keys and pointer through to the clients, where they race a
        /// running check's.
        #[arg(long)]
        interactive: bool,
    },
    /// Serves the clients' displays as a page on loopback, and prints its address.
    Web {
        /// As for `vnc`.
        #[arg(long)]
        interactive: bool,
    },
    /// Puts the QR b33p shows for linking on the emulator camera's poster, for Android's
    /// scanner when linking by hand.
    Qr,
    /// Stops and removes the pod.
    Down {
        /// Also removes the volumes: accounts, keys and stores.
        #[arg(long)]
        wipe: bool,
    },
    /// Prints containers' logs.
    Logs {
        /// The containers' names in testbox/pod.yaml.
        services: Vec<String>,
    },
}

/// `test`'s phases.
#[derive(Clone, Copy, clap::ValueEnum)]
enum Phase {
    Core,
    Desktop,
    Android,
    Attachments,
    Storage,
    Groups,
}

impl Phase {
    const ALL: [Phase; 6] = [
        Phase::Core,
        Phase::Desktop,
        Phase::Attachments,
        Phase::Android,
        Phase::Storage,
        Phase::Groups,
    ];

    fn name(self) -> String {
        clap::ValueEnum::to_possible_value(&self)
            .map(|value| value.get_name().to_owned())
            .unwrap_or_default()
    }

    /// The clients the phase drives, which `up` brings.
    fn clients(self) -> &'static [Client] {
        match self {
            Phase::Core => &[],
            Phase::Desktop | Phase::Attachments => &[Client::Desktop],
            Phase::Android | Phase::Storage | Phase::Groups => &[Client::Desktop, Client::Android],
        }
    }
}

fn main() -> ExitCode {
    let repo = repo_root();
    let pod = Pod::new(&repo);
    let outcome = match Cli::parse().command {
        Command::Up { desktop, android } => {
            let clients = [(desktop, Client::Desktop), (android, Client::Android)];
            let clients: Vec<Client> = clients
                .into_iter()
                .filter_map(|(asked, client)| asked.then_some(client))
                .collect();
            up(&pod, &clients)
        }
        Command::Test { phase, screenshots } => {
            test(&pod, &repo, phase, &phase.name(), screenshots)
        }
        Command::Run {
            phases,
            rounds,
            screenshots,
        } => run(&pod, &repo, &phases, rounds, screenshots),
        Command::Vnc { interactive } => vnc(&pod, &repo, interactive),
        Command::Web { interactive } => web(&pod, &repo, interactive),
        Command::Qr => qr(&pod, &repo),
        Command::Down { wipe } => pod.down(wipe),
        Command::Logs { services } => {
            let services: Vec<&str> = services.iter().map(String::as_str).collect();
            pod.logs(&services)
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("testbox: {reason}");
            ExitCode::FAILURE
        }
    }
}

/// The harness's and drivers' sockets, on the host.
#[cfg(unix)]
struct Sockets {
    b33p: PathBuf,
    desktop: PathBuf,
    android: PathBuf,
}

#[cfg(unix)]
impl Sockets {
    fn new(repo: &Path) -> Self {
        let run = run_dir(repo);
        Self {
            b33p: run.join("agent.sock"),
            desktop: run.join("desktop.sock"),
            android: run.join("android.sock"),
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_owned()
}

/// Where the harness and driver sockets appear on the host: the clients' `/run/b33p`
/// (testbox/pod.yaml).
fn run_dir(repo: &Path) -> PathBuf {
    repo.join(pod::RUN)
}

fn up(pod: &Pod, clients: &[Client]) -> Result<()> {
    pod.up(clients)
}

/// Each of `phases`, or all, `rounds` times, each on a fresh box; then how each went. Fails if
/// any did.
fn run(pod: &Pod, repo: &Path, phases: &[Phase], rounds: u32, screenshots: bool) -> Result<()> {
    let phases = if phases.is_empty() {
        &Phase::ALL[..]
    } else {
        phases
    };
    let runs = repo.join(RUNS);
    if runs.exists() {
        std::fs::remove_dir_all(&runs).context(runs.display().to_string())?;
    }
    let mut outcomes = Vec::new();
    for round in 1..=rounds {
        for &phase in phases {
            let name = phase.name();
            println!("== round {round}: {name}");
            let outcome = pod
                .down(true)
                .and_then(|()| up(pod, phase.clients()))
                .and_then(|()| {
                    test(
                        pod,
                        repo,
                        phase,
                        &format!("run/{round}-{name}"),
                        screenshots,
                    )
                });
            if let Err(reason) = &outcome {
                println!("== round {round}: {name}: {reason}");
            }
            outcomes.push((round, name, outcome.is_ok()));
        }
    }
    println!("== summary (records in {})", runs.display());
    for (round, name, passed) in &outcomes {
        let outcome = if *passed { "passed" } else { "FAILED" };
        println!("round {round} {name}: {outcome}");
    }
    let failed = outcomes.iter().filter(|(_, _, passed)| !passed).count();
    if failed == 0 {
        Ok(())
    } else {
        Err(error::Error::Failed(format!(
            "{failed} of {} failed",
            outcomes.len()
        )))
    }
}

/// Where `run` keeps its records, one directory per round and phase.
const RUNS: &str = "target/testbox-record/run";

/// A phase's checks, recorded in target/testbox-record/`record`.
#[cfg(unix)]
fn test(pod: &Pod, repo: &Path, phase: Phase, record: &str, screenshots: bool) -> Result<()> {
    let sockets = Sockets::new(repo);
    let record = checks::Record::new(pod, repo, record, screenshots)?;
    checks::phase(pod, record, |tally| match phase {
        Phase::Core => checks::core(tally, pod, &sockets),
        Phase::Desktop => checks::desktop(tally, pod, &sockets),
        Phase::Android => checks::android(tally, pod, &sockets),
        Phase::Storage => checks::storage(tally, pod, &sockets),
        Phase::Groups => checks::groups(tally, pod, &sockets),
        Phase::Attachments => checks::attachments(
            tally,
            pod,
            &sockets,
            &repo.join(pod::FILES),
            // Any picture will do; this one is in the repository already.
            &repo.join("docs/screenshots/01-chats.png"),
        ),
    })
}

#[cfg(unix)]
fn vnc(pod: &Pod, repo: &Path, interactive: bool) -> Result<()> {
    vnc::vnc(pod, repo, interactive)
}

#[cfg(unix)]
fn qr(pod: &Pod, repo: &Path) -> Result<()> {
    vnc::qr(pod, &Sockets::new(repo).b33p)
}

#[cfg(not(unix))]
fn qr(_pod: &Pod, _repo: &Path) -> Result<()> {
    Err("the harness is Unix only (b33p-slint `harness` feature)".into())
}

#[cfg(unix)]
fn web(pod: &Pod, repo: &Path, interactive: bool) -> Result<()> {
    vnc::web(pod, repo, interactive)
}

#[cfg(not(unix))]
fn web(_pod: &Pod, _repo: &Path, _interactive: bool) -> Result<()> {
    Err("the displays' sockets are Unix sockets".into())
}

#[cfg(not(unix))]
fn vnc(_pod: &Pod, _repo: &Path, _interactive: bool) -> Result<()> {
    Err("the displays' sockets are Unix sockets".into())
}

#[cfg(not(unix))]
fn test(_pod: &Pod, _repo: &Path, _phase: Phase, _record: &str, _screenshots: bool) -> Result<()> {
    Err("the harness is Unix only (b33p-slint `harness` feature)".into())
}
