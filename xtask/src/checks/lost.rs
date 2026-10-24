//! The check every phase ends with: no message was lost. A phase's checks can pass with one
//! lost on the way (a sync copy, an answer to a request), and only the clients' logs say so:
//!
//! - b33p: a message that failed to decrypt, or a duplicate of one it never decrypted, whose
//!   sender used a counter twice (b33p-core/src/signal.rs `Decrypted`).
//! - Signal-Android: "Decryption error" (MessageDecryptor.kt), and "Duplicate message!" for an
//!   envelope that hadn't been decrypted before. Each line names the envelope as
//!   `[timestamp] sender:device |`, and each decrypted one is logged as such, so one delivered
//!   again is told from one whose sender used a counter twice.
//! - Signal-Desktop: `handleDecryptionError` (ts/textsecure/MessageReceiver.preload.ts). Desktop
//!   drops a duplicate without a word, so a counter used twice towards Desktop doesn't show.
//! - presage-cli: "failed to decrypt a message", as its syncs printed it ([`Pod::lost`]).

use std::collections::HashSet;

use crate::error::{Error, Result};
use crate::pod::{Pod, Service};

/// b33p's lines for a lost message (b33p-core/src/signal.rs, `Received::DecryptionError`).
const B33P: [&str; 2] = [
    "lost a message: its sender had used its counter for another one",
    "failed to decrypt a message",
];
/// Signal-Android's (MessageDecryptor.kt).
const ANDROID_FAILED: &str = "| Decryption error";
const ANDROID_DUPLICATE: &str = "| Duplicate message!";
const ANDROID_DECRYPTED: &str = "| Successfully decrypted the envelope";
/// Signal-Desktop's (MessageReceiver.preload.ts `handleDecryptionError`).
const DESKTOP: [&str; 1] = ["handleDecryptionError"];
/// Where Signal-Android's logcat goes in its container (testbox/tools `android`).
const LOGCAT: &str = "/tmp/logcat.txt";
/// Signal-Desktop's logs, app.log and main.log, in its user data (testbox/desktop/driver.ts).
const DESKTOP_LOGS: &str = "cat /home/desktop/data/logs/*.log";
/// How many lost messages a failure lists.
const LISTED: usize = 10;

/// Fails, listing them, if any client's log tells of a lost message.
pub(super) fn none(containers: &Pod) -> Result<()> {
    let mut lost = Vec::new();
    if containers.running(Service::B33p) {
        let log = containers.log(Service::B33p)?;
        lost.extend(
            log.lines()
                .filter(|line| B33P.iter().any(|needle| line.contains(needle)))
                .map(|line| format!("b33p: {}", line.trim())),
        );
    }
    if containers.running(Service::Android) {
        let logcat = containers.exec(Service::Android, &["cat", LOGCAT], "")?;
        lost.extend(android(&logcat));
    }
    if containers.running(Service::Desktop) {
        let logs = containers.exec(Service::Desktop, &["sh", "-c", DESKTOP_LOGS], "")?;
        lost.extend(
            logs.lines()
                .filter(|line| DESKTOP.iter().any(|needle| line.contains(needle)))
                .map(|line| format!("Desktop: {}", line.trim())),
        );
    }
    lost.extend(containers.lost());
    if lost.is_empty() {
        return Ok(());
    }
    let count = lost.len();
    lost.truncate(LISTED);
    Err(Error::Failed(format!("{count} lost:\n{}", lost.join("\n"))))
}

/// The lines of Signal-Android's `logcat` that tell of a lost message.
fn android(logcat: &str) -> Vec<String> {
    let mut decrypted = HashSet::new();
    let mut lost = Vec::new();
    for line in logcat.lines() {
        let Some(envelope) = envelope(line) else {
            continue;
        };
        if line.contains(ANDROID_DECRYPTED) {
            decrypted.insert(envelope);
        } else if line.contains(ANDROID_FAILED)
            || line.contains(ANDROID_DUPLICATE) && !decrypted.contains(envelope)
        {
            lost.push(format!("Android: {}", line.trim()));
        }
    }
    lost
}

/// The `[timestamp] sender:device` a MessageDecryptor line names its envelope by.
fn envelope(line: &str) -> Option<&str> {
    let start = line.find("MessageDecryptor")?;
    let line = &line[start..];
    let from = line.find('[')?;
    let to = line[from..].find(" |")?;
    Some(&line[from..from + to])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A duplicate of an envelope decrypted before was delivered again; any other duplicate,
    /// and any decryption error, is a lost message.
    #[test]
    fn android_loses_what_failed_and_duplicates_of_what_never_decrypted() {
        let logcat = "\
10-24 22:37:38.1 D/MessageDecryptor( 3527): [100] <ACI:1>:2 | Successfully decrypted the envelope in 5 ms
10-24 22:37:38.2 W/MessageDecryptor( 3527): [100] <ACI:1>:2 | Duplicate message!
10-24 22:37:38.3 W/MessageDecryptor( 3527): org.signal.libsignal.protocol.DuplicateMessageException: message with old counter 2 / 1
10-24 22:37:39.1 W/MessageDecryptor( 3527): [124] <ACI:1>:2 | Duplicate message!
10-24 22:37:39.2 W/MessageDecryptor( 3527): [125] <ACI:1>:1 | Decryption error for a sync message! Enqueuing a session reset job.
10-24 22:37:39.3 I/MessageDecryptor( 3527): [126] <ACI:1>:2 | Prekey message. Scheduling a prekey sync job.
";
        let lost = android(logcat);
        assert_eq!(lost.len(), 2, "{lost:?}");
        assert!(lost[0].contains("[124]") && lost[0].ends_with("Duplicate message!"));
        assert!(lost[1].contains("[125]") && lost[1].contains("Decryption error"));
    }
}
