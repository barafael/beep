//! Runs a command on a display of its own, which `cargo testbox vnc` shows:
//! TigerVNC's Xvnc, served on a Unix socket only, with the viewer's input and the clipboard off.
//! `with-display input on|off`, run beside it, turns the viewer's input on or off (`vncconfig`);
//! the clipboard stays off (`vncconfig -set AcceptCutText=1` fails).
//!
//! As the container's entrypoint it is PID 1, which ignores a signal it has no handler for:
//! it hands SIGTERM, SIGINT and SIGHUP on to the command, so that `podman stop` and `restart`
//! let the client end cleanly (b33p closes its database) instead of killing it 10 s later.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use strum::VariantArray;
use testbox_tools::{Context, Error, Input, Result};

const USAGE: &str = "with-display <socket> <width>x<height> -- <command> [argument...] \
    | with-display input on|off";
/// The container's one X display.
const DISPLAY: &str = ":1";
/// The viewer's input, off at the start and turned on at run time: the only parameters
/// `-AllowOverride` lets `vncconfig` change.
#[derive(Clone, Copy, strum::IntoStaticStr, strum::VariantArray)]
enum InputParameter {
    AcceptKeyEvents,
    AcceptPointerEvents,
}

fn main() -> std::process::ExitCode {
    testbox_tools::exit("with-display", run())
}

fn run() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [input, state] if input == "input" => set_input(
            state
                .parse()
                .map_err(|_| Error::Failed(format!("input {state}: on or off")))?,
        ),
        [socket, geometry, separator, program, program_arguments @ ..] if separator == "--" => {
            serve(socket, geometry, program, program_arguments)
        }
        _ => Err(Error::Failed(format!("usage: {USAGE}"))),
    }
}

/// Turns the viewer's input on the running display on or off.
fn set_input(state: Input) -> Result<()> {
    let value = match state {
        Input::On => "1",
        Input::Off => "0",
    };
    for &parameter in InputParameter::VARIANTS {
        let parameter: &'static str = parameter.into();
        let mut vncconfig = Command::new("vncconfig");
        vncconfig.args(["-display", DISPLAY, "-set", &format!("{parameter}={value}")]);
        testbox_tools::process::run(vncconfig)?;
    }
    Ok(())
}

fn serve(socket: &str, geometry: &str, program: &str, program_arguments: &[String]) -> Result<()> {
    // One left by this container before it restarted (`restartPolicy: Always`): Xvnc doesn't
    // bind over it.
    testbox_tools::remove_if_present(Path::new(socket))?;
    let overridable: Vec<&'static str> = InputParameter::VARIANTS
        .iter()
        .map(|&parameter| parameter.into())
        .collect();
    let mut xvnc = Command::new("Xvnc")
        .args([DISPLAY, "-geometry", geometry, "-depth", "24"])
        // The socket only, the owner's only: no VNC port, no X over TCP.
        .args([
            "-rfbport=-1",
            "-rfbunixpath",
            socket,
            "-rfbunixmode",
            "0600",
        ])
        // X only on the file socket in this container's /tmp: the pod's containers share a
        // network namespace, so an abstract socket (`local`) would let them all reach this
        // display, and collide with each other's `:1`.
        .args(["-nolisten", "tcp", "-nolisten", "local"])
        // Whoever can open the socket may watch: its mode and its directory's are the access
        // control.
        .args(["-SecurityTypes", "None", "-AlwaysShared"])
        .args(
            overridable
                .iter()
                .map(|parameter| format!("-{parameter}=0")),
        )
        .arg("-AcceptSetDesktopSize=0")
        .args([
            "-AcceptCutText=0",
            "-SendCutText=0",
            "-SendPrimary=0",
            "-SetPrimary=0",
        ])
        .args(["-AllowOverride", &overridable.join(",")])
        // The display's number, once it accepts connections (Xserver(1) `-displayfd`).
        .args(["-displayfd", "1"])
        .stdout(Stdio::piped())
        .spawn()
        .context("Xvnc")?;
    let stdout = xvnc.stdout.take().ok_or("Xvnc's stdout isn't piped")?;
    // Open while Xvnc runs: closed, Xvnc's next write to it would end Xvnc (SIGPIPE).
    let mut announced = BufReader::new(stdout);
    let mut number = String::new();
    announced
        .read_line(&mut number)
        .context("Xvnc's -displayfd")?;
    if number.trim().is_empty() {
        let status = xvnc.wait().context("Xvnc")?;
        return Err(Error::Failed(format!(
            "Xvnc exited before it served a display: {status}"
        )));
    }
    let mut command = Command::new(program)
        .args(program_arguments)
        .env("DISPLAY", DISPLAY)
        .spawn()
        .context(program)?;
    hand_on_signals(command.id())?;
    let status = command.wait().context(program)?;
    xvnc.kill().context("Xvnc")?;
    xvnc.wait().context("Xvnc")?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Failed(format!("{program} exited: {status}")))
    }
}

/// Hands the stop signals this process gets on to `pid`, from a thread of its own, for as long as
/// this process runs.
fn hand_on_signals(pid: u32) -> Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT, SIGHUP])
        .context("the stop signals' handler")?;
    let pid = libc::pid_t::try_from(pid).map_err(|_| Error::Failed(format!("pid {pid}")))?;
    std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            for signal in signals.forever() {
                // SAFETY: kill(2) takes plain integers and touches no memory of ours.
                if unsafe { libc::kill(pid, signal) } != 0 {
                    eprintln!(
                        "with-display: signal {signal} to {pid}: {}",
                        std::io::Error::last_os_error()
                    );
                }
            }
        })
        .context("the stop signals' thread")?;
    Ok(())
}
