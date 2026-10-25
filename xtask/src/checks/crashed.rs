//! The other check every phase ends with: no client crashed. A crash shows only as the checks
//! after it failing, each in its own way; `with-display`, the clients' entrypoint, logs how its
//! client ended, and since it hands the stop signals on, a client ended by a signal crashed.

use crate::error::{Error, Result};
use crate::pod::{Pod, SCREENS};

/// How `with-display` says its client ended by a signal (testbox/tools `with-display`).
const KILLED: &str = "exited: signal:";

/// Fails, naming them, if a client's log says it ended by a signal.
pub(super) fn none(containers: &Pod) -> Result<()> {
    let mut crashed = Vec::new();
    for screen in SCREENS {
        let Ok(log) = containers.log(screen) else {
            continue;
        };
        let name: &'static str = screen.into();
        crashed.extend(
            log.lines()
                .filter(|line| line.starts_with("with-display:") && line.contains(KILLED))
                .map(|line| format!("{name}: {line}")),
        );
    }
    if crashed.is_empty() {
        Ok(())
    } else {
        Err(Error::Failed(crashed.join("\n")))
    }
}
