//! The box as one pod under `podman kube play` (testbox/pod.yaml), not Compose: that would be a
//! second definition of the box beside the pod. Revise if the box has to run on Docker.

use std::fs::{File, TryLockError};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::error::{Context, Error, Result, WithContext};
use crate::process::{self, Heard};
use testbox_tools::View;
use testbox_tools::process::describe;

/// The pod's name without an instance (`--instance`); an instance's is `<POD>.<instance>`, which
/// no `b33p-testbox-` prefix matches (an older xtask wipes the volumes by it) and no other
/// `b33p-testbox-*` image is named. It is pod.yaml's `metadata.name` (`@POD@`), after which kube
/// play names each container `<pod>-<container>` and the volumes are `<pod>-<claim>`; the
/// network's; and the images', `localhost/<pod>-<image>`.
const POD: &str = "b33p-testbox";
/// How much of a failed build's log its error shows.
const BUILD_TAIL: usize = 40;
/// The line of pod.yaml's container list the clients go in, all of it.
const CLIENTS: &str = "# @CLIENTS@";
/// The lines of pod.yaml's container and volume lists each b33p's go in
/// (testbox/b33p/pod.yaml, testbox/b33p/volumes.yaml).
const B33PS: &str = "# @B33PS@";
const B33P_VOLUMES: &str = "# @B33P_VOLUMES@";
/// The lines of pod.yaml's init containers, containers and volumes the tap's go in, with
/// `--capture` (testbox/tap); without, nothing.
const TAP_INIT: &str = "# @TAP_INIT@";
const TAP: &str = "# @TAP@";
const TAP_VOLUMES: &str = "# @TAP_VOLUMES@";
/// The most b33ps a box has (`--b33ps`).
pub const MAX_B33PS: u8 = 16;
/// The longest instance name: the names podman gives the pod's parts stay short.
const INSTANCE_MAX: usize = 24;

/// A container of the pod (testbox/pod.yaml, the clients' files).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Server,
    Storage,
    Cdn0,
    Presage,
    /// The `n`th b33p of [`Pod::play`]'s: every box has the first, a mesh several.
    B33p(u8),
    Desktop,
    Android,
    /// With `--capture`, the relay of the clients' TLS (testbox/tap).
    Tap,
}

/// The b33p every box has.
pub const B33P: Service = Service::B33p(0);

impl Service {
    /// Its container's name in pod.yaml: `b33p` for the first b33p, `b33p-<n>` for the others.
    pub fn name(self) -> String {
        match self {
            Self::Server => "server".to_owned(),
            Self::Storage => "storage".to_owned(),
            Self::Cdn0 => "cdn0".to_owned(),
            Self::Presage => "presage".to_owned(),
            Self::B33p(0) => "b33p".to_owned(),
            Self::B33p(n) => format!("b33p-{n}"),
            Self::Desktop => "desktop".to_owned(),
            Self::Android => "android".to_owned(),
            Self::Tap => "tap".to_owned(),
        }
    }

    /// The clients with a display, as pod.yaml's `view-<client>` volumes name them, by their
    /// containers' names; none for another container.
    fn screen(name: &str) -> Option<Self> {
        match name {
            "b33p" => Some(B33P),
            "desktop" => Some(Self::Desktop),
            "android" => Some(Self::Android),
            _ => name
                .strip_prefix("b33p-")
                .and_then(|n| n.parse().ok())
                .map(Self::B33p),
        }
    }
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
    B33p(u8),
    Desktop,
    Android,
    Tap,
}

impl Ready {
    fn service(self) -> Service {
        match self {
            Self::Server => Service::Server,
            Self::Storage => Service::Storage,
            Self::B33p(n) => Service::B33p(n),
            Self::Desktop => Service::Desktop,
            Self::Android => Service::Android,
            Self::Tap => Service::Tap,
        }
    }

    /// b33p-slint/src/agent.rs, testbox/desktop/driver.ts and testbox/tools `android` for the
    /// harness and the drivers.
    fn line(self) -> &'static str {
        match self {
            Self::Server => testbox_tools::SERVER_READY,
            Self::Storage => testbox_tools::STORAGE_READY,
            Self::B33p(_) => b33p_harness::LISTENING,
            Self::Desktop | Self::Android => testbox_tools::drivers::LISTENING,
            Self::Tap => testbox_tools::TAP_READY,
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
    /// Tagged `localhost/<pod>-<name>` ([`Pod::tag`]).
    name: &'static str,
    /// Relative to the repository.
    containerfile: &'static str,
    context: &'static str,
    /// Built only with this client.
    client: Option<Client>,
    /// Takes the build profile (`PROFILE`): b33p's.
    profiled: bool,
    /// Copies from the server's image, which its `SERVER` build argument names.
    from_server: bool,
}

impl Image {
    /// The defaults: the repository as the context, with every client.
    const REPOSITORY: Self = Self {
        name: "",
        containerfile: "",
        context: ".",
        client: None,
        profiled: false,
        from_server: false,
    };
}

/// The server's image, which the clients' copy the box's keys and tools from.
const SERVER: &str = "server";

/// How a build shares the CPU.
#[derive(Clone, Copy)]
enum Priority {
    Normal,
    /// `nice -n 10`.
    Lower,
}

const IMAGES: [Image; 10] = [
    Image {
        name: SERVER,
        containerfile: "testbox/server/Containerfile",
        ..Image::REPOSITORY
    },
    Image {
        name: "dynamodb",
        containerfile: "testbox/dynamodb/Containerfile",
        context: "testbox/dynamodb",
        ..Image::REPOSITORY
    },
    Image {
        name: "foundationdb",
        containerfile: "testbox/foundationdb/Containerfile",
        ..Image::REPOSITORY
    },
    Image {
        name: "tus",
        containerfile: "testbox/tus/Containerfile",
        context: "testbox/tus",
        ..Image::REPOSITORY
    },
    Image {
        name: "cdn0",
        containerfile: "testbox/garage/Containerfile",
        ..Image::REPOSITORY
    },
    Image {
        name: "storage",
        containerfile: "testbox/storage/Containerfile",
        ..Image::REPOSITORY
    },
    Image {
        name: "presage",
        containerfile: "testbox/presage/Containerfile",
        ..Image::REPOSITORY
    },
    Image {
        name: "b33p",
        containerfile: "testbox/b33p/Containerfile",
        profiled: true,
        from_server: true,
        ..Image::REPOSITORY
    },
    Image {
        name: "desktop",
        containerfile: "testbox/desktop/Containerfile",
        context: "testbox/desktop",
        client: Some(Client::Desktop),
        profiled: false,
        from_server: true,
    },
    Image {
        name: "android",
        containerfile: "testbox/android/Containerfile",
        client: Some(Client::Android),
        from_server: true,
        ..Image::REPOSITORY
    },
];

/// An image's build going on; dropped before [`Building::finish`], it is stopped.
pub struct Building {
    name: String,
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
            // Lossy: a build's output needn't be UTF-8, and its end is what tells why.
            let output = std::fs::read(&self.log)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
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
    /// [`POD`], or `<POD>.<instance>`.
    name: String,
    /// Where the host side of the box goes, relative to the repository: [`POD`]'s in
    /// `target/testbox-<what>`, an instance's in `target/testbox-<instance>-<what>`.
    target: String,
    /// pod.yaml with its paths and clients filled in: hostPath must be absolute.
    rendered: PathBuf,
    /// What tells of a lost message, from outside the containers' logs ([`Self::note_lost`]).
    lost: Mutex<Vec<String>>,
    /// Whether the boxes it plays relay the clients' TLS through the tap ([`Self::capture`]).
    capture: AtomicBool,
}

impl Pod {
    /// The box of `instance`, one of several on the host, each with a pod, network, volumes,
    /// images and directories of its own; without one, [`POD`]'s.
    pub fn new(repo: &Path, instance: Option<&str>) -> Result<Self> {
        let (name, target) = match instance {
            None => (POD.to_owned(), "target/testbox".to_owned()),
            Some(instance) => {
                let valid = instance.len() <= INSTANCE_MAX
                    && instance.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                    && instance
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                if !valid {
                    return Err(Error::Failed(format!(
                        "instance {instance:?}: up to {INSTANCE_MAX} lowercase letters, digits \
                         and dashes, starting with a letter or digit"
                    )));
                }
                (
                    format!("{POD}.{instance}"),
                    format!("target/testbox-{instance}"),
                )
            }
        };
        Ok(Self {
            repo: repo.to_owned(),
            rendered: repo.join(format!("{target}-pod.yaml")),
            name,
            target,
            lost: Mutex::default(),
            capture: AtomicBool::new(false),
        })
    }

    /// This box's `what` on the host ([`Self::target`]).
    fn target(&self, what: &str) -> PathBuf {
        self.repo.join(format!("{}-{what}", self.target))
    }

    /// The harness's and the drivers' sockets (testbox/pod.yaml volume `run`): the owner's only,
    /// as the clients' side and the xtask's are both the user's.
    pub fn run_dir(&self) -> PathBuf {
        self.target("run")
    }

    /// With `capture`, the boxes played from now on relay what the clients say to the servers
    /// over TLS through the tap, which writes it down in [`Self::tap_dir`] (testbox/tap).
    pub fn capture(&self, capture: bool) {
        self.capture.store(capture, Ordering::Relaxed);
    }

    pub fn capturing(&self) -> bool {
        self.capture.load(Ordering::Relaxed)
    }

    /// What the tap wrote down (testbox/tap/volumes.yaml), the box's since it played from
    /// nothing ([`Self::fresh`]).
    pub fn tap_dir(&self) -> PathBuf {
        self.target("tap")
    }

    /// The test files the xtask writes and the clients read (testbox/pod.yaml volume `files`).
    pub fn files_dir(&self) -> PathBuf {
        self.target("files")
    }

    /// What each box leaves for looking into it (xtask/src/checks/record.rs): `test`'s under
    /// `<box>`, `run`'s under `run/<when it started>/<round>/<box>`.
    pub fn record_dir(&self) -> PathBuf {
        self.target("record")
    }

    /// A directory per client for its display's socket, which only that client mounts
    /// (testbox/pod.yaml volumes `view-<client>`): no client reaches another's display.
    fn views_dir(&self) -> PathBuf {
        self.target("view")
    }

    /// `image`'s tag: this box's own, as each box builds its images from its own worktree.
    fn tag(&self, image: &str) -> String {
        format!("localhost/{}-{image}", self.name)
    }

    /// The lock [`Self::holding`] takes: the host's, for this box from any worktree.
    fn held(&self) -> PathBuf {
        let runtime =
            std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
        runtime.join(format!("{}.lock", self.name))
    }

    /// `optimized` builds b33p optimized as it ships (the `testbox` profile) instead of as the
    /// checks' own builds are.
    pub fn up(&self, clients: &[Client], b33ps: u8, optimized: bool) -> Result<()> {
        self.build(clients, optimized)?;
        self.play(clients, b33ps)
    }

    /// Builds the images the pod and `clients` run, b33p's `optimized` or not ([`Self::up`]).
    pub fn build(&self, clients: &[Client], optimized: bool) -> Result<()> {
        self.build_server()?;
        self.build_others(clients, optimized)
    }

    /// Builds the server's image, which the clients' copy from (`COPY --from`).
    pub fn build_server(&self) -> Result<()> {
        self.build_images(|image| image.name == SERVER, Priority::Normal, false)
    }

    /// Builds the images the pod and `clients` run but the server's, built already.
    pub fn build_others(&self, clients: &[Client], optimized: bool) -> Result<()> {
        self.build_images(
            |image| {
                image.name != SERVER && image.client.is_none_or(|client| clients.contains(&client))
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

    /// Starts building `image`, its output in a log of its own, `<image>.log` in
    /// `target/testbox-build` ([`Self::target`]): a build's output would bury what a run's
    /// checks print.
    fn start_build(&self, image: &Image, priority: Priority, optimized: bool) -> Result<Building> {
        let logs = self.target("build");
        std::fs::create_dir_all(&logs).context(logs.display().to_string())?;
        let tag = self.tag(image.name);
        let name = tag.rsplit('/').next().unwrap_or(&tag).to_owned();
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
        build.args(["build", "--jobs", "0", "-t", &tag]);
        if image.from_server {
            build
                .arg("--build-arg")
                .arg(format!("SERVER={}", self.tag(SERVER)));
        }
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

    /// Runs `change` with the box held: two commands that play or take down one pod at once
    /// break each other's, from one worktree or two. Fails at once if another process holds it.
    pub fn holding(&self, change: impl FnOnce() -> Result<()>) -> Result<()> {
        let path = self.held();
        let held = File::create(&path).context(path.display().to_string())?;
        match held.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(Error::Failed(format!(
                    "another cargo testbox up, test or down holds the box {} ({}); another box \
                     plays beside it with --instance",
                    self.name,
                    path.display()
                )));
            }
            Err(TryLockError::Error(error)) => {
                return Err(error).context(path.display().to_string());
            }
        }
        change()
    }

    /// Plays the pod with `clients` and `b33ps` b33ps (at least one), their images built.
    pub fn play(&self, clients: &[Client], b33ps: u8) -> Result<()> {
        // Not in pod.yaml: Kubernetes YAML has no network objects. Named after the pod: each box
        // on a network of its own, out of the other boxes' reach.
        let network = self.name.as_str();
        quietly(podman(&[
            "network",
            "create",
            "--ignore",
            "--internal",
            network,
        ]))?;
        // `--ignore` keeps a network of that name that was there: only if it has no egress.
        let internal = process::output(
            podman(&["network", "inspect", "--format", "{{.Internal}}", network]),
            "",
        )?;
        if internal.trim() != "true" {
            return Err(Error::Failed(format!(
                "the network {network} has egress: `podman network rm {network}`"
            )));
        }
        // The test files: the clients mount them read-only, so none can plant a link there for
        // the xtask to write through.
        let files = self.files_dir();
        std::fs::create_dir_all(&files).context(files.display().to_string())?;
        // The sockets: the owner's only, whatever is in it; a link a container plants there
        // leads nowhere (`testbox_tools::box_socket`).
        let run = self.run_dir();
        std::fs::create_dir_all(&run).context(run.display().to_string())?;
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700))
            .context(run.display().to_string())?;
        // The displays' sockets likewise; pod.yaml mounts each client's, so all of them exist.
        let screens = (0..b33ps.max(1))
            .map(Service::B33p)
            .chain([Service::Desktop, Service::Android]);
        for directory in
            std::iter::once(self.views_dir()).chain(screens.map(|screen| self.view_dir(screen)))
        {
            std::fs::create_dir_all(&directory).context(directory.display().to_string())?;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .context(directory.display().to_string())?;
        }
        if self.capturing() {
            let tap = self.tap_dir();
            std::fs::create_dir_all(&tap).context(tap.display().to_string())?;
            std::fs::set_permissions(&tap, std::fs::Permissions::from_mode(0o700))
                .context(tap.display().to_string())?;
        }
        testbox_tools::write(&self.rendered, self.render(clients, b33ps)?)?;
        // Each container's log in a file conmon writes as it comes (k8s-file), not journald's,
        // which falls up to a second behind under heavy disk writes (testbox/container-logs.md).
        // TESTBOX_LOG_DRIVER picks another, for testbox/logexp's comparison.
        let driver = std::env::var("TESTBOX_LOG_DRIVER").unwrap_or_else(|_| "k8s-file".to_owned());
        let mut play = podman(&[
            "kube",
            "play",
            "--replace",
            "--log-driver",
            &driver,
            "--network",
            network,
        ]);
        play.arg(&self.rendered);
        quietly(play)
    }

    /// [`Self::play`] from nothing: the pod and every volume removed first, and what
    /// [`Self::note_lost`] noted of the box before.
    pub fn fresh(&self, clients: &[Client], b33ps: u8) -> Result<()> {
        self.down(true)?;
        // The box before's capture went into its record ([`crate::checks`]' `keep_logs`).
        let tap = self.tap_dir();
        if tap.exists() {
            std::fs::remove_dir_all(&tap).context(tap.display().to_string())?;
        }
        self.lost
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.play(clients, b33ps)
    }

    /// pod.yaml with this box's names (`@POD@`) and directories (`@TARGET@`) filled in, the
    /// clients' containers in its [`CLIENTS`] line, each b33p's container and volumes in its
    /// [`B33PS`] and [`B33P_VOLUMES`] lines, and with `--capture` the tap's in its [`TAP_INIT`],
    /// [`TAP`] and [`TAP_VOLUMES`] lines.
    fn render(&self, clients: &[Client], b33ps: u8) -> Result<String> {
        let read = |path: &str| testbox_tools::read(&self.repo.join(path));
        let containers = clients
            .iter()
            .map(|client| read(&client.container()))
            .collect::<Result<String>>()?;
        let each_b33p = |path: &str| -> Result<String> {
            let template = read(path)?;
            Ok((0..b33ps.max(1))
                .map(|n| {
                    template
                        .replace("@B33P@", &Service::B33p(n).name())
                        .replace("@AGENT@", &agent_socket(n))
                })
                .collect())
        };
        let b33p_containers = each_b33p("testbox/b33p/pod.yaml")?;
        let b33p_volumes = each_b33p("testbox/b33p/volumes.yaml")?;
        let tap = |path: &str| {
            if self.capturing() {
                read(path)
            } else {
                Ok(String::new())
            }
        };
        let (tap_init, tap_container, tap_volumes) = (
            tap("testbox/tap/init.yaml")?,
            tap("testbox/tap/pod.yaml")?,
            tap("testbox/tap/volumes.yaml")?,
        );
        let pod: String = read("testbox/pod.yaml")?
            .lines()
            .map(|line| match line.trim() {
                CLIENTS => containers.clone(),
                B33PS => b33p_containers.clone(),
                B33P_VOLUMES => b33p_volumes.clone(),
                TAP_INIT => tap_init.clone(),
                TAP => tap_container.clone(),
                TAP_VOLUMES => tap_volumes.clone(),
                _ => format!("{line}\n"),
            })
            .collect();
        Ok(pod
            .replace("@TARGET@", &self.repo.join(&self.target).to_string_lossy())
            .replace("@POD@", &self.name))
    }

    /// Where `screen`'s display serves on the host: its `with-display` directory, /run/view.
    pub fn view_dir(&self, screen: Service) -> PathBuf {
        self.views_dir().join(screen.name())
    }

    /// The clients with a display that run, as [`Self::containers`] says.
    pub fn screens(&self) -> Vec<Service> {
        self.all_screens()
            .into_iter()
            .filter(|screen| self.running(*screen))
            .collect()
    }

    /// The clients with a display, as [`Self::containers`] says, stopped ones too: one between
    /// a crash and its restart (`restartPolicy: Always`) still has its log.
    pub fn all_screens(&self) -> Vec<Service> {
        let prefix = format!("{}-", self.name);
        let mut screens: Vec<Service> = self
            .containers()
            .iter()
            .filter_map(|name| Service::screen(name.strip_prefix(&prefix)?))
            .collect();
        screens.sort_by_key(|screen| match screen {
            Service::B33p(n) => u16::from(*n),
            _ => u16::MAX,
        });
        screens
    }

    /// The b33ps, stopped ones too: [`Self::all_screens`]' b33ps.
    pub fn b33ps(&self) -> Vec<Service> {
        self.all_screens()
            .into_iter()
            .filter(|screen| matches!(screen, Service::B33p(_)))
            .collect()
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
                &self.container(service),
            ]),
            "",
        );
        state.is_ok_and(|running| running.trim() == "true")
    }

    /// `--force` also removes the persistentVolumeClaims' volumes. kube down needs the YAML the
    /// pod was played from, which another worktree's `up` wrote in its own `target`: then the
    /// pod and, to wipe, the volumes go by their names, pod.yaml's claims. Not by a prefix: the
    /// default box's would take every instance's.
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
            podman(&[
                "pod",
                "ps",
                "-q",
                "--filter",
                &format!("name=^{}$", self.name),
            ]),
            "",
        )?;
        if !pod.trim().is_empty() {
            quietly(podman(&["pod", "rm", "--force", &self.name]))?;
        }
        if wipe {
            // Every b33p's too, however many this box had.
            let claims = claims(&self.render(&[], MAX_B33PS)?);
            let present = process::output(podman(&["volume", "ls", "-q"]), "")?;
            let volumes: Vec<&str> = present
                .split_whitespace()
                .filter(|volume| claims.iter().any(|claim| claim == volume))
                .collect();
            if !volumes.is_empty() {
                quietly(podman(&[&["volume", "rm"][..], &volumes].concat()))?;
            }
        }
        Ok(())
    }

    /// `service`'s log so far, what it wrote to stdout and stderr alike.
    pub fn log(&self, service: Service) -> Result<String> {
        self.log_named(&self.container(service))
    }

    /// The pod's containers by name, stopped ones too; none if podman can't say.
    pub fn containers(&self) -> Vec<String> {
        let listed = process::output(
            podman(&[
                "ps",
                "--all",
                "--filter",
                &format!("pod={}", self.name),
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

    /// The `cpu.stat` of the container `name`'s cgroup, read from the host: the CPU time it had,
    /// and how often and how long it was throttled.
    pub fn cpu_stat_named(&self, name: &str) -> Result<String> {
        let cgroup = process::output(
            podman(&[
                "container",
                "inspect",
                "--format",
                "{{.State.CgroupPath}}",
                name,
            ]),
            "",
        )?;
        let stat = Path::new("/sys/fs/cgroup")
            .join(cgroup.trim().trim_start_matches('/'))
            .join("cpu.stat");
        std::fs::read_to_string(&stat).context(stat.display().to_string())
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
            process::run(podman(&["logs", &format!("{}-{service}", self.name)]))?;
        }
        Ok(())
    }

    /// Runs `argv` in `service`, without a shell, with `stdin` as its input; returns stdout, and
    /// fails with what it printed on a non-zero exit.
    pub fn exec(&self, service: Service, argv: &[&str], stdin: &str) -> Result<String> {
        let mut exec = podman(&["exec", "-i", &self.container(service)]);
        exec.args(argv);
        process::output(exec, stdin)
    }

    /// Runs `argv` in `service` without a shell, and returns how it ended, whatever that was.
    pub fn exec_status(&self, service: Service, argv: &[&str]) -> Result<std::process::Output> {
        let mut exec = podman(&["exec", &self.container(service)]);
        exec.args(argv);
        exec.output()
            .with_context(|| format!("podman exec {}", argv.join(" ")))
    }

    /// Freezes `service` until [`Self::unpause`]: its connections stay open, and nothing answers
    /// on them, as when a network goes away without a word.
    pub fn pause(&self, service: Service) -> Result<()> {
        quietly(podman(&["pause", &self.container(service)]))
    }

    pub fn unpause(&self, service: Service) -> Result<()> {
        quietly(podman(&["unpause", &self.container(service)]))
    }

    pub fn restart(&self, service: Service) -> Result<()> {
        quietly(podman(&["restart", &self.container(service)]))
    }

    /// Stops `service` as `podman stop` does: its stop signal, then SIGKILL after the timeout.
    pub fn stop(&self, service: Service) -> Result<()> {
        quietly(podman(&["stop", &self.container(service)]))
    }

    pub fn start(&self, service: Service) -> Result<()> {
        quietly(podman(&["start", &self.container(service)]))
    }

    /// Ends every process in `service` at once, as a power cut would: nothing gets to flush or
    /// close. `with-display` goes with its client, so the crash check doesn't count it.
    pub fn kill(&self, service: Service) -> Result<()> {
        quietly(podman(&[
            "kill",
            "--signal",
            "KILL",
            &self.container(service),
        ]))
    }

    /// What `service` uses of memory, as `podman stats` says it ("212.4MB / 33.3GB").
    pub fn memory(&self, service: Service) -> Result<String> {
        let stats = podman(&[
            "stats",
            "--no-stream",
            "--format",
            "{{.MemUsage}}",
            &self.container(service),
        ]);
        process::output(stats, "").map(|usage| usage.trim().to_owned())
    }

    /// Lets `service` take at most `mib` MiB, swap included: past it, the kernel kills it.
    pub fn limit_memory(&self, service: Service, mib: u64) -> Result<()> {
        let limit = format!("{mib}m");
        quietly(podman(&[
            "update",
            "--memory",
            &limit,
            "--memory-swap",
            &limit,
            &self.container(service),
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
            match process::await_lines(self.follow_logs(service, since), needle, times, left)? {
                Heard::Found => return Ok(()),
                Heard::Ended => {
                    if let Some(failure) = self.failure(service, since)? {
                        return Err(Error::Failed(format!(
                            "{} failed before it said {needle:?}:\n{failure}",
                            service.name()
                        )));
                    }
                    let running =
                        podman(&["wait", "--condition", "running", &self.container(service)]);
                    process::output(running, "")?;
                }
            }
        }
    }
}

/// Runs `command` to success, what it printed logged rather than shown: podman's account of what
/// it did would break up a run's report. Its warnings too, which go to stderr, such as `stop`'s
/// that it had to kill.
fn quietly(mut command: Command) -> Result<()> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .context(describe(&command))?;
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(Error::Command {
            command: describe(&command),
            output: format!(" ({})\n{printed}", output.status),
        });
    }
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

impl Pod {
    /// What kube play names `service`'s container.
    fn container(&self, service: Service) -> String {
        format!("{}-{}", self.name, service.name())
    }

    /// `logs --follow` of `service`, from `since` on (seconds since the epoch), or from its
    /// start.
    /// How `service`'s application failed on its own after `since`, if it did, as `with-display`
    /// says it: the end of its log up to that line. Restarted, it would most likely fail again,
    /// or wait for what its first run left behind, until the wait's patience ran out.
    fn failure(&self, service: Service, since: Option<u64>) -> Result<Option<String>> {
        let mut logs = podman(&["logs"]);
        if let Some(since) = since {
            logs.args(["--since", &since.to_string()]);
        }
        logs.arg(self.container(service));
        let output = logs
            .stdin(Stdio::null())
            .output()
            .context(describe(&logs))?;
        Ok(failure(&format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }

    fn follow_logs(&self, service: Service, since: Option<u64>) -> Command {
        let mut logs = podman(&["logs", "--follow"]);
        if let Some(since) = since {
            logs.args(["--since", &since.to_string()]);
        }
        logs.arg(self.container(service));
        logs
    }
}

/// The last lines of `log` up to the first that says its application failed
/// ([`testbox_tools::APPLICATION_FAILED`]), if one does.
fn failure(log: &str) -> Option<String> {
    /// Enough for the application's own last words before `with-display`'s.
    const TAIL: usize = 20;
    let lines: Vec<&str> = log.lines().collect();
    let failed = lines
        .iter()
        .position(|line| line.contains(testbox_tools::APPLICATION_FAILED))?;
    Some(lines[failed.saturating_sub(TAIL)..=failed].join("\n"))
}

/// The file name of the `n`th b33p's harness socket in the run directory (`B33P_AGENT_SOCKET`).
pub fn agent_socket(n: u8) -> String {
    match n {
        0 => "agent.sock".to_owned(),
        n => format!("agent-{n}.sock"),
    }
}

/// The volumes a rendered pod.yaml claims (`claimName`), by name.
fn claims(pod: &str) -> Vec<String> {
    pod.split("claimName: \"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_application_ends_the_wait_with_its_last_words() {
        let log = format!(
            "booting\nandroid: adb root: closed\nwith-display: {}: android exited: exit status: 1\nbooting\n",
            testbox_tools::APPLICATION_FAILED
        );
        let last_words = failure(&log).expect("a failure");
        assert!(last_words.starts_with("booting\nandroid: adb root: closed"));
        assert!(last_words.ends_with("exit status: 1"));
        assert_eq!(
            failure("booting\nwith-display: cage exited: signal: 9\n"),
            None
        );
    }

    /// The repository xtask is in, for pod.yaml.
    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives in the workspace root")
            .to_owned()
    }

    /// A second hold fails while the first is held, and succeeds once it ended; the host's, so
    /// from any worktree.
    #[test]
    fn the_box_is_held_once() {
        let instance = format!("hold-{}", std::process::id());
        let pod = Pod::new(Path::new("/one"), Some(&instance)).expect("an instance");
        let elsewhere = Pod::new(Path::new("/two"), Some(&instance)).expect("an instance");
        pod.holding(|| {
            assert!(
                elsewhere.holding(|| Ok(())).is_err(),
                "a second hold while the first is held"
            );
            Ok(())
        })
        .expect("the first hold");
        elsewhere
            .holding(|| Ok(()))
            .expect("a hold once the first ended");
        std::fs::remove_file(pod.held()).expect("the lock removed");
    }

    /// Without an instance, the names and directories are those of before instances.
    #[test]
    fn the_default_box_keeps_its_names() {
        let repo = repo();
        let pod = Pod::new(&repo, None).expect("the default box");
        assert_eq!(pod.container(B33P), "b33p-testbox-b33p");
        assert_eq!(pod.tag(SERVER), "localhost/b33p-testbox-server");
        assert_eq!(pod.run_dir(), repo.join("target/testbox-run"));
        assert_eq!(pod.record_dir(), repo.join("target/testbox-record"));
        let rendered = pod.render(&[], 1).expect("pod.yaml");
        assert!(rendered.contains(r#"name: "b33p-testbox","#), "{rendered}");
        assert!(rendered.contains(r#"image: "localhost/b33p-testbox-b33p","#));
        assert!(rendered.contains(&format!(r#"path: "{}/target/testbox-run""#, repo.display())));
    }

    /// An instance's names and directories are its own, and pod.yaml leaves none of the
    /// default box's: two boxes share nothing.
    #[test]
    fn an_instance_shares_nothing_with_the_default_box() {
        let repo = repo();
        let pod = Pod::new(&repo, Some("two")).expect("an instance");
        assert_eq!(pod.container(B33P), "b33p-testbox.two-b33p");
        assert_eq!(pod.tag("b33p"), "localhost/b33p-testbox.two-b33p");
        assert_eq!(pod.run_dir(), repo.join("target/testbox-two-run"));
        assert_eq!(pod.files_dir(), repo.join("target/testbox-two-files"));
        let rendered = pod
            .render(&[Client::Desktop, Client::Android], 2)
            .expect("pod.yaml");
        for line in rendered
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
        {
            assert!(
                !line.contains("@POD@") && !line.contains("@TARGET@") && !line.contains("@REPO@"),
                "a placeholder left: {line}"
            );
            for word in line.split('"').filter(|word| word.contains("b33p-testbox")) {
                assert!(
                    word.contains("b33p-testbox.two"),
                    "the default box's {word:?} in {line:?}"
                );
            }
            if line.contains("hostPath") {
                assert!(line.contains("/target/testbox-two-"), "{line}");
            }
        }
    }

    /// A wipe takes a box's own volumes, by their names: the default box's not an instance's,
    /// though theirs start alike.
    #[test]
    fn a_wipe_takes_only_its_own_volumes() {
        let repo = repo();
        let default = claims(&Pod::new(&repo, None).unwrap().render(&[], 2).unwrap());
        let two = claims(
            &Pod::new(&repo, Some("two"))
                .unwrap()
                .render(&[], 2)
                .unwrap(),
        );
        assert!(
            default.contains(&"b33p-testbox-keys".to_owned()),
            "{default:?}"
        );
        assert!(two.contains(&"b33p-testbox.two-keys".to_owned()), "{two:?}");
        assert_eq!(default.len(), two.len());
        assert!(default.iter().all(|claim| !two.contains(claim)));
        // What an older xtask's wipe takes, `name=^b33p-testbox-`, is none of an instance's.
        assert!(two.iter().all(|claim| !claim.starts_with("b33p-testbox-")));
    }

    #[test]
    fn an_instance_is_a_short_plain_name() {
        for good in ["2", "tour", "pr-82"] {
            assert!(Pod::new(Path::new("/r"), Some(good)).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-x",
            "Tour",
            "a b",
            "a/b",
            "a_b",
            &"x".repeat(INSTANCE_MAX + 1),
        ] {
            assert!(Pod::new(Path::new("/r"), Some(bad)).is_err(), "{bad:?}");
        }
    }

    /// The tap is in the pod only with `--capture`: its redirect, its relay and its directory.
    #[test]
    fn the_tap_is_there_only_when_capturing() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the workspace");
        let pod = Pod::new(repo, None).expect("the default box");
        let tapped = [
            r#"name: "tap-redirect","#,
            r#"name: "tap","#,
            "target/testbox-tap\"",
        ];
        let plain = pod.render(&[], 1).expect("a pod");
        for part in tapped {
            assert!(
                !plain.contains(part),
                "{part} without --capture in\n{plain}"
            );
        }
        pod.capture(true);
        let captured = pod.render(&[], 1).expect("a pod that captures");
        for part in tapped {
            assert!(captured.contains(part), "{part} in\n{captured}");
        }
    }

    /// Each b33p gets a container, a home, a display and a harness socket of its own; the first
    /// keeps the names a box of one b33p always had.
    #[test]
    fn each_b33p_is_rendered_apart() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the workspace");
        let pod = Pod::new(repo, None).expect("the default box");
        let one = pod.render(&[], 1).expect("a pod of one b33p");
        for kept in [
            r#"name: "b33p","#,
            r#"value: "/run/b33p/agent.sock""#,
            r#"claimName: "b33p-testbox-b33p-home""#,
            "target/testbox-view/b33p\"",
        ] {
            assert!(one.contains(kept), "{kept} in\n{one}");
        }
        assert!(
            !one.contains(r#"name: "b33p-1""#),
            "a second b33p in\n{one}"
        );
        let three = pod
            .render(&[Client::Desktop], 3)
            .expect("a pod of three b33ps");
        for n in 1..=2 {
            for named in [
                format!(r#"name: "b33p-{n}","#),
                format!(r#"value: "/run/b33p/agent-{n}.sock""#),
                format!(r#"claimName: "b33p-testbox-b33p-{n}-home""#),
                format!(r#"name: "view-b33p-{n}", hostPath"#),
            ] {
                assert!(three.contains(&named), "{named} in\n{three}");
            }
        }
        assert!(three.contains(r#"name: "desktop","#));
        for placeholder in ["@B33P@", "@AGENT@", "@REPO@"] {
            assert!(
                !three.contains(placeholder),
                "{placeholder} left in\n{three}"
            );
        }
        for line in [CLIENTS, B33PS, B33P_VOLUMES] {
            assert!(
                !three.lines().any(|each| each.trim() == line),
                "{line} left in\n{three}"
            );
        }
    }
}
