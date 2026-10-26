//! The box as one pod under `podman kube play` (testbox/pod.yaml), not Compose: that would be a
//! second definition of the box beside the pod. Revise if the box has to run on Docker.

use std::fs::{File, TryLockError};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::error::{Context, Error, Result, WithContext};
use crate::process::{self, Heard};
use testbox_tools::View;

/// pod.yaml's `metadata.name`; kube play names each container `<pod>-<container>`.
const POD: &str = "b33p-testbox";
const NETWORK: &str = "b33p-testbox";
/// The harness's and the drivers' sockets (testbox/pod.yaml volume `run`): the owner's only, as
/// the clients' side and the xtask's are both the user's.
pub const RUN: &str = "target/testbox-run";
/// The test files the xtask writes and the clients read (testbox/pod.yaml volume `files`).
pub const FILES: &str = "target/testbox-files";
/// What each box leaves for looking into it (xtask/src/checks/record.rs): `test`'s under
/// `<box>`, `run`'s under `run/<when it started>/<round>/<box>`.
pub const RECORD: &str = "target/testbox-record";
/// Each image's build output, as `<image>.log`.
const BUILD_LOGS: &str = "target/testbox-build";
/// The lock [`Pod::holding`] takes.
const HELD: &str = "target/testbox.lock";
/// How much of a failed build's log its error shows.
const BUILD_TAIL: usize = 40;
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

/// The server's image, which the clients' copy the box's keys and tools from.
const SERVER: &str = "localhost/b33p-testbox-server";

/// How a build shares the CPU.
#[derive(Clone, Copy)]
enum Priority {
    Normal,
    /// `nice -n 10`.
    Lower,
}

const IMAGES: [Image; 10] = [
    Image {
        tag: SERVER,
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

/// An image's build going on; dropped before [`Building::finish`], it is stopped.
pub struct Building {
    name: &'static str,
    log: PathBuf,
    started: Instant,
    child: Option<Child>,
}

impl Building {
    /// Waits for the build to end; fails with the end of its log if it failed.
    pub fn finish(mut self) -> Result<()> {
        let mut child = self.child.take().ok_or("the build was stopped")?;
        let status = child
            .wait()
            .context(format!("podman build {}", self.name))?;
        if !status.success() {
            let output = testbox_tools::read(&self.log).unwrap_or_default();
            let lines: Vec<&str> = output.lines().collect();
            let tail = lines[lines.len().saturating_sub(BUILD_TAIL)..].join("\n");
            return Err(Error::Failed(format!(
                "building {} failed ({status}); the end of {}:\n{tail}",
                self.name,
                self.log.display()
            )));
        }
        info!(
            "built {} in {:.0} s",
            self.name,
            self.started.elapsed().as_secs_f64()
        );
        Ok(())
    }
}

impl Drop for Building {
    /// With SIGTERM, which ends the build at once; podman keeps its working container
    /// (`podman ps --all --external`).
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        warn!("stopping the build of {}", self.name);
        let mut kill = Command::new("kill");
        kill.args(["-TERM", &child.id().to_string()]);
        if let Err(error) = process::run(kill) {
            warn!("{error}");
        }
        if let Err(error) = child.wait() {
            warn!("the build of {}: {error}", self.name);
        }
    }
}

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

    /// `optimized` builds b33p optimized as it ships (the `testbox` profile) instead of as the
    /// checks' own builds are.
    pub fn up(&self, clients: &[Client], optimized: bool) -> Result<()> {
        self.build(clients, optimized)?;
        self.play(clients)
    }

    /// Builds the images the pod and `clients` run, b33p's `optimized` or not ([`Self::up`]).
    pub fn build(&self, clients: &[Client], optimized: bool) -> Result<()> {
        self.build_server()?;
        self.build_others(clients, optimized)
    }

    /// Builds the server's image, which the clients' copy from (`COPY --from`).
    pub fn build_server(&self) -> Result<()> {
        self.build_images(|image| image.tag == SERVER, Priority::Normal, false)
    }

    /// Builds the images the pod and `clients` run but the server's, built already.
    pub fn build_others(&self, clients: &[Client], optimized: bool) -> Result<()> {
        self.build_images(
            |image| {
                image.tag != SERVER && image.client.is_none_or(|client| clients.contains(&client))
            },
            Priority::Normal,
            optimized,
        )
    }

    /// Starts building `client`'s image, the server's built already, beside a run's checks: at
    /// a lower CPU priority, as the checks' waits are bounded and the build's isn't.
    pub fn build_beside(&self, client: Client) -> Result<Building> {
        let image = IMAGES
            .iter()
            .find(|image| image.client == Some(client))
            .ok_or("no image of that client")?;
        self.start_build(image, Priority::Lower, false)
    }

    /// Builds each image `wanted` takes, one after the other.
    fn build_images(
        &self,
        wanted: impl Fn(&Image) -> bool,
        priority: Priority,
        optimized: bool,
    ) -> Result<()> {
        for image in IMAGES.iter().filter(|image| wanted(image)) {
            self.start_build(image, priority, optimized)?.finish()?;
        }
        Ok(())
    }

    /// Starts building `image`, its output in a log of its own under [`BUILD_LOGS`]: a build's
    /// output would bury what a run's checks print.
    fn start_build(&self, image: &Image, priority: Priority, optimized: bool) -> Result<Building> {
        let logs = self.repo.join(BUILD_LOGS);
        std::fs::create_dir_all(&logs).context(logs.display().to_string())?;
        let name = image.tag.rsplit('/').next().unwrap_or(image.tag);
        let path = logs.join(format!("{name}.log"));
        let log = File::create(&path).context(path.display().to_string())?;
        let mut build = match priority {
            Priority::Normal => podman(&[]),
            Priority::Lower => {
                let mut nice = Command::new("nice");
                nice.args(["-n", "10", "podman"]);
                nice
            }
        };
        // Stages that don't depend on each other at once (a Maven build and a cargo one).
        build.args(["build", "--jobs", "0", "-t", image.tag]);
        if image.profiled {
            let profile = if optimized { "testbox" } else { "dev" };
            build.arg("--build-arg").arg(format!("PROFILE={profile}"));
        }
        build
            .arg("-f")
            .arg(self.repo.join(image.containerfile))
            .arg(self.repo.join(image.context))
            .stdout(log.try_clone().context(path.display().to_string())?)
            .stderr(log);
        info!("building {name}: {}", path.display());
        let child = build.spawn().context(format!("podman build {name}"))?;
        Ok(Building {
            name,
            log: path,
            started: Instant::now(),
            child: Some(child),
        })
    }

    /// Runs `change` with the box held: two commands that play or take down the one pod at once
    /// break each other's. Fails at once if another process holds it.
    pub fn holding(&self, change: impl FnOnce() -> Result<()>) -> Result<()> {
        let path = self.repo.join(HELD);
        let held = File::create(&path).context(path.display().to_string())?;
        match held.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(Error::Failed(format!(
                    "another cargo testbox up, test or down holds the box ({})",
                    path.display()
                )));
            }
            Err(TryLockError::Error(error)) => {
                return Err(error).context(path.display().to_string());
            }
        }
        change()
    }

    /// Plays the pod with `clients`, their images built.
    pub fn play(&self, clients: &[Client]) -> Result<()> {
        // Not in pod.yaml: Kubernetes YAML has no network objects.
        quietly(podman(&[
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
        for directory in
            std::iter::once(self.repo.join(VIEW)).chain(SCREENS.map(|screen| self.view_dir(screen)))
        {
            std::fs::create_dir_all(&directory).context(directory.display().to_string())?;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .context(directory.display().to_string())?;
        }
        testbox_tools::write(&self.rendered, self.render(clients)?)?;
        let mut play = podman(&["kube", "play", "--replace", "--network", NETWORK]);
        play.arg(&self.rendered);
        quietly(play)
    }

    /// [`Self::play`] from nothing: the pod and every volume removed first.
    pub fn fresh(&self, clients: &[Client]) -> Result<()> {
        self.down(true)?;
        self.play(clients)
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

    /// Where `screen`'s display serves on the host: its `with-display` directory, /run/view.
    pub fn view_dir(&self, screen: Service) -> PathBuf {
        let name: &'static str = screen.into();
        self.repo.join(VIEW).join(name)
    }

    /// The socket in [`Self::view_dir`] that takes the viewer's input if `interactive`, else
    /// the one that only shows.
    pub fn view_socket(&self, screen: Service, interactive: bool) -> PathBuf {
        let view = if interactive {
            View::Interact
        } else {
            View::Watch
        };
        let socket: &'static str = view.into();
        self.view_dir(screen).join(socket)
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
            quietly(down)?;
        }
        let pod = process::output(
            podman(&["pod", "ps", "-q", "--filter", &format!("name=^{POD}$")]),
            "",
        )?;
        if !pod.trim().is_empty() {
            quietly(podman(&["pod", "rm", "--force", POD]))?;
        }
        if wipe {
            let volumes = process::output(
                podman(&["volume", "ls", "-q", "--filter", &format!("name=^{POD}-")]),
                "",
            )?;
            let volumes: Vec<&str> = volumes.split_whitespace().collect();
            if !volumes.is_empty() {
                quietly(podman(&[&["volume", "rm"][..], &volumes].concat()))?;
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
        quietly(podman(&["restart", &container(service)]))
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

/// Runs `command` to success, what it printed logged rather than shown: podman's account of what
/// it did would break up a run's report.
fn quietly(command: Command) -> Result<()> {
    let printed = process::output(command, "")?;
    let printed = printed.trim_end();
    if !printed.is_empty() {
        debug!("{printed}");
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A second hold fails while the first is held, and succeeds once it ended.
    #[test]
    fn the_box_is_held_once() {
        let repo = std::env::temp_dir().join(format!("testbox-hold-{}", std::process::id()));
        std::fs::create_dir_all(repo.join("target")).expect("a scratch repository");
        let pod = Pod::new(&repo);
        pod.holding(|| {
            assert!(
                pod.holding(|| Ok(())).is_err(),
                "a second hold while the first is held"
            );
            Ok(())
        })
        .expect("the first hold");
        pod.holding(|| Ok(())).expect("a hold once the first ended");
        std::fs::remove_dir_all(&repo).expect("the scratch repository removed");
    }
}
