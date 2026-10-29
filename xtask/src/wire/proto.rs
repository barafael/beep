//! Protobuf without its schema: fields by number and wire type, a field of bytes read as a
//! message where it parses as one. Enough to say which fields a client fills in; and Signal's
//! `WebSocketMessage` (libsignal-service's websocket.proto), which every chat message is in.

/// A field's value as the wire has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    /// Bytes that parse as a message, or the bytes.
    Message(Vec<Field>),
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Field {
    pub(crate) number: u32,
    pub(crate) value: Value,
}

/// How deep a field of bytes is still tried as a message.
const DEPTH: usize = 8;
/// Signal's messages number their fields in the tens: bytes that parse only with larger numbers
/// are bytes (ciphertext that happens to parse).
const LARGEST_FIELD: u32 = 1000;

/// `bytes` as a message, if all of them parse as one; empty bytes are the empty message.
pub(crate) fn parse(bytes: &[u8]) -> Option<Vec<Field>> {
    parse_at(bytes, DEPTH)
}

fn parse_at(mut bytes: &[u8], depth: usize) -> Option<Vec<Field>> {
    let mut fields = Vec::new();
    while !bytes.is_empty() {
        let key = varint(&mut bytes)?;
        let number = u32::try_from(key >> 3).ok().filter(|&number| number != 0)?;
        let value = match key & 7 {
            0 => Value::Varint(varint(&mut bytes)?),
            1 => Value::Fixed64(u64::from_le_bytes(take(&mut bytes, 8)?.try_into().ok()?)),
            2 => {
                let length = usize::try_from(varint(&mut bytes)?).ok()?;
                let inner = take(&mut bytes, length)?;
                match depth
                    .checked_sub(1)
                    .filter(|_| plausible_message(inner))
                    .and_then(|depth| parse_at(inner, depth))
                    .filter(|fields| fields.iter().all(|field| field.number <= LARGEST_FIELD))
                {
                    Some(message) => Value::Message(message),
                    None => Value::Bytes(inner.to_vec()),
                }
            }
            5 => Value::Fixed32(u32::from_le_bytes(take(&mut bytes, 4)?.try_into().ok()?)),
            _ => return None,
        };
        fields.push(Field { number, value });
    }
    Some(fields)
}

/// Text that happens to parse as a message is still text: printable UTF-8 is taken as a
/// string. Empty bytes stay bytes.
fn plausible_message(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && !std::str::from_utf8(bytes).is_ok_and(|text| {
            text.chars()
                .all(|c| !c.is_control() || c == '\n' || c == '\t')
        })
}

fn varint(bytes: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = bytes.split_first()?;
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn take<'a>(bytes: &mut &'a [u8], length: usize) -> Option<&'a [u8]> {
    if bytes.len() < length {
        return None;
    }
    let (taken, rest) = bytes.split_at(length);
    *bytes = rest;
    Some(taken)
}

/// The fields a message has, as paths: `3.1:varint`, nested by number, each once, in order.
pub(crate) fn shape(fields: &[Field]) -> Vec<String> {
    let mut paths = Vec::new();
    collect(fields, "", &mut paths);
    paths.sort();
    paths.dedup();
    paths
}

fn collect(fields: &[Field], prefix: &str, paths: &mut Vec<String>) {
    for field in fields {
        let path = format!("{prefix}{}", field.number);
        match &field.value {
            Value::Message(inner) => {
                paths.push(format!("{path}:message"));
                collect(inner, &format!("{path}."), paths);
            }
            Value::Varint(_) => paths.push(format!("{path}:varint")),
            Value::Fixed64(_) => paths.push(format!("{path}:fixed64")),
            Value::Fixed32(_) => paths.push(format!("{path}:fixed32")),
            Value::Bytes(bytes) if std::str::from_utf8(bytes).is_ok() && !bytes.is_empty() => {
                paths.push(format!("{path}:string"));
            }
            Value::Bytes(_) => paths.push(format!("{path}:bytes")),
        }
    }
}

/// An envelope's fields by name (SignalService.proto's `Envelope`), its content left whole: it
/// is end-to-end encrypted.
pub(crate) fn envelope_shape(bytes: &[u8]) -> Option<Vec<String>> {
    let fields = parse_at(bytes, 0)?;
    let mut shape: Vec<String> = fields
        .iter()
        .map(|field| {
            let name = match field.number {
                1 => "type",
                5 => "clientTimestamp",
                7 => "sourceDeviceId",
                8 => "content",
                9 => "serverGuid",
                10 => "serverTimestamp",
                11 => "sourceServiceId",
                12 => "ephemeral",
                13 => "destinationServiceId",
                14 => "urgent",
                15 => "updatedPni",
                16 => "story",
                17 => "report_spam_token",
                19 => "sourceServiceIdBinary",
                20 => "destinationServiceIdBinary",
                21 => "serverGuidBinary",
                22 => "updatedPniBinary",
                _ => return format!("{}", field.number),
            };
            match field.value {
                // Which kind of envelope it is says the most.
                Value::Varint(kind) if field.number == 1 => format!("type={kind}"),
                _ => name.to_owned(),
            }
        })
        .collect();
    shape.sort();
    shape.dedup();
    Some(shape)
}

/// Signal's chat message, websocket.proto's `WebSocketMessage`: a request or a response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChatMessage {
    Request {
        id: Option<u64>,
        verb: String,
        path: String,
        headers: Vec<String>,
        body: Option<Vec<u8>>,
    },
    Response {
        id: Option<u64>,
        status: Option<u32>,
        message: Option<String>,
        headers: Vec<String>,
        body: Option<Vec<u8>>,
    },
}

impl ChatMessage {
    /// `WebSocketMessage { type = 1; request = 2; response = 3; }`, its request
    /// `{ verb = 1; path = 2; body = 3; headers = 5; id = 4; }` and response
    /// `{ id = 1; status = 2; message = 3; headers = 5; body = 4; }`.
    pub(crate) fn parse(bytes: &[u8]) -> Option<Self> {
        let outer = parse_at(bytes, 0)?;
        let inner = |number| {
            outer
                .iter()
                .find_map(|field| match (&field.value, field.number) {
                    (Value::Bytes(bytes), n) if n == number => parse_at(bytes, 0),
                    _ => None,
                })
        };
        let kind = outer.iter().find_map(|field| match field.value {
            Value::Varint(kind) if field.number == 1 => Some(kind),
            _ => None,
        });
        match kind {
            Some(1) => {
                let request = inner(2)?;
                Some(Self::Request {
                    id: varint_of(&request, 4),
                    verb: text_of(&request, 1).unwrap_or_default(),
                    path: text_of(&request, 2).unwrap_or_default(),
                    headers: texts_of(&request, 5),
                    body: bytes_of(&request, 3),
                })
            }
            Some(2) => {
                let response = inner(3)?;
                Some(Self::Response {
                    id: varint_of(&response, 1),
                    status: varint_of(&response, 2).and_then(|status| u32::try_from(status).ok()),
                    message: text_of(&response, 3),
                    headers: texts_of(&response, 5),
                    body: bytes_of(&response, 4),
                })
            }
            _ => None,
        }
    }
}

fn varint_of(fields: &[Field], number: u32) -> Option<u64> {
    fields.iter().find_map(|field| match field.value {
        Value::Varint(value) if field.number == number => Some(value),
        _ => None,
    })
}

fn bytes_of(fields: &[Field], number: u32) -> Option<Vec<u8>> {
    fields.iter().find_map(|field| match &field.value {
        Value::Bytes(bytes) if field.number == number => Some(bytes.clone()),
        _ => None,
    })
}

fn text_of(fields: &[Field], number: u32) -> Option<String> {
    bytes_of(fields, number).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn texts_of(fields: &[Field], number: u32) -> Vec<String> {
    fields
        .iter()
        .filter_map(|field| match &field.value {
            Value::Bytes(bytes) if field.number == number => {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{ChatMessage, parse, shape};

    /// A field of bytes: key, length, bytes.
    pub(crate) fn bytes_field(number: u8, bytes: &[u8]) -> Vec<u8> {
        let mut field = vec![(number << 3) | 2];
        let mut length = bytes.len();
        loop {
            let byte = (length & 0x7f) as u8;
            length >>= 7;
            if length == 0 {
                field.push(byte);
                break;
            }
            field.push(byte | 0x80);
        }
        field.extend_from_slice(bytes);
        field
    }

    /// A `WebSocketMessage` request, as a client sends one.
    pub(crate) fn chat_request(id: u8, verb: &str, path: &str, body: &[u8]) -> Vec<u8> {
        let mut request = bytes_field(1, verb.as_bytes());
        request.extend(bytes_field(2, path.as_bytes()));
        request.extend(bytes_field(3, body));
        request.extend([4 << 3, id]);
        request.extend(bytes_field(5, b"content-type:application/json"));
        let mut message = vec![1 << 3, 1];
        message.extend(bytes_field(2, &request));
        message
    }

    #[test]
    fn a_message_has_its_fields_as_paths() {
        // { 1: 150, 2: { 1: "hi" }, 3: "text" }
        let mut bytes = vec![0x08, 0x96, 0x01];
        bytes.extend(bytes_field(2, &bytes_field(1, b"hi")));
        bytes.extend(bytes_field(3, b"text"));
        let fields = parse(&bytes).unwrap();
        assert_eq!(
            shape(&fields),
            ["1:varint", "2.1:string", "2:message", "3:string"]
        );
    }

    #[test]
    fn what_doesnt_parse_isnt_a_message() {
        assert!(parse(b"\xff\xff").is_none());
        assert!(parse(&[0x0a, 0x05, 1]).is_none());
    }

    #[test]
    fn a_chat_request_reads_back() {
        let bytes = chat_request(7, "PUT", "/v1/messages/abc", br#"{"a":1}"#);
        assert_eq!(
            ChatMessage::parse(&bytes),
            Some(ChatMessage::Request {
                id: Some(7),
                verb: "PUT".to_owned(),
                path: "/v1/messages/abc".to_owned(),
                headers: vec!["content-type:application/json".to_owned()],
                body: Some(br#"{"a":1}"#.to_vec()),
            })
        );
    }
}
