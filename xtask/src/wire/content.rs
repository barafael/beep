//! What each client puts in the contents it sends, where b33p takes them in: the end-to-end
//! encrypted part the tap can't see. b33p's `test-servers` builds write each content they take
//! in to a file (b33p-core/src/content_log.rs); the record keeps them in `content/`. What a b33p
//! sends shows where another b33p takes it in (the mesh box).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use base64::Engine as _;

use super::Summary;
use super::proto::{self, Field, Value};
use crate::error::{Context, Result, WithContext};

/// The record's directory of b33p's content logs.
pub(crate) const DIRECTORY: &str = "content";

/// One content as b33p logged it.
#[derive(serde::Deserialize)]
struct Logged {
    sender: String,
    device: u32,
    content: String,
}

/// The section of compare.md on the contents, if the record has any: for each kind of content,
/// which fields each client sent.
pub(crate) fn report(record: &Path, summaries: &[Summary]) -> Result<Option<String>> {
    let directory = record.join(DIRECTORY);
    if !directory.is_dir() {
        return Ok(None);
    }
    let senders = senders(summaries);
    // Kind, then path, then the clients that sent it; and how many of each kind each sent.
    let mut paths: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut clients = BTreeSet::new();
    for entry in std::fs::read_dir(&directory).context(directory.display().to_string())? {
        let path = entry.context(directory.display().to_string())?.path();
        for line in testbox_tools::read(&path)?.lines() {
            let logged: Logged =
                serde_json::from_str(line).with_context(|| path.display().to_string())?;
            let client = senders
                .get(&(logged.sender.clone(), logged.device))
                .cloned()
                .unwrap_or_else(|| format!("{}.{}", short(&logged.sender), logged.device));
            let Some(fields) = base64::engine::general_purpose::STANDARD
                .decode(&logged.content)
                .ok()
                .and_then(|bytes| proto::parse(&bytes))
            else {
                continue;
            };
            let kind = kind(&fields);
            clients.insert(client.clone());
            *counts
                .entry(kind.clone())
                .or_default()
                .entry(client.clone())
                .or_default() += 1;
            for path in named(&fields) {
                paths
                    .entry(kind.clone())
                    .or_default()
                    .entry(path)
                    .or_default()
                    .insert(client.clone());
            }
        }
    }
    let clients: Vec<String> = clients.into_iter().collect();
    let mut out = String::from("\n## Contents\n\n");
    out.push_str(
        "The fields of the contents b33p took in, by who sent them (b33p's `test-servers` ",
    );
    out.push_str("content log). **Bold**: only one sender filled it in.\n");
    for (kind, by_path) in &paths {
        let sent: Vec<String> = counts[kind]
            .iter()
            .map(|(client, n)| format!("{client} {n}"))
            .collect();
        out.push_str(&format!("\n### {kind} ({})\n\n", sent.join(", ")));
        out.push_str(&format!(
            "| field | {} |\n|---|{}\n",
            clients.join(" | "),
            "---|".repeat(clients.len())
        ));
        for (path, by) in by_path {
            let senders_of_kind = counts[kind].len();
            let path = if by.len() == 1 && senders_of_kind > 1 {
                format!("**`{path}`**")
            } else {
                format!("`{path}`")
            };
            let cells: Vec<&str> = clients
                .iter()
                .map(|client| if by.contains(client) { "✓" } else { "" })
                .collect();
            out.push_str(&format!("| {path} | {} |\n", cells.join(" | ")));
        }
    }
    Ok(Some(out))
}

/// The client behind each account and device, from the logins the connections made: `<aci>`
/// for a primary (device 1), `<aci>.<device>` for a linked device.
fn senders(summaries: &[Summary]) -> HashMap<(String, u32), String> {
    summaries
        .iter()
        .filter_map(|summary| {
            let user = summary.user.as_deref()?;
            let (aci, device) = match user.rsplit_once('.') {
                Some((aci, device)) => (aci, device.parse().ok()?),
                None => (user, 1),
            };
            // A storage credential's user isn't an account's: no dashes.
            aci.contains('-')
                .then(|| ((aci.to_owned(), device), summary.client.clone()))
        })
        .collect()
}

fn short(aci: &str) -> &str {
    aci.get(..8).unwrap_or(aci)
}

/// What a content is: the field it carries (a sync message's, the sync's kind).
fn kind(fields: &[Field]) -> String {
    let kinds: Vec<String> = fields
        .iter()
        .filter(|field| !matches!(field.number, 7 | 12))
        .map(|field| match (field.number, &field.value) {
            (2, Value::Message(sync)) => {
                let inner: Vec<&str> = sync
                    .iter()
                    .filter(|field| field.number != 8)
                    .map(|field| name(&["syncMessage"], field.number))
                    .collect();
                format!("syncMessage.{}", inner.join("+"))
            }
            (number, _) => name(&[], number).to_owned(),
        })
        .collect();
    if kinds.is_empty() {
        // A sender key distribution, say, with nothing else.
        fields
            .iter()
            .map(|field| name(&[], field.number))
            .collect::<Vec<_>>()
            .join("+")
    } else {
        kinds.join("+")
    }
}

/// The content's fields as paths, named where SignalService.proto's names are known here.
fn named(fields: &[Field]) -> Vec<String> {
    let mut paths = Vec::new();
    collect(fields, &mut Vec::new(), &mut paths);
    paths.sort();
    paths.dedup();
    paths
}

fn collect(fields: &[Field], parents: &mut Vec<&'static str>, paths: &mut Vec<String>) {
    for field in fields {
        let name = name(parents, field.number);
        let path = parents
            .iter()
            .copied()
            .chain(std::iter::once(name))
            .collect::<Vec<_>>()
            .join(".");
        match &field.value {
            Value::Message(inner) => {
                paths.push(format!("{path}:message"));
                parents.push(name);
                collect(inner, parents, paths);
                parents.pop();
            }
            value => paths.push(format!("{path}:{}", kind_of(value))),
        }
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Varint(_) => "varint",
        Value::Fixed64(_) => "fixed64",
        Value::Fixed32(_) => "fixed32",
        Value::Bytes(bytes) if !bytes.is_empty() && std::str::from_utf8(bytes).is_ok() => "string",
        Value::Bytes(_) | Value::Message(_) => "bytes",
    }
}

/// Field `number`'s name in the message at `parents` (SignalService.proto), or its number.
fn name(parents: &[&str], number: u32) -> &'static str {
    let names: &[(u32, &str)] = match parents.last().copied() {
        None => &[
            (1, "dataMessage"),
            (2, "syncMessage"),
            (3, "callMessage"),
            (4, "nullMessage"),
            (5, "receiptMessage"),
            (6, "typingMessage"),
            (7, "senderKeyDistributionMessage"),
            (8, "decryptionErrorMessage"),
            (9, "storyMessage"),
            (10, "pniSignatureMessage"),
            (11, "editMessage"),
        ],
        Some("dataMessage") => &[
            (1, "body"),
            (2, "attachments"),
            (4, "flags"),
            (5, "expireTimer"),
            (6, "profileKey"),
            (7, "timestamp"),
            (8, "quote"),
            (9, "contact"),
            (10, "preview"),
            (11, "sticker"),
            (12, "requiredProtocolVersion"),
            (14, "isViewOnce"),
            (15, "groupV2"),
            (16, "reaction"),
            (17, "delete"),
            (18, "bodyRanges"),
            (19, "groupCallUpdate"),
            (20, "payment"),
            (21, "storyContext"),
            (22, "giftBadge"),
            (23, "expireTimerVersion"),
            (24, "pollCreate"),
            (25, "pollTerminate"),
            (26, "pollVote"),
            (27, "pinMessage"),
            (28, "unpinMessage"),
            (29, "adminDelete"),
        ],
        Some("syncMessage") => &[
            (1, "sent"),
            (2, "contacts"),
            (4, "request"),
            (5, "read"),
            (6, "blocked"),
            (7, "verified"),
            (8, "padding"),
            (9, "configuration"),
            (10, "stickerPackOperation"),
            (11, "viewOnceOpen"),
            (12, "fetchLatest"),
            (13, "keys"),
            (14, "messageRequestResponse"),
            (15, "outgoingPayment"),
            (16, "viewed"),
            (18, "pniChangeNumber"),
            (19, "callEvent"),
            (20, "callLinkUpdate"),
            (21, "callLogEvent"),
            (22, "deleteForMe"),
            (23, "deviceNameChange"),
            (24, "attachmentBackfillRequest"),
            (25, "attachmentBackfillResponse"),
            (26, "usernameChange"),
        ],
        Some("sent") => &[
            (1, "destinationE164"),
            (2, "timestamp"),
            (3, "message"),
            (4, "expirationStartTimestamp"),
            (5, "unidentifiedStatus"),
            (6, "isRecipientUpdate"),
            (7, "destinationServiceId"),
            (8, "storyMessage"),
            (9, "storyMessageRecipients"),
            (10, "editMessage"),
            (12, "destinationServiceIdBinary"),
        ],
        Some("storyMessage") => &[
            (1, "profileKey"),
            (2, "group"),
            (3, "fileAttachment"),
            (4, "textAttachment"),
            (5, "allowsReplies"),
            (6, "bodyRanges"),
        ],
        Some("receiptMessage") => &[(1, "type"), (2, "timestamp")],
        Some("typingMessage") => &[(1, "timestamp"), (2, "action"), (3, "groupId")],
        Some("editMessage") => &[(1, "targetSentTimestamp"), (2, "dataMessage")],
        Some("nullMessage") => &[(1, "padding")],
        _ => &[],
    };
    names
        .iter()
        .find(|(n, _)| *n == number)
        .map_or_else(|| number_name(number), |(_, name)| name)
}

/// A field's number as its name: leaked once per number, of which there are few.
fn number_name(number: u32) -> &'static str {
    use std::sync::{Mutex, OnceLock, PoisonError};
    static NAMES: OnceLock<Mutex<HashMap<u32, &'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    names
        .entry(number)
        .or_insert_with(|| Box::leak(number.to_string().into_boxed_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contents_fields_go_by_their_names() {
        // dataMessage { body: "hi", timestamp: 5, groupV2 { 1: [0xff] } }, and a sender key
        // distribution beside it.
        let bytes = [10, 11, 10, 2, b'h', b'i', 56, 5, 122, 3, 10, 1, 0xff, 58, 1, 0xfe];
        let fields = proto::parse(&bytes).unwrap();
        assert_eq!(kind(&fields), "dataMessage");
        assert_eq!(
            named(&fields),
            [
                "dataMessage.body:string",
                "dataMessage.groupV2.1:bytes",
                "dataMessage.groupV2:message",
                "dataMessage.timestamp:varint",
                "dataMessage:message",
                "senderKeyDistributionMessage:bytes",
            ]
        );
    }
}
