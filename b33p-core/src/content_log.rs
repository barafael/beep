//! With `test-servers`: every content b33p takes in, a JSON line each in the file
//! `B33P_CONTENT_LOG` names, for the testbox's wire comparison (xtask/src/wire), which sees only
//! the encrypted envelopes. What b33p sends shows where another b33p takes it in (the mesh box):
//! as sent, not as b33p handed it to presage. Encoded as libsignal-service has it: what its
//! protobufs don't know isn't there.

use std::io::Write as _;
use std::sync::OnceLock;

use base64::Engine as _;
use presage::libsignal_service::content::ContentBody;
use presage::libsignal_service::prelude::ProtobufMessage as _;
use tracing::warn;

/// The file, if `B33P_CONTENT_LOG` names one.
fn file() -> Option<&'static std::path::Path> {
    static FILE: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    FILE.get_or_init(|| std::env::var_os("B33P_CONTENT_LOG").map(Into::into))
        .as_deref()
}

/// A content b33p took in: from `sender`'s device `device`.
pub(crate) fn received(sender: String, device: u32, timestamp: u64, body: &ContentBody) {
    note(serde_json::json!({
        "direction": "received",
        "sender": sender,
        "device": device,
        "timestamp": timestamp,
        "content": encoded(body),
    }));
}

fn encoded(body: &ContentBody) -> String {
    base64::engine::general_purpose::STANDARD.encode(body.clone().into_proto().encode_to_vec())
}

fn note(line: serde_json::Value) {
    let Some(path) = file() else {
        return;
    };
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(error) = written {
        warn!(%error, "failed to log a content");
    }
}
