//! Editing protobuf messages in their bytes, field by field, keeping every field this code
//! doesn't touch exactly as it was — including fields newer than the `.proto` files langis was
//! built with, which decoding and encoding again would drop.

/// A field as found in a message: its number, and the bytes it takes (tag included).
struct Field {
    number: u64,
    /// For length-delimited fields, where the payload starts and ends.
    payload: Option<(usize, usize)>,
    start: usize,
    end: usize,
}

fn read_varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// The fields of a message, in order; `None` if it isn't well-formed (or uses groups).
fn fields(message: &[u8]) -> Option<Vec<Field>> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < message.len() {
        let start = at;
        let tag = read_varint(message, &mut at)?;
        let (number, wire_type) = (tag >> 3, tag & 7);
        let mut payload = None;
        match wire_type {
            0 => {
                read_varint(message, &mut at)?;
            }
            1 => at = at.checked_add(8)?,
            2 => {
                let length = usize::try_from(read_varint(message, &mut at)?).ok()?;
                let end = at.checked_add(length)?;
                payload = Some((at, end));
                at = end;
            }
            5 => at = at.checked_add(4)?,
            _ => return None,
        }
        if at > message.len() || number == 0 {
            return None;
        }
        found.push(Field {
            number,
            payload,
            start,
            end: at,
        });
    }
    Some(found)
}

/// `message` without field `number`, then with `extra` (already encoded fields) at the end.
fn without(message: &[u8], number: u64, extra: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(message.len() + extra.len());
    for field in fields(message)? {
        if field.number != number {
            out.extend_from_slice(&message[field.start..field.end]);
        }
    }
    out.extend_from_slice(extra);
    Some(out)
}

/// Sets a varint field (bool, uint64, …); 0 removes it, as proto3 leaves defaults out.
pub(crate) fn set_varint(message: &[u8], number: u64, value: u64) -> Option<Vec<u8>> {
    let mut field = Vec::new();
    if value != 0 {
        put_varint(&mut field, number << 3);
        put_varint(&mut field, value);
    }
    without(message, number, &field)
}

/// Replaces every occurrence of a repeated length-delimited field with `values`.
pub(crate) fn set_repeated(message: &[u8], number: u64, values: &[Vec<u8>]) -> Option<Vec<u8>> {
    let mut fields = Vec::new();
    for value in values {
        put_varint(&mut fields, (number << 3) | 2);
        put_varint(&mut fields, value.len() as u64);
        fields.extend_from_slice(value);
    }
    without(message, number, &fields)
}

/// Sets a length-delimited field (bytes, string) to `value`; empty removes it.
pub(crate) fn set_bytes(message: &[u8], number: u64, value: &[u8]) -> Option<Vec<u8>> {
    let mut field = Vec::new();
    if !value.is_empty() {
        put_varint(&mut field, (number << 3) | 2);
        put_varint(&mut field, value.len() as u64);
        field.extend_from_slice(value);
    }
    without(message, number, &field)
}

/// The payloads of every occurrence of length-delimited field `number`.
#[cfg(test)]
pub(crate) fn repeated(message: &[u8], number: u64) -> Option<Vec<&[u8]>> {
    Some(
        fields(message)?
            .into_iter()
            .filter(|f| f.number == number)
            .filter_map(|f| f.payload)
            .map(|(from, to)| &message[from..to])
            .collect(),
    )
}

/// Changes every occurrence of the embedded message in field `number` that `edit` returns
/// something for, where it stands; the others stay as they are.
pub(crate) fn edit_each(
    message: &[u8],
    number: u64,
    mut edit: impl FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(message.len());
    for field in fields(message)? {
        match field.payload.filter(|_| field.number == number) {
            Some((from, to)) => match edit(&message[from..to]) {
                Some(edited) => {
                    put_varint(&mut out, (number << 3) | 2);
                    put_varint(&mut out, edited.len() as u64);
                    out.extend_from_slice(&edited);
                }
                None => out.extend_from_slice(&message[field.start..field.end]),
            },
            None => out.extend_from_slice(&message[field.start..field.end]),
        }
    }
    Some(out)
}

/// Changes the embedded message in field `number` (the first, if repeated) where it stands.
pub(crate) fn edit_message(
    message: &[u8],
    number: u64,
    edit: impl FnOnce(&[u8]) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let all = fields(message)?;
    let field = all.iter().find(|f| f.number == number)?;
    let (from, to) = field.payload?;
    let edited = edit(&message[from..to])?;
    let mut out = Vec::with_capacity(message.len() + edited.len());
    out.extend_from_slice(&message[..field.start]);
    put_varint(&mut out, (number << 3) | 2);
    put_varint(&mut out, edited.len() as u64);
    out.extend_from_slice(&edited);
    out.extend_from_slice(&message[field.end..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use presage::libsignal_service::prelude::ProtobufMessage as _;
    use presage::proto::{ContactRecord, StorageRecord, storage_record::Record};

    /// A record as the phone might write it, with a field from a newer Signal (number 999).
    fn stored() -> Vec<u8> {
        let contact = ContactRecord {
            aci: "a".into(),
            given_name: "Ann".into(),
            archived: true,
            muted_until_timestamp: 5,
            ..Default::default()
        };
        let mut contact = contact.encode_to_vec();
        // Field 999, length-delimited: "new".
        put_varint(&mut contact, (999 << 3) | 2);
        put_varint(&mut contact, 3);
        contact.extend_from_slice(b"new");
        let mut record = Vec::new();
        put_varint(&mut record, (1 << 3) | 2);
        put_varint(&mut record, contact.len() as u64);
        record.extend_from_slice(&contact);
        record
    }

    #[test]
    fn edits_keep_unknown_fields() {
        let edited = edit_message(&stored(), 1, |contact| {
            let contact = set_varint(contact, 11, 0)?; // unarchive
            let contact = set_varint(&contact, 9, 1)?; // block
            set_varint(&contact, 13, 9) // mute longer
        })
        .unwrap();
        let Some(Record::Contact(contact)) = StorageRecord::decode(&*edited).unwrap().record else {
            panic!("still a contact");
        };
        assert!(!contact.archived && contact.blocked);
        assert_eq!(
            (contact.muted_until_timestamp, contact.given_name.as_str()),
            (9, "Ann")
        );
        // The field langis doesn't know is still there.
        assert!(edited.windows(3).any(|w| w == b"new"));
    }

    #[test]
    fn repeated_fields_are_replaced_as_a_whole() {
        let message = set_repeated(&[], 14, &[b"one".to_vec(), b"two".to_vec()]).unwrap();
        let message = set_varint(&message, 6, 1).unwrap();
        let message = set_repeated(&message, 14, &[b"three".to_vec()]).unwrap();
        let fields = fields(&message).unwrap();
        let numbers: Vec<u64> = fields.iter().map(|f| f.number).collect();
        assert_eq!(numbers, [6, 14]);
    }

    #[test]
    fn repeated_messages_are_edited_one_by_one() {
        let message = set_repeated(&[], 2, &[b"aa".to_vec(), b"bb".to_vec()]).unwrap();
        let message = set_varint(&message, 1, 7).unwrap();
        let edited = edit_each(&message, 2, |m| (m == b"bb").then(|| b"cc".to_vec())).unwrap();
        assert_eq!(repeated(&edited, 2).unwrap(), [&b"aa"[..], &b"cc"[..]]);
        let replaced = set_bytes(&edited, 2, b"").unwrap();
        assert!(repeated(&replaced, 2).unwrap().is_empty());
    }

    #[test]
    fn broken_messages_are_left_alone() {
        assert!(set_varint(&[0x0a, 0x05, 1], 3, 1).is_none(), "too short");
        assert!(
            set_varint(&[0x0b], 3, 1).is_none(),
            "groups aren't supported"
        );
    }
}
