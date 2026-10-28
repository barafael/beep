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
mod phase;
mod pins;
mod pod;
mod process;
#[cfg(unix)]
mod vnc;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use error::Result;
use phase::Phase;
use pod::{Client, Pod};
use tracing_subscriber::EnvFilter;

/// Checks b33p against Signal's own server and clients: Signal-Server with its stores and CDNs,
/// Signal-Desktop and Signal-Android, in one Podman pod.
///
/// The pod's network is internal: nothing in it reaches Signal or anything else outside. Every
/// upstream is pinned, and images take network only while building. Android needs `/dev/kvm`.
#[derive(Parser)]
#[command(bin_name = "cargo testbox")]
struct Cli {
    /// A box of its own beside the others, from any worktree: its pod, network, volumes and
    /// images named `b33p-testbox.<instance>`, its directories `target/testbox-<instance>-*`.
    /// Without it, the box is `b33p-testbox`, with its directories in `target/testbox-*`.
    #[arg(long, global = true, env = "TESTBOX_INSTANCE")]
    instance: Option<String>,
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
        #[command(flatten)]
        b33p: B33pOptions,
    },
    /// Runs the phases' checks, every phase's if none is named but the stress and mesh phases'.
    ///
    /// Builds the images, the Android image while the linked box runs, each build's output in
    /// target/testbox-build. The phases on one setup share a box (without `--fresh`): the pod
    /// plays from nothing, its volumes removed, then the setup's checks run once and the phases
    /// one after the other. Each box's record is in `target/testbox-record/<box>/`. A pass shows
    /// agreement with Signal-Server's test mode at its pin, not with Signal's servers.
    Test {
        #[command(flatten)]
        options: TestOptions,
    },
    /// Runs `test` in rounds and sums up; each run's records are in
    /// `target/testbox-record/run/<when it started>/<round>/`.
    Run {
        /// How many times.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        rounds: u32,
        #[command(flatten)]
        options: TestOptions,
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

/// What `test` and `run` run, and how.
#[derive(clap::Args)]
pub(crate) struct TestOptions {
    /// The phases; all but the stress and mesh phases if none.
    pub(crate) phases: Vec<Phase>,
    /// Also takes each client's display after each check, into the record beside the
    /// containers' logs.
    #[arg(long)]
    pub(crate) screenshots: bool,
    /// Each phase on a box of its own, as the stress phases: slower, a box's setup for each, but
    /// no phase sees what another left.
    #[arg(long)]
    pub(crate) fresh: bool,
    #[command(flatten)]
    pub(crate) b33p: B33pOptions,
}

/// How `up` runs b33p.
#[derive(Clone, Copy, clap::Args)]
pub(crate) struct B33pOptions {
    /// b33p optimized as it ships, with debug assertions on (Cargo.toml's `testbox` profile),
    /// instead of as a debug build.
    #[arg(long)]
    pub(crate) optimized: bool,
    /// The most memory b33p's container may take, in MiB; the kernel kills b33p past it, as
    /// on a small machine or under a desktop's own limits.
    #[arg(long, value_name = "MIB")]
    pub(crate) b33p_memory: Option<u64>,
    /// How many b33ps the box has, each in a container of its own: `up`'s and every box's but
    /// the mesh's one, the mesh's three.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=i64::from(pod::MAX_B33PS)))]
    pub(crate) b33ps: Option<u8>,
}

impl B33pOptions {
    /// Lets each of the box's `b33ps` b33ps take at most `--b33p-memory`, if given: every one
    /// the pod played, whether it runs at this moment or not.
    fn limit_memory(&self, pod: &Pod, b33ps: u8) -> Result<()> {
        let Some(mib) = self.b33p_memory else {
            return Ok(());
        };
        (0..b33ps.max(1)).try_for_each(|n| pod.limit_memory(pod::Service::B33p(n), mib))
    }
}

fn main() -> ExitCode {
    // RUST_LOG as tracing-subscriber reads it; `debug` adds podman's own output.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let repo = repo_root();
    let cli = Cli::parse();
    let pod = match Pod::new(&repo, cli.instance.as_deref()) {
        Ok(pod) => pod,
        Err(reason) => {
            eprintln!("testbox: {reason}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = match cli.command {
        Command::Up {
            desktop,
            android,
            b33p,
        } => {
            let clients = [(desktop, Client::Desktop), (android, Client::Android)];
            let clients: Vec<Client> = clients
                .into_iter()
                .filter_map(|(asked, client)| asked.then_some(client))
                .collect();
            pod.holding(|| up(&pod, &clients, b33p))
        }
        Command::Test { options } => pod.holding(|| test(&pod, &repo, "", &options)),
        Command::Run { rounds, options } => pod.holding(|| run(&pod, &repo, rounds, &options)),
        Command::Vnc { interactive } => vnc(&pod, &repo, interactive),
        Command::Web { interactive } => web(&pod, &repo, interactive),
        Command::Qr => qr(&pod),
        Command::Down { wipe } => pod.holding(|| pod.down(wipe)),
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
    /// The first b33p's ([`Self::agent`] 0).
    b33p: PathBuf,
    desktop: PathBuf,
    android: PathBuf,
    run: PathBuf,
}

#[cfg(unix)]
impl Sockets {
    /// Where the harness and driver sockets appear on the host: the clients' `/run/b33p`
    /// (testbox/pod.yaml).
    fn new(pod: &Pod) -> Self {
        let run = pod.run_dir();
        Self {
            b33p: run.join(pod::agent_socket(0)),
            desktop: run.join("desktop.sock"),
            android: run.join("android.sock"),
            run,
        }
    }

    /// The `n`th b33p's harness.
    fn agent(&self, n: u8) -> PathBuf {
        self.run.join(pod::agent_socket(n))
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_owned()
}

fn up(pod: &Pod, clients: &[Client], b33p: B33pOptions) -> Result<()> {
    let b33ps = b33p.b33ps.unwrap_or(1);
    pod.up(clients, b33ps, b33p.optimized)?;
    b33p.limit_memory(pod, b33ps)
}

/// `test` `rounds` times, each round's records in a directory of its own; then how each went.
/// Fails if any did.
fn run(pod: &Pod, repo: &Path, rounds: u32, options: &TestOptions) -> Result<()> {
    // A directory of its own, by when it started: an earlier run's records stay.
    let started = process::now()?;
    let run = format!("run/{started}");
    let mut outcomes = Vec::new();
    for round in 1..=rounds {
        println!("== round {round}");
        let outcome = test(pod, repo, &format!("{run}/{round}"), options);
        if let Err(reason) = &outcome {
            println!("== round {round}: {reason}");
        }
        outcomes.push((round, outcome.is_ok()));
    }
    println!(
        "== summary (records in {})",
        pod.record_dir().join(&run).display()
    );
    for (round, passed) in &outcomes {
        let outcome = if *passed { "passed" } else { "FAILED" };
        println!("round {round}: {outcome}");
    }
    let failed = outcomes.iter().filter(|(_, passed)| !passed).count();
    if failed == 0 {
        Ok(())
    } else {
        Err(error::Error::Failed(format!(
            "{failed} of {rounds} rounds failed"
        )))
    }
}

/// The phases' checks once, each box's record in `target/testbox-record/<records>/<box>/`.
#[cfg(unix)]
fn test(pod: &Pod, repo: &Path, records: &str, options: &TestOptions) -> Result<()> {
    checks::run(pod, repo, records, options)
}

#[cfg(unix)]
fn vnc(pod: &Pod, repo: &Path, interactive: bool) -> Result<()> {
    vnc::vnc(pod, repo, interactive)
}

#[cfg(unix)]
fn qr(pod: &Pod) -> Result<()> {
    vnc::qr(pod, &Sockets::new(pod).b33p)
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
fn test(_pod: &Pod, _repo: &Path, _records: &str, _options: &TestOptions) -> Result<()> {
    Err("the harness is Unix only (b33p-slint `harness` feature)".into())
}
