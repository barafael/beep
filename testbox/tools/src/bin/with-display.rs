//! Runs a command on a display of its own, which `cargo testbox vnc` shows: cage, a Wayland kiosk
//! compositor whose one application fills the display, without a screen, served by wayvnc on a
//! Unix socket per [`View`] in a directory. Nothing of the clipboard reaches a viewer or comes
//! from one: wayvnc reaches it only through a data-control protocol, and the session refuses a
//! compositor that offers one.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::os::fd::BorrowedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};

use rustix::fs::Mode;
use rustix::process::{Pid, Signal};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use strum::VariantArray;
use testbox_tools::View;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};

const USAGE: &str = "with-display <directory> <width>x<height> -- <command> [argument...]";
/// The runtime directory of cage, wayvnc and the application, the container's alone.
const RUNTIME: &str = "/tmp/with-display";
/// The application's pid, in [`RUNTIME`]: for the stop signals.
const APPLICATION: &str = "application.pid";
/// wlroots' one headless output.
const OUTPUT: &str = "HEADLESS-1";
/// What a client reads and sets the clipboard through without a window of its own, as wayvnc
/// does.
const DATA_CONTROL: [&str; 2] = [
    "zwlr_data_control_manager_v1",
    "ext_data_control_manager_v1",
];

/// What keeps the display from serving, or ends it with a failure.
#[derive(Debug, thiserror::Error)]
enum DisplayError {
    #[error("usage: {USAGE}")]
    Usage,
    #[error("creating {}", path.display())]
    CreateRuntime {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("finding with-display's own path")]
    OwnPath(#[source] std::io::Error),
    #[error("keeping the descriptors cage gave the session from what the session starts")]
    Descriptors(#[source] std::io::Error),
    #[error("starting {program}")]
    Start {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("waiting for {program}")]
    Wait {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("stopping wayvnc")]
    StopWayvnc(#[source] std::io::Error),
    #[error("cage's pid {pid}, not a process group's")]
    CagePid { pid: u32 },
    #[error("setting up the stop signals' handler")]
    Signals(#[source] std::io::Error),
    #[error("cage exited: {status}")]
    CageExited { status: ExitStatus },
    /// The application's own failure: [`testbox_tools::APPLICATION_FAILED`] first, which what
    /// waits for the application looks for.
    #[error("{}: {program} exited: {status}", testbox_tools::APPLICATION_FAILED)]
    ApplicationExited { program: String, status: ExitStatus },
}

/// What keeps the session from refusing a compositor that would share the clipboard.
#[derive(Debug, thiserror::Error)]
enum DataControlError {
    #[error("connecting to cage")]
    Connect(#[source] wayland_client::ConnectError),
    #[error("reading cage's globals")]
    Globals(#[source] wayland_client::globals::GlobalError),
    #[error(
        "cage offers {protocol}, through which wayvnc would share the clipboard with the viewers"
    )]
    Offered { protocol: &'static str },
}

fn main() -> std::process::ExitCode {
    testbox_tools::exit("with-display", run())
}

type Failure = Box<dyn std::error::Error + Send + Sync>;

/// `with-display session …`, with the same arguments, is what cage runs.
fn run() -> Result<(), Failure> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (in_cage, arguments) = match arguments.split_first() {
        Some((first, rest)) if first == "session" => (true, rest),
        _ => (false, arguments.as_slice()),
    };
    match arguments {
        [
            directory,
            geometry,
            separator,
            program,
            program_arguments @ ..,
        ] if separator == "--" => {
            let directory = Path::new(directory);
            if in_cage {
                session(directory, geometry, program, program_arguments)
            } else {
                serve(directory, geometry, program, program_arguments)
            }
        }
        _ => Err(DisplayError::Usage.into()),
    }
}

fn serve(
    directory: &Path,
    geometry: &str,
    program: &str,
    program_arguments: &[String],
) -> Result<(), Failure> {
    // The sockets, the runtime directory and the application's files are the owner's only.
    rustix::process::umask(Mode::from_raw_mode(0o077));
    // Left by this container before it restarted (`restartPolicy: Always`): wayvnc doesn't bind
    // over one.
    for &view in View::VARIANTS {
        let name: &'static str = view.into();
        testbox_tools::remove_if_present(&directory.join(name))?;
    }
    std::fs::create_dir_all(RUNTIME).map_err(|source| DisplayError::CreateRuntime {
        path: RUNTIME.into(),
        source,
    })?;
    // The application's pid, likewise when the container was killed with it: until the session
    // writes the new one, a stop signal would go to whatever has that pid now.
    testbox_tools::remove_if_present(&Path::new(RUNTIME).join(APPLICATION))?;
    let this = std::env::current_exe().map_err(DisplayError::OwnPath)?;
    let mut cage = Command::new("cage")
        .arg("-d")
        .arg("--")
        .arg(this)
        .arg("session")
        .arg(directory)
        .arg(geometry)
        .arg("--")
        .arg(program)
        .args(program_arguments)
        .env("XDG_RUNTIME_DIR", RUNTIME)
        // No screen in the container, and no GPU unless it is given one: `WITH_DISPLAY_RENDERER=gles2`
        // (with a render node passed in) draws the display on it, which clients that present
        // through the GPU's buffers, as GPUI does, need to run on it too.
        .env("WLR_BACKENDS", "headless")
        .env(
            "WLR_RENDERER",
            std::env::var("WITH_DISPLAY_RENDERER").unwrap_or_else(|_| "pixman".to_owned()),
        )
        .env("WLR_LIBINPUT_NO_DEVICES", "1")
        .process_group(0)
        .spawn()
        .map_err(|source| DisplayError::Start {
            program: "cage".to_owned(),
            source,
        })?;
    stop_with_container(&cage)?;
    let status = cage.wait().map_err(|source| DisplayError::Wait {
        program: "cage".to_owned(),
        source,
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(DisplayError::CageExited { status }.into())
    }
}

/// Passes the stop signals the container gets (SIGTERM, SIGINT, SIGHUP) on to the application,
/// so that `podman stop` and `restart` let it end cleanly (b33p closes its database); the
/// session then ends, and cage with it. Before the application runs, there is nothing to end
/// cleanly: they go to cage's whole process group. As PID 1 without a handler, this process would
/// ignore them.
fn stop_with_container(cage: &Child) -> Result<(), DisplayError> {
    let group = i32::try_from(cage.id())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or(DisplayError::CagePid { pid: cage.id() })?;
    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP]).map_err(DisplayError::Signals)?;
    std::thread::spawn(move || {
        for raw in signals.forever() {
            let Some(signal) = Signal::from_named_raw(raw) else {
                continue;
            };
            let sent = match application() {
                Some(application) => rustix::process::kill_process(application, signal),
                None => rustix::process::kill_process_group(group, signal),
            };
            if let Err(error) = sent {
                eprintln!("with-display: signal {raw}: {error}");
            }
        }
    });
    Ok(())
}

/// In cage: the display's size, a VNC server per view, then the application until it exits.
fn session(
    directory: &Path,
    geometry: &str,
    program: &str,
    program_arguments: &[String],
) -> Result<(), Failure> {
    keep_descriptors_from_children()?;
    refuse_data_control()?;
    let mut wlr_randr = Command::new("wlr-randr");
    wlr_randr.args(["--output", OUTPUT, "--custom-mode", geometry]);
    testbox_tools::process::run(wlr_randr)?;
    let servers = View::VARIANTS
        .iter()
        .map(|&view| wayvnc(directory, view))
        .collect::<Result<Vec<Child>, DisplayError>>()?;
    // A group of its own: what it leaves running when it exits goes with it below.
    let mut application = Command::new(program)
        .args(program_arguments)
        .env(testbox_tools::DISPLAY_SIZE, geometry)
        .process_group(0)
        .spawn()
        .map_err(|source| DisplayError::Start {
            program: program.to_owned(),
            source,
        })?;
    let pid = Path::new(RUNTIME).join(APPLICATION);
    testbox_tools::write(&pid, application.id().to_string())?;
    let status = application.wait().map_err(|source| DisplayError::Wait {
        program: program.to_owned(),
        source,
    })?;
    testbox_tools::remove_if_present(&pid)?;
    // Left running, its children (the Android driver's emulator) keep cage, and so the
    // container, up: it would look alive to whatever waits for it, doing nothing.
    if let Some(group) = i32::try_from(application.id()).ok().and_then(Pid::from_raw) {
        match rustix::process::kill_process_group(group, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(error) => eprintln!("with-display: what {program} left running: {error}"),
        }
    }
    for mut server in servers {
        server.kill().map_err(DisplayError::StopWayvnc)?;
        server.wait().map_err(|source| DisplayError::Wait {
            program: "wayvnc".to_owned(),
            source,
        })?;
    }
    if status.success() {
        Ok(())
    } else {
        Err(DisplayError::ApplicationExited {
            program: program.to_owned(),
            status,
        }
        .into())
    }
}

/// Marks the descriptors the session was given past the standard three close-on-exec, so that
/// what it starts doesn't get them. Cage learns that the session ended when the pipe it gave the
/// session hangs up (cage's `spawn_primary_client`), so every process holding the pipe's write
/// end keeps cage, and the container, up. The session's own copy goes when it exits; a copy in an
/// adb server the Android driver started (`adb start-server` forks one into a session of its own,
/// beyond the application's process group, which the session kills) stayed: cage and the
/// container then ran on after the driver failed, and what waited for the driver waited its whole
/// patience, an hour. Called first, while the session has no other thread to open or close
/// descriptors.
fn keep_descriptors_from_children() -> Result<(), DisplayError> {
    let failed = DisplayError::Descriptors;
    for entry in std::fs::read_dir("/proc/self/fd").map_err(failed)? {
        let name = entry.map_err(failed)?.file_name();
        let Some(fd) = name.to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        if fd <= 2 {
            continue;
        }
        // SAFETY: `fd` was open as the listing read it, and nothing closes it while it is
        // borrowed: this thread is the session's only one, and only `read_dir`'s own descriptor,
        // listed too, may be closed by now, which `fcntl` answers with EBADF.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match rustix::io::fcntl_getfd(borrowed) {
            Ok(flags) => rustix::io::fcntl_setfd(borrowed, flags | rustix::io::FdFlags::CLOEXEC)
                .map_err(|errno| failed(errno.into()))?,
            Err(rustix::io::Errno::BADF) => {}
            Err(errno) => return Err(failed(errno.into())),
        }
    }
    Ok(())
}

/// The application's pid, as the session writes it while the application runs.
fn application() -> Option<Pid> {
    let pid = std::fs::read_to_string(Path::new(RUNTIME).join(APPLICATION)).ok()?;
    Pid::from_raw(pid.trim().parse().ok()?)
}

/// wayvnc on `view`'s socket in `directory`.
fn wayvnc(directory: &Path, view: View) -> Result<Child, DisplayError> {
    let name: &'static str = view.into();
    let mut wayvnc = Command::new("wayvnc");
    wayvnc
        // wayvnc's default is 30, below what the viewers take.
        .args(["--unix-socket", "--max-fps", "60", "--output", OUTPUT])
        .arg("--socket")
        .arg(Path::new(RUNTIME).join(format!("wayvnc-{name}")));
    if view == View::Watch {
        wayvnc.arg("--disable-input");
    }
    wayvnc
        .arg(directory.join(name))
        .spawn()
        .map_err(|source| DisplayError::Start {
            program: "wayvnc".to_owned(),
            source,
        })
}

/// The globals' list, which `registry_queue_init` keeps; nothing to do on its events.
struct Registry;

impl Dispatch<WlRegistry, GlobalListContents> for Registry {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn refuse_data_control() -> Result<(), DataControlError> {
    let connection = Connection::connect_to_env().map_err(DataControlError::Connect)?;
    let globals = registry_queue_init::<Registry>(&connection)
        .map_err(DataControlError::Globals)?
        .0;
    let offered = globals.contents().with_list(|list| {
        DATA_CONTROL
            .into_iter()
            .find(|protocol| list.iter().any(|global| global.interface == *protocol))
    });
    match offered {
        Some(protocol) => Err(DataControlError::Offered { protocol }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A descriptor the session was given open across exec, as cage's pipe is, is closed in what
    /// the session starts. The only test here, so no other thread opens or closes descriptors
    /// meanwhile.
    #[test]
    fn what_the_session_was_given_stays_out_of_its_children() {
        let (reader, _writer) = std::io::pipe().expect("a pipe");
        rustix::io::fcntl_setfd(&reader, rustix::io::FdFlags::empty())
            .expect("open across exec, as cage gives it");
        keep_descriptors_from_children().expect("marked");
        let flags = rustix::io::fcntl_getfd(&reader).expect("its flags");
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
    }
}
