//! What a box leaves behind for looking into it once it is wiped: `target/testbox-record/<box>/`,
//! with every container's log, Desktop's own logs, Android's logcat and the host's load at the
//! box's end, and with `--screenshots` each client's display after each check, in a directory for
//! each part (the box's setup, each phase): b33p's window as its harness draws it, the others'
//! displays as their viewers show them (testbox/shot).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::harness::{Harness, HarnessError};
use crate::pod::{self, Pod, Service};
use crate::process;
use testbox_tools::Report;
use testbox_tools::process::CommandError;

use super::lost::{DESKTOP_LOGS, LOGCAT};

/// How long a check's name may be in a screenshot's file name.
const LABEL: usize = 60;

/// How long a b33p may take to draw its window for a screenshot.
const DRAWING: Duration = Duration::from_secs(30);

/// Where the run directory ([`Pod::run_dir`]) is in a b33p's container (testbox/b33p/pod.yaml).
const B33P_RUN: &str = "/run/b33p";

/// A box's or a part's record that couldn't be made.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RecordError {
    /// A former run's record, which this one replaces.
    #[error("emptying {}", path.display())]
    Clear {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("creating {}", path.display())]
    Directory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// testbox/shot, which `--screenshots` takes the displays with.
    #[error("building the screenshot tool")]
    BuildShot(#[source] CommandError),
}

/// A b33p's screenshot that wasn't taken ([`B33pShot::take`]).
#[derive(Debug, thiserror::Error)]
enum ShotError {
    #[error("asking {b33p} to draw its window")]
    Draw {
        b33p: String,
        #[source]
        source: HarnessError,
    },
    #[error("moving {} to {}", written.display(), to.display())]
    Move {
        written: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A check as `spans.jsonl` has it ([`Record::span`]): its name, when it started and ended
/// (nanoseconds since the epoch, as the capture's packets), and whether it passed.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Span {
    pub(crate) check: String,
    pub(crate) started: u128,
    pub(crate) ended: u128,
    pub(crate) passed: bool,
}

/// A box's record, or a part's within it.
pub(crate) struct Record {
    directory: PathBuf,
    shots: Option<Shots>,
    /// With `--capture`: the checks' spans are written down ([`Self::span`]).
    capture: bool,
}

/// The screenshots: the tool, the displays it takes (`<name>=<socket>`), the b33ps, and how
/// many were taken, which numbers them in the order of the checks.
///
/// A b33p's is its harness's: the window drawn from the state the check left, where its display
/// shows that only once b33p has painted it and the compositor and the viewer have caught up,
/// often a check later. Popups may be windows of their own, and then they aren't in it.
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
    fn take(&self, path: &Path) -> Result<(), ShotError> {
        let file = format!("shot-{}.png", self.name);
        Harness::connect(&self.socket)
            .and_then(|mut b33p| b33p.screenshot(&format!("{B33P_RUN}/{file}"), DRAWING))
            .map_err(|source| ShotError::Draw {
                b33p: self.name.clone(),
                source,
            })?;
        let written = self.run.join(file);
        std::fs::rename(&written, path).map_err(|source| ShotError::Move {
            written,
            to: path.to_owned(),
            source,
        })
    }
}

impl Record {
    /// The record in target/testbox-record/`name` ([`Pod::record_dir`]), emptied of a former
    /// run's; with
    /// `screenshots`, testbox/shot built and the running clients' displays found.
    pub(crate) fn new(
        pod: &Pod,
        repo: &Path,
        name: &str,
        screenshots: bool,
    ) -> Result<Self, RecordError> {
        let directory = pod.record_dir().join(name);
        if directory.exists() {
            std::fs::remove_dir_all(&directory).map_err(|source| RecordError::Clear {
                path: directory.clone(),
                source,
            })?;
        }
        create(&directory)?;
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
            process::run(build).map_err(RecordError::BuildShot)?;
            let screens = pod.screens();
            let displays = screens
                .iter()
                .filter(|screen| !(matches!(screen, Service::B33p(_)) && pod.ui().draws_to_file()))
                .map(|&screen| {
                    format!(
                        "{}={}",
                        screen.name(),
                        pod.view_socket(screen, false).display()
                    )
                })
                .collect();
            // A GPUI b33p can't write its window to a file: its display, which shows only that,
            // is taken as the other clients' are.
            let draws_to_file = pod.ui().draws_to_file();
            let b33ps = screens
                .iter()
                .filter_map(|&screen| match screen {
                    Service::B33p(n) if draws_to_file => Some(B33pShot {
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
        Ok(Self {
            directory,
            shots,
            capture: pod.capturing(),
        })
    }

    /// The record of the part `name` within this one, its screenshots numbered from 1.
    pub(super) fn part(&self, name: &str) -> Result<Self, RecordError> {
        let directory = self.directory.join(name);
        create(&directory)?;
        let shots = self.shots.clone().map(|shots| Shots { taken: 0, ..shots });
        Ok(Self {
            directory,
            shots,
            capture: self.capture,
        })
    }

    /// With `--capture`, when the check `name` ran and how it went, as a line of `spans.jsonl`:
    /// what the capture holds of that time is the check's (xtask/src/wire).
    pub(super) fn span(&self, name: &str, started: SystemTime, ended: SystemTime, passed: bool) {
        if !self.capture {
            return;
        }
        let nanos = |at: SystemTime| {
            at.duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos())
        };
        let span = Span {
            check: name.to_owned(),
            started: nanos(started),
            ended: nanos(ended),
            passed,
        };
        let path = self.directory.join("spans.jsonl");
        let line = match serde_json::to_string(&span) {
            Ok(line) => line,
            Err(error) => {
                println!("record: the span of {name}: {error}");
                return;
            }
        };
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| {
                use std::io::Write as _;
                writeln!(file, "{line}")
            });
        if let Err(error) = appended {
            println!("record: {}: {error}", path.display());
        }
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
                println!("screenshot {label} of {}: {}", b33p.name, Report(&error));
            }
        }
        if shots.displays.is_empty() {
            return;
        }
        let mut shot = Command::new(&shots.tool);
        shot.arg(&self.directory).arg(&label).args(&shots.displays);
        if let Err(error) = process::run(shot) {
            println!("screenshot {label}: {}", Report(&error));
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
            println!("record: stopping the server: {}", Report(&error));
        }
        for service in pod.containers() {
            let log = pod
                .log_named(&service)
                .unwrap_or_else(|error| Report(&error).to_string());
            self.write(&format!("{service}.log"), &log);
        }
        if server && let Err(error) = pod.start(Service::Server) {
            println!("record: starting the server again: {}", Report(&error));
        }
        if pod.running(Service::Desktop) {
            let logs = pod.exec(Service::Desktop, &["sh", "-c", DESKTOP_LOGS], "");
            self.write(
                "desktop-app.log",
                &logs.unwrap_or_else(|error| Report(&error).to_string()),
            );
        }
        if pod.running(Service::Android) {
            let logcat = pod.exec(Service::Android, &["cat", LOGCAT], "");
            self.write(
                "logcat.txt",
                &logcat.unwrap_or_else(|error| Report(&error).to_string()),
            );
        }
        self.keep_contents(pod);
        if pod.capturing() {
            self.keep_capture(pod);
        }
        println!("record: {}", self.directory.display());
    }

    /// The b33ps' content logs (b33p-core content_log.rs) into the record's `content/`, for
    /// the wire comparison: moved, so that the next box's b33ps start new ones.
    fn keep_contents(&self, pod: &Pod) {
        let kept = self.directory.join(crate::wire::content::DIRECTORY);
        let moved = std::fs::read_dir(pod.run_dir()).and_then(|entries| {
            entries.into_iter().try_for_each(|entry| {
                let entry = entry?;
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with(pod::CONTENT_LOG_PREFIX) {
                    return Ok(());
                }
                std::fs::create_dir_all(&kept)?;
                std::fs::rename(entry.path(), kept.join(name))
            })
        });
        if let Err(error) = moved {
            println!("record: the content logs into {}: {error}", kept.display());
        }
    }

    /// The capture and the key logs into the record's `wire/raw/`. tcpdump and the servers go
    /// on writing to their files there while the box is up: moved, they are the same files.
    fn keep_capture(&self, pod: &Pod) {
        let raw = self.directory.join("wire/raw");
        let moved = std::fs::create_dir_all(&raw)
            .and_then(|()| std::fs::read_dir(pod.capture_dir()))
            .and_then(|entries| {
                entries.into_iter().try_for_each(|entry| {
                    let entry = entry?;
                    std::fs::rename(entry.path(), raw.join(entry.file_name()))
                })
            });
        if let Err(error) = moved {
            println!("record: the capture into {}: {error}", raw.display());
            return;
        }
        if let Err(error) = crate::wire::decode(&self.directory) {
            println!("record: decoding the capture: {}", Report(&error));
        }
    }

    /// `contents` as the file `name` in the record; a failure is said, and fails nothing.
    pub(super) fn write(&self, name: &str, contents: &str) {
        let path = self.directory.join(name);
        if let Err(error) = std::fs::write(&path, contents) {
            println!("record: {}: {error}", path.display());
        }
    }
}

/// Makes `directory` with what it is in.
fn create(directory: &Path) -> Result<(), RecordError> {
    std::fs::create_dir_all(directory).map_err(|source| RecordError::Directory {
        path: directory.to_owned(),
        source,
    })
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
            .unwrap_or_else(|error| format!("{}\n", Report(&error)));
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
