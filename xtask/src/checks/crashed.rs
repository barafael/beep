//! The other check every phase ends with: no client crashed. A crash shows only as the checks
//! after it failing, each in its own way; `with-display`, the clients' entrypoint, logs how its
//! client ended, and since it hands the stop signals on, a client ended by a signal crashed.

use crate::pod::Pod;

/// How `with-display` says its client ended by a signal (testbox/tools `with-display`).
const KILLED: &str = "exited: signal:";

/// The lines of the clients' logs that say one ended by a signal.
pub(super) fn found(containers: &Pod) -> Vec<String> {
    let mut crashed = Vec::new();
    for screen in containers.all_screens() {
        let Ok(log) = containers.log(screen) else {
            continue;
        };
        let name = screen.name();
        crashed.extend(
            log.lines()
                .filter(|line| line.starts_with("with-display:") && line.contains(KILLED))
                .map(|line| format!("{name}: {line}")),
        );
    }
    crashed
}
