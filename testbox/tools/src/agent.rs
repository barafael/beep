//! The protocol of the Android driver's on-device agent (testbox/android/agent), one JSON
//! document per line each way: a [`Request`], answered with a
//! [`Reply`]. Its Kotlin side is generated from this file (typeshare, testbox/android/Containerfile
//! stage `protocol`), so the two can't drift: each sum type is tagged by `kind` beside its
//! content, the one discriminator kotlinx.serialization takes, and numbers are 32-bit, as
//! typeshare carries them.

use serde::{Deserialize, Serialize};
use typeshare::typeshare;

#[typeshare]
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u32,
    pub call: Call,
}

/// Each waits on the screen's accessibility events, up to `timeout_ms`; a wait answers with
/// [`Found`] once the screen has settled, an action with [`Outcome::Acted`].
#[typeshare]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "params", rename_all = "snake_case")]
pub enum Call {
    /// The first of `wanted` to show.
    WaitFor {
        wanted: Vec<Wanted>,
        timeout_ms: u32,
    },
    /// Until none of `wanted` shows, on the settled screen: a menu or dialog gone, its closing
    /// animation over.
    WaitGone {
        wanted: Vec<Wanted>,
        timeout_ms: u32,
    },
    /// Sets the text of the field with input focus, once one has it (accessibility's
    /// ACTION_SET_TEXT: no keys, so nothing goes astray while the keyboard binds to the field),
    /// and answers once it reads as `echo` says.
    SetText {
        text: String,
        echo: Echo,
        timeout_ms: u32,
    },
    /// Scrolls until `label` shows.
    ScrollTo { label: String, timeout_ms: u32 },
    /// Taps the node `handle` names in the last answer's screen where it is now, a long press if
    /// `long`, with a real touch (Agent.kt `tap` says why not accessibility's click).
    Click { handle: u32, long: bool },
}

#[typeshare]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Wanted {
    /// A node whose text or content description is this, padding aside.
    Label(String),
    /// A node whose text has this in it: a notice, worded around a name.
    Containing(String),
    /// A [`Wanted::Label`] that is enabled: what a tap is for, as a disabled button takes taps and
    /// does nothing (the profile's Next, the moment its name is set).
    Control(String),
    /// An editable field.
    Editable,
}

impl Wanted {
    pub fn label(label: &str) -> Self {
        Self::Label(label.to_owned())
    }

    pub fn control(label: &str) -> Self {
        Self::Control(label.to_owned())
    }

    pub fn containing(text: &str) -> Self {
        Self::Containing(text.to_owned())
    }
}

#[typeshare]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Echo {
    /// Any text: the field only has to have focus.
    Anything,
    Exact(String),
    /// Its digits, whatever the field formats around them (a phone number).
    Digits(String),
}

/// A visible node; `handle` names it for a [`Call::Click`] after the answer it came in; `window`
/// is its window's place from the top (0 for a popup over the app); `y` its middle's height on
/// the screen (a row's buttons are told by it).
#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub handle: u32,
    pub text: String,
    pub content_desc: String,
    pub class_name: String,
    pub checked: bool,
    pub enabled: bool,
    pub editable: bool,
    pub window: u32,
    pub y: u32,
}

impl Node {
    /// Whether its text or content description is `label`, padding aside ("Note to Self  "
    /// around a badge).
    pub fn labelled(&self, label: &str) -> bool {
        self.text.trim() == label || self.content_desc.trim() == label
    }
}

/// Which of the wanted showed, its node, and the settled screen it is on.
#[typeshare]
#[derive(Debug, Serialize, Deserialize)]
pub struct Found {
    pub which: u32,
    pub node: Node,
    pub screen: Vec<Node>,
}

/// The answer to request `id`; 0 for a line that didn't parse as a request.
#[typeshare]
#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub id: u32,
    pub outcome: Outcome,
}

#[typeshare]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Outcome {
    Done(Found),
    Acted,
    Failed(String),
}
