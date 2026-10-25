//! What a phase leaves behind for looking into it once the box is wiped:
//! `target/testbox-record/<phase>/`, with every container's log, Desktop's own logs and
//! Android's logcat at the phase's end, and with `--screenshots` each client's display after
//! each check (testbox/shot).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Context, Result};
use crate::pod::{Pod, SCREENS, Service};
use crate::process;

use super::lost::{DESKTOP_LOGS, LOGCAT};

/// Where the phases' records go, one directory each.
pub(crate) const RECORD: &str = "target/testbox-record";
/// How long a check's name may be in a screenshot's file name.
const LABEL: usize = 60;

/// A phase's record.
pub(crate) struct Record {
    directory: PathBuf,
    shots: Option<Shots>,
}

/// The screenshots: the tool, the displays as it takes them (`<name>=<socket>`), and how many
/// were taken, which numbers them in the order of the checks.
struct Shots {
    tool: PathBuf,
    displays: Vec<String>,
    taken: usize,
}

impl Record {
    /// The record of `phase`, emptied of a former run's; with `screenshots`, testbox/shot built
    /// and the running clients' displays found.
    pub(crate) fn new(pod: &Pod, repo: &Path, phase: &str, screenshots: bool) -> Result<Self> {
        let directory = repo.join(RECORD).join(phase);
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
            let displays = SCREENS
                .into_iter()
                .filter(|screen| pod.running(*screen))
                .map(|screen| {
                    let name: &'static str = screen.into();
                    format!("{name}={}", pod.view_socket(screen).display())
                })
                .collect();
            Some(Shots {
                tool: repo.join("testbox/shot/target/release/testbox-shot"),
                displays,
                taken: 0,
            })
        } else {
            None
        };
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
        let mut shot = Command::new(&shots.tool);
        shot.arg(&self.directory).arg(&label).args(&shots.displays);
        if let Err(error) = process::run(shot) {
            println!("screenshot {label}: {error}");
        }
    }

    /// Every container's log, Desktop's own logs and Android's logcat into the record; says
    /// where it is. What can't be had is noted in its file instead.
    pub(super) fn keep_logs(&self, pod: &Pod) {
        for service in pod.containers() {
            let log = pod
                .log_named(&service)
                .unwrap_or_else(|error| error.to_string());
            self.write(&format!("{service}.log"), &log);
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

    fn write(&self, name: &str, contents: &str) {
        let path = self.directory.join(name);
        if let Err(error) = std::fs::write(&path, contents) {
            println!("record: {}: {error}", path.display());
        }
    }
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
