//! Runs a command on a display of its own, which `cargo testbox vnc` shows: cage, a Wayland kiosk
//! compositor whose one application fills the display, without a screen, served by wayvnc on a
//! Unix socket per [`View`] in a directory. Nothing of the clipboard reaches a viewer or comes
//! from one: wayvnc reaches it only through a data-control protocol, and the session refuses a
//! compositor that offers one.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};

use rustix::fs::Mode;
use rustix::process::{Pid, Signal};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use strum::VariantArray;
use testbox_tools::{Context, Describe, Error, Result, View};
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

fn main() -> std::process::ExitCode {
    testbox_tools::exit("with-display", run())
}

/// `with-display session …`, with the same arguments, is what cage runs.
fn run() -> Result<()> {
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
        _ => Err(Error::Failed(format!("usage: {USAGE}"))),
    }
}

fn serve(
    directory: &Path,
    geometry: &str,
    program: &str,
    program_arguments: &[String],
) -> Result<()> {
    // The sockets, the runtime directory and the application's files are the owner's only.
    rustix::process::umask(Mode::from_raw_mode(0o077));
    // Left by this container before it restarted (`restartPolicy: Always`): wayvnc doesn't bind
    // over one.
    for &view in View::VARIANTS {
        let name: &'static str = view.into();
        testbox_tools::remove_if_present(&directory.join(name))?;
    }
    std::fs::create_dir_all(RUNTIME).context(RUNTIME)?;
    // The application's pid, likewise when the container was killed with it: until the session
    // writes the new one, a stop signal would go to whatever has that pid now.
    testbox_tools::remove_if_present(&Path::new(RUNTIME).join(APPLICATION))?;
    let this = std::env::current_exe().context("with-display's path")?;
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
        // No screen and no GPU in the container.
        .env("WLR_BACKENDS", "headless")
        .env("WLR_RENDERER", "pixman")
        .env("WLR_LIBINPUT_NO_DEVICES", "1")
        .process_group(0)
        .spawn()
        .context("cage")?;
    stop_with_container(&cage)?;
    let status = cage.wait().context("cage")?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Failed(format!("cage exited: {status}")))
    }
}

/// Passes the stop signals the container gets (SIGTERM, SIGINT, SIGHUP) on to the application,
/// so that `podman stop` and `restart` let it end cleanly (b33p closes its database); the
/// session then ends, and cage with it. Before the application runs, there is nothing to end
/// cleanly: they go to cage's whole process group. As PID 1 without a handler, this process would
/// ignore them.
fn stop_with_container(cage: &Child) -> Result<()> {
    let group = i32::try_from(cage.id())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or("cage's pid")?;
    let mut signals =
        Signals::new([SIGTERM, SIGINT, SIGHUP]).context("the stop signals' handler")?;
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
) -> Result<()> {
    refuse_data_control()?;
    let mut wlr_randr = Command::new("wlr-randr");
    wlr_randr.args(["--output", OUTPUT, "--custom-mode", geometry]);
    testbox_tools::process::run(wlr_randr)?;
    let servers = View::VARIANTS
        .iter()
        .map(|&view| wayvnc(directory, view))
        .collect::<Result<Vec<Child>>>()?;
    // A group of its own: what it leaves running when it exits goes with it below.
    let mut application = Command::new(program)
        .args(program_arguments)
        .env(testbox_tools::DISPLAY_SIZE, geometry)
        .process_group(0)
        .spawn()
        .context(program)?;
    let pid = Path::new(RUNTIME).join(APPLICATION);
    testbox_tools::write(&pid, application.id().to_string())?;
    let status = application.wait().context(program)?;
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
        server.kill().context("wayvnc")?;
        server.wait().context("wayvnc")?;
    }
    if status.success() {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "{}: {program} exited: {status}",
            testbox_tools::APPLICATION_FAILED
        )))
    }
}

/// The application's pid, as the session writes it while the application runs.
fn application() -> Option<Pid> {
    let pid = std::fs::read_to_string(Path::new(RUNTIME).join(APPLICATION)).ok()?;
    Pid::from_raw(pid.trim().parse().ok()?)
}

/// wayvnc on `view`'s socket in `directory`.
fn wayvnc(directory: &Path, view: View) -> Result<Child> {
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
    wayvnc.arg(directory.join(name)).spawn().context("wayvnc")
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

fn refuse_data_control() -> Result<()> {
    let connection = Connection::connect_to_env().describe("cage")?;
    let globals = registry_queue_init::<Registry>(&connection)
        .describe("cage's globals")?
        .0;
    let offered = globals.contents().with_list(|list| {
        DATA_CONTROL
            .into_iter()
            .find(|protocol| list.iter().any(|global| global.interface == *protocol))
    });
    match offered {
        Some(protocol) => Err(Error::Failed(format!(
            "cage offers {protocol}, through which wayvnc would share the clipboard with the \
             viewers"
        ))),
        None => Ok(()),
    }
}
