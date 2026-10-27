//! The check every part of a run ends with (a fixture's setup, each phase): no message was lost. A phase's checks can pass with one
//! lost on the way (a sync copy, an answer to a request), and only the clients' logs say so:
//!
//! - b33p: a message that failed to decrypt, or a duplicate of one it neither decrypted lately
//!   nor stored: its sender used a counter twice, or b33p stopped between reading and storing
//!   it (b33p-core/src/signal/receive.rs `Decrypted`).
//! - Signal-Android: "Decryption error" (MessageDecryptor.kt), and "Duplicate message!" for an
//!   envelope that hadn't been decrypted before. Each line names the envelope as
//!   `[timestamp] sender:device |`, and each decrypted one is logged as such, so one delivered
//!   again is told from one whose sender used a counter twice.
//! - Signal-Desktop: `handleDecryptionError` (ts/textsecure/MessageReceiver.preload.ts), and a
//!   `DuplicatedMessage` it dropped for an envelope it hadn't decrypted before; it logs each it
//!   does, as `queueing decrypted envelope <sender>.<device> > <us> <timestamp>`.
//! - presage-cli: "failed to decrypt a message", as its syncs printed it ([`Pod::lost`]).

use std::collections::HashSet;

use crate::error::Result;
use crate::pod::{Pod, Service};

/// b33p's lines for a lost message (b33p-core/src/signal.rs, `Received::DecryptionError`).
const B33P: [&str; 2] = [
    "lost a message: its counter was used before, and it isn't stored",
    "failed to decrypt a message",
];
/// Signal-Android's (MessageDecryptor.kt).
const ANDROID_FAILED: &str = "| Decryption error";
const ANDROID_DUPLICATE: &str = "| Duplicate message!";
const ANDROID_DECRYPTED: &str = "| Successfully decrypted the envelope";
/// Signal-Desktop's (MessageReceiver.preload.ts).
const DESKTOP_FAILED: &str = "handleDecryptionError";
const DESKTOP_DUPLICATE: &str = "DuplicatedMessage";
const DESKTOP_DROPPED: &str = "queueEncryptedEnvelope error handling envelope ";
const DESKTOP_DECRYPTED: &str = "queueing decrypted envelope ";
/// Where Signal-Android's logcat goes in its container (testbox/tools `android`).
pub(super) const LOGCAT: &str = "/tmp/logcat.txt";
/// Signal-Desktop's logs, app.log and main.log, in its user data (testbox/desktop/driver.ts).
pub(super) const DESKTOP_LOGS: &str = "cat /home/desktop/data/logs/*.log";
/// Every line of the clients' logs that tells of a lost message.
pub(super) fn found(containers: &Pod) -> Result<Vec<String>> {
    let mut lost = Vec::new();
    for b33p in containers.b33ps() {
        let log = containers.log(b33p)?;
        let name = b33p.name();
        lost.extend(
            log.lines()
                .filter(|line| B33P.iter().any(|needle| line.contains(needle)))
                .map(|line| format!("{name}: {}", plain(line).trim())),
        );
    }
    if containers.running(Service::Android) {
        let logcat = containers.exec(Service::Android, &["cat", LOGCAT], "")?;
        lost.extend(android(&logcat));
    }
    if containers.running(Service::Desktop) {
        let logs = containers.exec(Service::Desktop, &["sh", "-c", DESKTOP_LOGS], "")?;
        lost.extend(desktop(&logs));
    }
    lost.extend(containers.lost());
    Ok(lost)
}

/// `line` without the colours of b33p's log (SGR escapes, `ESC [ … m`).
fn plain(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            chars.by_ref().find(|&c| c == 'm');
        } else {
            plain.push(c);
        }
    }
    plain
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

/// The lines of Signal-Desktop's logs that tell of a lost message.
fn desktop(logs: &str) -> Vec<String> {
    let mut decrypted = HashSet::new();
    let mut lost = Vec::new();
    for line in logs.lines() {
        if let Some(envelope) = desktop_envelope(line, DESKTOP_DECRYPTED) {
            decrypted.insert(envelope);
        } else if line.contains(DESKTOP_FAILED)
            || line.contains(DESKTOP_DUPLICATE)
                && desktop_envelope(line, DESKTOP_DROPPED)
                    .is_some_and(|envelope| !decrypted.contains(envelope))
        {
            lost.push(format!("Desktop: {}", desktop_message(line)));
        }
    }
    lost
}

/// The `<sender>.<device> > <us> <timestamp>` after `marker` (an envelope's id, without the
/// server's GUID in parentheses after it).
fn desktop_envelope<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let rest = &line[line.find(marker)? + marker.len()..];
    Some(rest[..rest.find(" (")?].trim())
}

/// The `msg` of a line of Desktop's JSON log, up to its first escaped newline (a stack follows).
fn desktop_message(line: &str) -> &str {
    let message = line.find("\"msg\":\"").map_or(line, |at| &line[at + 7..]);
    message.split("\\n").next().unwrap_or(message).trim()
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

    #[test]
    fn colours_are_left_out() {
        let line = "\u{1b}[33m WARN\u{1b}[0m \u{1b}[2mb33p_core::signal\u{1b}[0m: lost a message";
        assert_eq!(plain(line), " WARN b33p_core::signal: lost a message");
    }

    /// As for Android: a duplicate counts only if Desktop never decrypted that envelope.
    #[test]
    fn desktop_loses_what_failed_and_duplicates_of_what_never_decrypted() {
        let logs = r#"{"level":30,"msg":"[MessageReceiver] queueing decrypted envelope [REDACTED]b44.1 > [REDACTED]b44 100 (01a1.aa)"}
{"level":50,"msg":"[MessageReceiver] queueEncryptedEnvelope error handling envelope [REDACTED]b44.1 > [REDACTED]b44 100 (01a1.bb) : DuplicatedMessage: message with old counter 1 / 0
    at new LibSignalErrorBase"}
{"level":50,"msg":"[MessageReceiver] queueEncryptedEnvelope error handling envelope [REDACTED]b44.1 > [REDACTED]b44 101 (01a1.cc) : DuplicatedMessage: message with old counter 2 / 0
    at new LibSignalErrorBase"}
{"level":30,"msg":"handleDecryptionError: 102"}
"#;
        let lost = desktop(logs);
        assert_eq!(lost.len(), 2, "{lost:?}");
        assert!(lost[0].contains(" 101 ") && lost[0].ends_with("old counter 2 / 0"));
        assert!(lost[1].contains("handleDecryptionError: 102"));
    }

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
