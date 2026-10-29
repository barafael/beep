//! What a box leaves behind for looking into it once it is wiped: `target/testbox-record/<box>/`,
//! with every container's log, Desktop's own logs, Android's logcat and the host's load at the
//! box's end, and with `--screenshots` each client's display after each check, in a directory for
//! each part (the box's setup, each phase): b33p's window as its harness draws it, the others'
//! displays as their viewers show them (testbox/shot).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::error::{Context, Result};
use crate::harness::Harness;
use crate::pod::{self, Pod, Service};
use crate::process;

use super::lost::{DESKTOP_LOGS, LOGCAT};

/// How long a check's name may be in a screenshot's file name.
const LABEL: usize = 60;

/// How long a b33p may take to draw its window for a screenshot.
const DRAWING: Duration = Duration::from_secs(30);

/// Where the run directory ([`Pod::run_dir`]) is in a b33p's container (testbox/b33p/pod.yaml).
const B33P_RUN: &str = "/run/b33p";

/// A box's record, or a part's within it.
pub(crate) struct Record {
    directory: PathBuf,
    shots: Option<Shots>,
}

/// The screenshots: the tool, the displays it takes (`<name>=<socket>`), the b33ps, and how
/// many were taken, which numbers them in the order of the checks.
///
/// A b33p's is its harness's: the window drawn from the state the check left, where its display
/// shows that only once b33p has painted it and the compositor and the viewer have caught up,
/// often a check later. Popups are windows of their own on Qt, so they aren't in it.
#[derive(Clone)]
struct Shots {
    tool: PathBuf,
    displays: Vec<String>,
    b33ps: Vec<B33pShot>,
    taken: usize,
}

/// A b33p's screenshots: its name, its harness's socket, and the run directory it writes them
/// to, on the host.
#[derive(Clone)]
struct B33pShot {
    name: String,
    socket: PathBuf,
    run: PathBuf,
}

impl B33pShot {
    /// Its window, as it is drawn now, at `path`.
    fn take(&self, path: &Path) -> Result<()> {
        let file = format!("shot-{}.png", self.name);
        Harness::connect(&self.socket)?.screenshot(&format!("{B33P_RUN}/{file}"), DRAWING)?;
        let written = self.run.join(file);
        std::fs::rename(&written, path).context(written.display().to_string())
    }
}

impl Record {
    /// The record in target/testbox-record/`name` ([`Pod::record_dir`]), emptied of a former
    /// run's; with
    /// `screenshots`, testbox/shot built and the running clients' displays found.
    pub(crate) fn new(pod: &Pod, repo: &Path, name: &str, screenshots: bool) -> Result<Self> {
        let directory = pod.record_dir().join(name);
        if directory.exists() {
            std::fs::remove_dir_all(&directory).context(directory.display().to_string())?;
        }
        std::fs::create_dir_all(&directory).context(directory.display().to_string())?;
        let shots = if screenshots {
            let manifest = repo.join("testbox/shot/Cargo.toml");
            let mut build = Command::new(env!("CARGO"));
            build.args([
                "build",
                "--quiet",
                "--release",
                "--locked",
                "--manifest-path",
            ]);
            build.arg(&manifest);
            process::run(build)?;
            let screens = pod.screens();
            let displays = screens
                .iter()
                .filter(|screen| !matches!(screen, Service::B33p(_)))
                .map(|&screen| {
                    format!(
                        "{}={}",
                        screen.name(),
                        pod.view_socket(screen, false).display()
                    )
                })
                .collect();
            let b33ps = screens
                .iter()
                .filter_map(|&screen| match screen {
                    Service::B33p(n) => Some(B33pShot {
                        name: screen.name(),
                        socket: pod.run_dir().join(pod::agent_socket(n)),
                        run: pod.run_dir(),
                    }),
                    _ => None,
                })
                .collect();
            Some(Shots {
                tool: repo.join("testbox/shot/target/release/testbox-shot"),
                displays,
                b33ps,
                taken: 0,
            })
        } else {
            None
        };
        Ok(Self { directory, shots })
    }

    /// The record of the part `name` within this one, its screenshots numbered from 1.
    pub(super) fn part(&self, name: &str) -> Result<Self> {
        let directory = self.directory.join(name);
        std::fs::create_dir_all(&directory).context(directory.display().to_string())?;
        let shots = self.shots.clone().map(|shots| Shots { taken: 0, ..shots });
        Ok(Self { directory, shots })
    }

    /// With `--screenshots`, each display as it is after the check `name`. A screenshot that
    /// fails fails no check: it is said so, and the run goes on.
    pub(super) fn shoot(&mut self, name: &str) {
        let Some(shots) = &mut self.shots else {
            return;
        };
        shots.taken += 1;
        let label = format!("{:02}-{}", shots.taken, label(name));
        for b33p in &shots.b33ps {
            let path = self.directory.join(format!("{label}-{}.png", b33p.name));
            if let Err(error) = b33p.take(&path) {
                println!("screenshot {label} of {}: {error}", b33p.name);
            }
        }
        if shots.displays.is_empty() {
            return;
        }
        let mut shot = Command::new(&shots.tool);
        shot.arg(&self.directory).arg(&label).args(&shots.displays);
        if let Err(error) = process::run(shot) {
            println!("screenshot {label}: {error}");
        }
    }

    /// Every container's log, Desktop's own logs and Android's logcat into the record, and the
    /// host's load (`load.txt`); says where it is. What can't be had is noted in its file
    /// instead.
    ///
    /// Signal-Server's lines go through Dropwizard's queue, which drains late on a loaded host
    /// (testbox/container-logs.md), so its log is taken with the server stopped: stopping, it
    /// writes out the queue before it exits. Then it starts again, and the box stays up to be
    /// looked at.
    pub(super) fn keep_logs(&self, pod: &Pod) {
        self.write("load.txt", &load(pod));
        let server = pod.running(Service::Server);
        if server && let Err(error) = pod.stop(Service::Server) {
            println!("record: stopping the server: {error}");
        }
        for service in pod.containers() {
            let log = pod
                .log_named(&service)
                .unwrap_or_else(|error| error.to_string());
            self.write(&format!("{service}.log"), &log);
        }
        if server && let Err(error) = pod.start(Service::Server) {
            println!("record: starting the server again: {error}");
        }
        if pod.running(Service::Desktop) {
            let logs = pod.exec(Service::Desktop, &["sh", "-c", DESKTOP_LOGS], "");
            self.write(
                "desktop-app.log",
                &logs.unwrap_or_else(|error| error.to_string()),
            );
        }
        if pod.running(Service::Android) {
            let logcat = pod.exec(Service::Android, &["cat", LOGCAT], "");
            self.write(
                "logcat.txt",
                &logcat.unwrap_or_else(|error| error.to_string()),
            );
        }
        println!("record: {}", self.directory.display());
    }

    /// `contents` as the file `name` in the record; a failure is said, and fails nothing.
    pub(super) fn write(&self, name: &str, contents: &str) {
        let path = self.directory.join(name);
        if let Err(error) = std::fs::write(&path, contents) {
            println!("record: {}: {error}", path.display());
        }
    }
}

/// The host's load average and pressure, and each container's `cpu.stat`, as they are at the
/// box's end: whether the host was loaded when a line came late or went missing.
fn load(pod: &Pod) -> String {
    let mut load = String::new();
    for file in [
        "/proc/loadavg",
        "/proc/pressure/cpu",
        "/proc/pressure/io",
        "/proc/pressure/memory",
    ] {
        let read = std::fs::read_to_string(file).unwrap_or_else(|error| format!("{error}\n"));
        load.push_str(&format!("== {file}\n{read}"));
    }
    for name in pod.containers() {
        let stat = pod
            .cpu_stat_named(&name)
            .unwrap_or_else(|error| format!("{error}\n"));
        load.push_str(&format!("== {name}: cpu.stat\n{stat}"));
    }
    load
}

/// A check's name as part of a file name: lowercase letters and digits, the rest one dash.
fn label(name: &str) -> String {
    let mut label = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            label.push(c.to_ascii_lowercase());
        } else if !label.is_empty() && !label.ends_with('-') {
            label.push('-');
        }
    }
    let mut label: String = label.trim_end_matches('-').chars().take(LABEL).collect();
    while label.ends_with('-') {
        label.pop();
    }
    label
}

#[cfg(test)]
mod tests {
    use super::label;

    #[test]
    fn a_check_name_becomes_a_file_name() {
        assert_eq!(
            label("Desktop's delivery receipt reaches b33p"),
            "desktop-s-delivery-receipt-reaches-b33p"
        );
        assert_eq!(
            label("  text: from b33p, to Desktop!  "),
            "text-from-b33p-to-desktop"
        );
        assert!(label(&"a long name ".repeat(20)).len() <= 60);
        assert!(!label(&"a long name ".repeat(20)).ends_with('-'));
    }
}
