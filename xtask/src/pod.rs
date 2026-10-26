//! The box as one pod under `podman kube play` (testbox/pod.yaml), not Compose: that would be a
//! second definition of the box beside the pod. Revise if the box has to run on Docker.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::error::{Context, Error, Result, WithContext};
use crate::process::{self, Heard};

/// pod.yaml's `metadata.name`; kube play names each container `<pod>-<container>`.
const POD: &str = "b33p-testbox";
const NETWORK: &str = "b33p-testbox";
/// The harness's and the drivers' sockets (testbox/pod.yaml volume `run`): the owner's only, as
/// the clients' side and the xtask's are both the user's.
pub const RUN: &str = "target/testbox-run";
/// The test files the xtask writes and the clients read (testbox/pod.yaml volume `files`).
pub const FILES: &str = "target/testbox-files";
/// What each phase leaves for looking into it ([`crate::checks::Record`]), and `run`'s under
/// `run/<when it started>/<round>-<phase>`.
pub const RECORD: &str = "target/testbox-record";
/// A directory per client for its display's socket, which only that client mounts
/// (testbox/pod.yaml volumes `view-<client>`): no client reaches another's display.
pub const VIEW: &str = "target/testbox-view";
/// The clients with a display, as pod.yaml's `view-<client>` volumes name them.
pub const SCREENS: [Service; 3] = [Service::B33p, Service::Desktop, Service::Android];
/// The line of pod.yaml's container list the clients go in, all of it.
const CLIENTS: &str = "# @CLIENTS@";

/// A container of the pod (testbox/pod.yaml, the clients' files).
#[derive(Clone, Copy, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum Service {
    Server,
    Storage,
    Cdn0,
    Presage,
    B33p,
    Desktop,
    Android,
}

/// A service that says in its log when it is ready. The lines are constants the logging side
/// shares, so a reworded one fails to compile instead of hanging a check.
#[derive(Clone, Copy)]
pub enum Ready {
    /// Once its entrypoint says it serves with Desktop's remote config written (testbox/tools
    /// `provision`), not once it listens: registering before that fails with "websocket upgrade
    /// failed".
    Server,
    Storage,
    B33p,
    Desktop,
    Android,
}

impl Ready {
    fn service(self) -> Service {
        match self {
            Self::Server => Service::Server,
            Self::Storage => Service::Storage,
            Self::B33p => Service::B33p,
            Self::Desktop => Service::Desktop,
            Self::Android => Service::Android,
        }
    }

    /// b33p-slint/src/agent.rs, testbox/desktop/driver.ts and testbox/tools `android` for the
    /// harness and the drivers.
    fn line(self) -> &'static str {
        match self {
            Self::Server => testbox_tools::SERVER_READY,
            Self::Storage => testbox_tools::STORAGE_READY,
            Self::B33p => b33p_harness::LISTENING,
            Self::Desktop | Self::Android => testbox_tools::drivers::LISTENING,
        }
    }
}

/// A client `up` adds when asked (`--<name>`): its container in `testbox/<name>/pod.yaml`.
#[derive(Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum Client {
    Desktop,
    Android,
}

impl Client {
    fn container(self) -> String {
        let name: &str = self.into();
        format!("testbox/{name}/pod.yaml")
    }
}

/// An image pod.yaml runs, built before it plays: kube play builds only images named after a
/// directory of their own, and these mostly build from the repository (for testbox/tools).
struct Image {
    tag: &'static str,
    /// Relative to the repository.
    containerfile: &'static str,
    context: &'static str,
    /// Built only with this client.
    client: Option<Client>,
    /// Takes the build profile (`PROFILE`): b33p's.
    profiled: bool,
}

impl Image {
    /// The defaults: the repository as the context, with every client.
    const REPOSITORY: Self = Self {
        tag: "",
        containerfile: "",
        context: ".",
        client: None,
        profiled: false,
    };
}

const IMAGES: [Image; 10] = [
    Image {
        tag: "localhost/b33p-testbox-server",
        containerfile: "testbox/server/Containerfile",
        ..Image::REPOSITORY
    },
    Image {
        tag: "localhost/b33p-testbox-dynamodb",
        containerfile: "testbox/dynamodb/Containerfile",
        context: "testbox/dynamodb",
        ..Image::REPOSITORY
    },
    Image {
        tag: "localhost/b33p-testbox-foundationdb",
        containerfile: "testbox/foundationdb/Containerfile",
        ..Image::REPOSITORY
    },
    Image {
        tag: "localhost/b33p-testbox-tus",
        containerfile: "testbox/tus/Containerfile",
        context: "testbox/tus",
        ..Image::REPOSITORY
    },
    Image {
        tag: "localhost/b33p-testbox-cdn0",
        containerfile: "testbox/garage/Containerfile",
        ..Image::REPOSITORY
    },
    Image {
        tag: "localhost/b33p-testbox-storage",
        containerfile: "testbox/storage/Containerfile",
        ..Image::REPOSITORY
    },
    Image {
        tag: "localhost/b33p-testbox-presage",
        containerfile: "testbox/presage/Containerfile",
        ..Image::REPOSITORY
    },
    Image {
        tag: "localhost/b33p-testbox-b33p",
        containerfile: "testbox/b33p/Containerfile",
        profiled: true,
        ..Image::REPOSITORY
    },
    Image {
        tag: "localhost/b33p-testbox-desktop",
        containerfile: "testbox/desktop/Containerfile",
        context: "testbox/desktop",
        client: Some(Client::Desktop),
        profiled: false,
    },
    Image {
        tag: "localhost/b33p-testbox-android",
        containerfile: "testbox/android/Containerfile",
        client: Some(Client::Android),
        ..Image::REPOSITORY
    },
];

pub struct Pod {
    repo: PathBuf,
    /// pod.yaml with its paths and clients filled in: hostPath must be absolute.
    rendered: PathBuf,
    /// What tells of a lost message, from outside the containers' logs ([`Self::note_lost`]).
    lost: Mutex<Vec<String>>,
}

impl Pod {
    pub fn new(repo: &Path) -> Self {
        Self {
            repo: repo.to_owned(),
            rendered: repo.join("target/testbox-pod.yaml"),
            lost: Mutex::default(),
        }
    }

    /// `release` builds b33p as it ships instead of as the checks' own builds are.
    pub fn up(&self, clients: &[Client], release: bool) -> Result<()> {
        for image in IMAGES
            .iter()
            .filter(|image| image.client.is_none_or(|client| clients.contains(&client)))
        {
            // Stages that don't depend on each other at once (a Maven build and a cargo one).
            let mut build = podman(&["build", "--jobs", "0", "-t", image.tag]);
            if image.profiled {
                let profile = if release { "release" } else { "dev" };
                build.arg("--build-arg").arg(format!("PROFILE={profile}"));
            }
            build
                .arg("-f")
                .arg(self.repo.join(image.containerfile))
                .arg(self.repo.join(image.context));
            process::run(build)?;
        }
        // Not in pod.yaml: Kubernetes YAML has no network objects.
        process::run(podman(&[
            "network",
            "create",
            "--ignore",
            "--internal",
            NETWORK,
        ]))?;
        // `--ignore` keeps a network of that name that was there: only if it has no egress.
        let internal = process::output(
            podman(&["network", "inspect", "--format", "{{.Internal}}", NETWORK]),
            "",
        )?;
        if internal.trim() != "true" {
            return Err(Error::Failed(format!(
                "the network {NETWORK} has egress: `podman network rm {NETWORK}`"
            )));
        }
        // The test files: the clients mount them read-only, so none can plant a link there for
        // the xtask to write through.
        let files = self.repo.join(FILES);
        std::fs::create_dir_all(&files).context(files.display().to_string())?;
        // The sockets: the owner's only, whatever is in it; a link a container plants there
        // leads nowhere (`testbox_tools::box_socket`).
        let run = self.repo.join(RUN);
        std::fs::create_dir_all(&run).context(run.display().to_string())?;
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700))
            .context(run.display().to_string())?;
        // The displays' sockets likewise; pod.yaml mounts each client's, so all of them exist.
        for directory in std::iter::once(self.repo.join(VIEW))
            .chain(SCREENS.map(|screen| self.view_socket(screen).with_file_name("")))
        {
            std::fs::create_dir_all(&directory).context(directory.display().to_string())?;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .context(directory.display().to_string())?;
        }
        testbox_tools::write(&self.rendered, self.render(clients)?)?;
        let mut play = podman(&["kube", "play", "--replace", "--network", NETWORK]);
        play.arg(&self.rendered);
        process::run(play)
    }

    /// pod.yaml with the repository filled in, and the clients' containers in its
    /// [`CLIENTS`] line.
    fn render(&self, clients: &[Client]) -> Result<String> {
        let read = |path: &str| testbox_tools::read(&self.repo.join(path));
        let containers = clients
            .iter()
            .map(|client| read(&client.container()))
            .collect::<Result<String>>()?;
        let pod: String = read("testbox/pod.yaml")?
            .lines()
            .map(|line| {
                if line.trim() == CLIENTS {
                    containers.clone()
                } else {
                    format!("{line}\n")
                }
            })
            .collect();
        Ok(pod.replace("@REPO@", &self.repo.to_string_lossy()))
    }

    /// Where `screen`'s display serves on the host (its `with-display` socket, /run/view/vnc).
    pub fn view_socket(&self, screen: Service) -> PathBuf {
        let name: &'static str = screen.into();
        self.repo.join(VIEW).join(name).join("vnc")
    }

    /// Whether `service`'s container runs; one that isn't there doesn't.
    pub fn running(&self, service: Service) -> bool {
        let state = process::output(
            podman(&[
                "container",
                "inspect",
                "--format",
                "{{.State.Running}}",
                &container(service),
            ]),
            "",
        );
        state.is_ok_and(|running| running.trim() == "true")
    }

    /// `--force` also removes the persistentVolumeClaims' volumes. kube down needs the YAML the
    /// pod was played from, which another worktree's `up` wrote in its own `target`: then the
    /// pod and, to wipe, the volumes go by their names.
    pub fn down(&self, wipe: bool) -> Result<()> {
        if self.rendered.exists() {
            let mut down = podman(&["kube", "down"]);
            if wipe {
                down.arg("--force");
            }
            down.arg(&self.rendered);
            process::run(down)?;
        }
        let pod = process::output(
            podman(&["pod", "ps", "-q", "--filter", &format!("name=^{POD}$")]),
            "",
        )?;
        if !pod.trim().is_empty() {
            process::run(podman(&["pod", "rm", "--force", POD]))?;
        }
        if wipe {
            let volumes = process::output(
                podman(&["volume", "ls", "-q", "--filter", &format!("name=^{POD}-")]),
                "",
            )?;
            let volumes: Vec<&str> = volumes.split_whitespace().collect();
            if !volumes.is_empty() {
                process::run(podman(&[&["volume", "rm"][..], &volumes].concat()))?;
            }
        }
        Ok(())
    }

    /// `service`'s log so far, what it wrote to stdout and stderr alike.
    pub fn log(&self, service: Service) -> Result<String> {
        self.log_named(&container(service))
    }

    /// The pod's containers by name, stopped ones too; none if podman can't say.
    pub fn containers(&self) -> Vec<String> {
        let listed = process::output(
            podman(&[
                "ps",
                "--all",
                "--filter",
                &format!("pod={POD}"),
                "--format",
                "{{.Names}}",
            ]),
            "",
        );
        listed
            .map(|names| names.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// [`Self::log`] of the container `name`.
    pub fn log_named(&self, name: &str) -> Result<String> {
        let logs = podman(&["logs", name])
            .output()
            .with_context(|| format!("podman logs {name}"))?;
        if !logs.status.success() {
            return Err(Error::Failed(format!(
                "podman logs {name}: {}",
                String::from_utf8_lossy(&logs.stderr)
            )));
        }
        let mut log = String::from_utf8_lossy(&logs.stdout).into_owned();
        log.push_str(&String::from_utf8_lossy(&logs.stderr));
        Ok(log)
    }

    /// Notes `line` as telling of a lost message: for what only a command's output says, such
    /// as presage-cli's.
    pub fn note_lost(&self, line: String) {
        self.lost
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line);
    }

    /// What [`Self::note_lost`] noted.
    pub fn lost(&self) -> Vec<String> {
        self.lost
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn logs(&self, services: &[&str]) -> Result<()> {
        for service in services {
            process::run(podman(&["logs", &format!("{POD}-{service}")]))?;
        }
        Ok(())
    }

    /// Runs `argv` in `service`, without a shell, with `stdin` as its input; returns stdout, and
    /// fails with what it printed on a non-zero exit.
    pub fn exec(&self, service: Service, argv: &[&str], stdin: &str) -> Result<String> {
        let mut exec = podman(&["exec", "-i", &container(service)]);
        exec.args(argv);
        process::output(exec, stdin)
    }

    /// Runs `argv` in `service` without a shell, and returns how it ended, whatever that was.
    pub fn exec_status(&self, service: Service, argv: &[&str]) -> Result<std::process::Output> {
        let mut exec = podman(&["exec", &container(service)]);
        exec.args(argv);
        exec.output()
            .with_context(|| format!("podman exec {}", argv.join(" ")))
    }

    /// Freezes `service` until [`Self::unpause`]: its connections stay open, and nothing answers
    /// on them, as when a network goes away without a word.
    pub fn pause(&self, service: Service) -> Result<()> {
        process::run(podman(&["pause", &container(service)]))
    }

    pub fn unpause(&self, service: Service) -> Result<()> {
        process::run(podman(&["unpause", &container(service)]))
    }

    pub fn restart(&self, service: Service) -> Result<()> {
        process::run(podman(&["restart", &container(service)]))
    }

    /// Stops `service` as `podman stop` does: its stop signal, then SIGKILL after the timeout.
    pub fn stop(&self, service: Service) -> Result<()> {
        process::run(podman(&["stop", &container(service)]))
    }

    pub fn start(&self, service: Service) -> Result<()> {
        process::run(podman(&["start", &container(service)]))
    }

    /// Ends every process in `service` at once, as a power cut would: nothing gets to flush or
    /// close. `with-display` goes with its client, so the crash check doesn't count it.
    pub fn kill(&self, service: Service) -> Result<()> {
        process::run(podman(&["kill", "--signal", "KILL", &container(service)]))
    }

    /// What `service` uses of memory, as `podman stats` says it ("212.4MB / 33.3GB").
    pub fn memory(&self, service: Service) -> Result<String> {
        let stats = podman(&[
            "stats",
            "--no-stream",
            "--format",
            "{{.MemUsage}}",
            &container(service),
        ]);
        process::output(stats, "").map(|usage| usage.trim().to_owned())
    }

    /// Lets `service` take at most `mib` MiB, swap included: past it, the kernel kills it.
    pub fn limit_memory(&self, service: Service, mib: u64) -> Result<()> {
        let limit = format!("{mib}m");
        process::run(podman(&[
            "update",
            "--memory",
            &limit,
            "--memory-swap",
            &limit,
            &container(service),
        ]))
    }

    /// The most memory `service` used at once since it started, in bytes: its cgroup's
    /// `memory.peak`, which a sampled [`Self::memory`] would miss a spike of.
    pub fn memory_peak(&self, service: Service) -> Result<u64> {
        let peak = self.exec(service, &["cat", "/sys/fs/cgroup/memory.peak"], "")?;
        peak.trim()
            .parse()
            .map_err(|_| Error::Failed(format!("memory.peak reads {peak:?}")))
    }

    /// Waits until `service` logs a line containing `needle`, after `since`: what the service
    /// says when it is ready, as kube play has no readiness or startup probes, so nothing polls.
    /// Across restarts: a container that starts before what it needs exits, and the pod starts it
    /// again (`restartPolicy: Always`); its log goes on in the restarted one, which `podman wait`
    /// says is running.
    /// Waits until `ready` says so, after `since`; [`Self::await_log`].
    pub fn await_ready(&self, ready: Ready, since: Option<u64>, patience: Duration) -> Result<()> {
        self.await_log(ready.service(), ready.line(), since, patience)
    }

    pub fn await_log(
        &self,
        service: Service,
        needle: &str,
        since: Option<u64>,
        patience: Duration,
    ) -> Result<()> {
        self.await_logs(service, needle, 1, since, patience)
    }

    /// [`Self::await_log`] for `times` such lines.
    pub fn await_logs(
        &self,
        service: Service,
        needle: &str,
        times: usize,
        since: Option<u64>,
        patience: Duration,
    ) -> Result<()> {
        let deadline = Instant::now() + patience;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match process::await_lines(follow_logs(service, since), needle, times, left)? {
                Heard::Found => return Ok(()),
                Heard::Ended => {
                    let running = podman(&["wait", "--condition", "running", &container(service)]);
                    process::output(running, "")?;
                }
            }
        }
    }
}

fn podman(args: &[&str]) -> Command {
    let mut podman = Command::new("podman");
    podman.args(args);
    podman
}

/// What kube play names `service`'s container.
fn container(service: Service) -> String {
    let name: &'static str = service.into();
    format!("{POD}-{name}")
}

/// `logs --follow` of `service`, from `since` on (seconds since the epoch), or from its start.
fn follow_logs(service: Service, since: Option<u64>) -> Command {
    let mut logs = podman(&["logs", "--follow"]);
    if let Some(since) = since {
        logs.args(["--since", &since.to_string()]);
    }
    logs.arg(container(service));
    logs
}
