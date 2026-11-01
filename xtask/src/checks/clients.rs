//! What the checks wait for on Signal-Desktop and Signal-Android, through their drivers.

use super::PATIENCE_MS;
use crate::harness::{Driver, HarnessError};
use testbox_tools::drivers;

pub(super) type Desktop = Driver<drivers::Desktop>;
pub(super) type Android = Driver<drivers::Android>;

/// Waits for Desktop's own `text` in its open conversation to show `status` ("sent",
/// "delivered" or "read"), and nothing further.
pub(super) fn desktop_status(
    desktop: &mut Desktop,
    text: &str,
    status: &str,
) -> Result<(), HarnessError> {
    desktop.call(drivers::Desktop::WaitForStatus {
        text: text.to_owned(),
        status: status.to_owned(),
        timeout_ms: PATIENCE_MS,
    })
}

/// Waits for `text` in Desktop's open conversation.
pub(super) fn desktop_text(desktop: &mut Desktop, text: &str) -> Result<(), HarnessError> {
    desktop.call(drivers::Desktop::WaitForText {
        text: text.to_owned(),
        timeout_ms: PATIENCE_MS,
    })
}

/// Waits for a message with `text` in Desktop's open conversation.
pub(super) fn desktop_shows(desktop: &mut Desktop, text: &str) -> Result<(), HarnessError> {
    desktop.call(drivers::Desktop::WaitForMessage {
        text: text.to_owned(),
        timeout_ms: PATIENCE_MS,
    })
}

/// Waits for `text` anywhere on Android's screen: in the chat list's preview, or in the open
/// conversation.
pub(super) fn android_shows(android: &mut Android, text: &str) -> Result<(), HarnessError> {
    android.call(drivers::Android::WaitFor {
        wanted: testbox_tools::agent::Wanted::label(text),
        timeout_ms: PATIENCE_MS,
    })
}

/// Waits for a text on Android's screen with `text` in it.
pub(super) fn android_containing(android: &mut Android, text: &str) -> Result<(), HarnessError> {
    android.call(drivers::Android::WaitFor {
        wanted: testbox_tools::agent::Wanted::containing(text),
        timeout_ms: PATIENCE_MS,
    })
}

/// Waits for a message starting with `head` in Desktop's open conversation to show
/// [`super::desktop::LAST_LINE`] too, unfolded if Desktop folded it: a long text, whole.
pub(super) fn desktop_shows_whole(desktop: &mut Desktop, head: &str) -> Result<(), HarnessError> {
    desktop.call(drivers::Desktop::WaitForMessageWhole {
        head: head.to_owned(),
        tail: super::desktop::LAST_LINE.to_owned(),
        timeout_ms: PATIENCE_MS,
    })
}
