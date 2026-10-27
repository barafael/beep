//! The running clients' displays: `cargo testbox vnc` in a window (testbox/viewer),
//! `cargo testbox web` as a web page for a browser (testbox/web). Each is a workspace of its own
//! that only its command builds.

use std::path::Path;
use std::process::Command;

use b33p_harness::ScreenSnapshot;

use crate::error::{Error, Result};
use crate::harness::Harness;
use crate::pod::{Pod, SCREENS, Service};
use crate::process;

/// Shows each running client's display in a window. `interactive` lets the window's keys and
/// pointer through to them; without it they only show, as a driver's input and the viewer's
/// would race.
pub fn vnc(pod: &Pod, repo: &Path, interactive: bool) -> Result<()> {
    process::run(viewer(pod, repo, "testbox/viewer/Cargo.toml", interactive)?)
}

/// Serves each running client's display as a web page on loopback; it prints the URL to open.
/// `interactive` as for [`vnc`].
pub fn web(pod: &Pod, repo: &Path, interactive: bool) -> Result<()> {
    process::run(viewer(pod, repo, "testbox/web/Cargo.toml", interactive)?)
}

/// `cargo run` of a viewer's own workspace for the running clients' displays, built for speed
/// (the frames are copied by hand); their input set as `interactive` says.
fn viewer(pod: &Pod, repo: &Path, manifest: &str, interactive: bool) -> Result<Command> {
    let mut run = Command::new(env!("CARGO"));
    run.current_dir(repo).args([
        "run",
        "--quiet",
        "--release",
        "--locked",
        "--manifest-path",
        manifest,
        "--",
    ]);
    if interactive {
        run.arg("--interactive");
    }
    let mut shown = 0;
    for screen in SCREENS.into_iter().filter(|screen| pod.running(*screen)) {
        let name: &'static str = screen.into();
        run.arg(format!(
            "{name}={}",
            pod.view_socket(screen, interactive).display()
        ));
        shown += 1;
    }
    if shown == 0 {
        return Err("no client with a display runs: `cargo testbox up` first".into());
    }
    Ok(run)
}

/// Puts the QR of the link b33p shows on the poster in the emulator camera's scene, for linking
/// by hand in a viewer: Android's scanner, opened from Settings, Linked devices, sees it.
pub fn qr(pod: &Pod, agent_socket: &Path) -> Result<()> {
    let url = match Harness::connect(agent_socket)?.snapshot()?.screen {
        ScreenSnapshot::Link { url, .. } => url,
        other => {
            return Err(Error::Failed(format!(
                "b33p shows no link to scan, but {other:?}: on its first screen, link to a phone"
            )));
        }
    };
    // The driver binary's own, run beside the driver, which may be busy with a test.
    pod.exec(Service::Android, &["android", "show-qr", &url], "")?;
    println!(
        "b33p's QR is on the camera's poster. On Android: Settings, Linked devices, \
         Link a new device; the scanner reads it."
    );
    Ok(())
}
