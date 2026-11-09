//! Another check every phase ends with: b33p logged no ERROR line but the known ones. A line
//! that is new in kind is what this catches; the known ones are those every box leaves and
//! that aren't b33p's own doing ([`KNOWN`]), each with where it comes from. A box's record
//! keeps the whole log for reading them in place (xtask/src/checks/record.rs).

use crate::pod::Pod;

/// The ERROR lines a box leaves that say nothing new, by a piece of each.
const KNOWN: [&str; 7] = [
    // cage, the clients' compositor (testbox/tools `with-display`): the image has no Xwayland,
    // on purpose, and cage says so at every start.
    "Cannot find Xwayland binary",
    "Cannot create XWayland server",
    // libsignal-service's websocket and presage's receiving loop, when the server side goes:
    // the server's restart in `core`, a b33p killed in `core` and the tour, and the box's end,
    // where the record stops the server before it takes b33p's log.
    "websocket task ended",
    "unexpected error in message receiving loop",
    // libsignal-service's `sync_group_send`, on every typing indicator b33p sends to a group:
    // only data and edit messages have a sent transcript, which the one-to-one path knows
    // ("not getting a content body to sync is not an error") and the group path logs as an
    // error (observations of 2026-11-01).
    "could not create sync message from a group message",
    // presage's refresh task on a connect, when the websocket closes under its pre-key
    // registration (`core`'s keepalive drop): the next connect runs it again, and the keys
    // were on the server already (observations of 2026-10-31, the pre-key alarm).
    "failed to register pre-keys",
    // presage, on an envelope whose counter its session has seen: the same ciphertext a second
    // time, as a sender's retry over a dead link brings (libsignal-service sends the encrypted
    // messages again when the websocket closed under them, and the first try may have got
    // through). b33p tells a duplicate from a loss by its store (receive.rs `Decrypted`), and
    // the lost-message check counts the loss; the line itself says duplicate.
    "message with old counter",
];

/// The ERROR lines of every b33p's log not among the known, each with its b33p's name.
pub(super) fn found(containers: &Pod) -> Vec<String> {
    let mut unexpected = Vec::new();
    for b33p in containers.b33ps() {
        let Ok(log) = containers.log(b33p) else {
            continue;
        };
        let name = b33p.name();
        unexpected.extend(
            log.lines()
                .map(unnumbered)
                .filter(|line| is_error(line) && !KNOWN.iter().any(|known| line.contains(known)))
                .map(|line| format!("{name}: {line}")),
        );
    }
    unexpected
}

/// tracing's level in b33p's lines (` ERROR `), and cage's (`[ERROR]`).
fn is_error(line: &str) -> bool {
    line.starts_with("ERROR ") || line.contains(" ERROR ") || line.contains("[ERROR]")
}

/// `line` without its timestamp and ANSI colours, so that the same error on two boxes reads
/// the same and [`super::Reported`] counts it once.
fn unnumbered(line: &str) -> String {
    let out = super::without_colours(line);
    let trimmed = out.trim_start();
    // tracing's timestamp first: up to the first space.
    match trimmed.split_once(' ') {
        Some((first, rest)) if first.len() >= 20 && first.contains('T') => {
            rest.trim_start().to_owned()
        }
        _ => trimmed.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{KNOWN, is_error, unnumbered};

    #[test]
    fn lines_are_classed_and_stripped() {
        let line =
            "\u{1b}[2m2026-11-01T02:14:07.901Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m presage: boom";
        // Coloured, the level isn't found as a word: lines are stripped first.
        assert!(!is_error(line));
        let plain = unnumbered(line);
        assert_eq!(plain, "ERROR presage: boom");
        assert!(is_error(&format!(" {plain}")));
        assert!(is_error(&unnumbered(
            "2026-11-01T02:14:07.901Z  ERROR presage: boom"
        )));
        assert!(is_error(
            "00:00:00.000 [ERROR] [../cage.c:535] Cannot create XWayland server"
        ));
        assert!(
            KNOWN
                .iter()
                .any(|known| "x Cannot create XWayland server".contains(known))
        );
    }
}
