//! TLS 1.3 undone with the servers' key logs: the records of a connection's two sides, as
//! [`super::tcp`] put them back together, opened with the secrets the servers wrote for the
//! session (`SSLKEYLOGFILE`, the NSS key log format), into the plaintext each side sent.
//!
//! Only what the box negotiates: TLS 1.3 (RFC 8446), its three suites, with key updates. A
//! TLS 1.2 session, or one whose client random has no secrets in the logs, decodes to nothing
//! and says so. The handshake's own messages are read for what the decoding needs: the
//! client's hello (what tells the clients' TLS stacks apart) and the server's encrypted
//! extensions (the protocol it chose, ALPN, which says whether HTTP/2 follows).

use std::collections::HashMap;

use ring::{aead, hkdf};
use testbox_tools::connections::{Direction, Hello, Record};

use super::stream::Stream;

/// The secrets of one session, by the labels the key log gives them.
#[derive(Default, Clone)]
pub(super) struct Secrets {
    client_handshake: Option<Vec<u8>>,
    server_handshake: Option<Vec<u8>>,
    client_traffic: Option<Vec<u8>>,
    server_traffic: Option<Vec<u8>>,
    client_early: Option<Vec<u8>>,
    /// TLS 1.2's, which this doesn't decode: noted so that it can say so.
    master: bool,
}

/// What the servers' key logs hold, by the client random of each session.
#[derive(Default)]
pub(super) struct KeyLog {
    sessions: HashMap<[u8; 32], Secrets>,
}

impl KeyLog {
    /// Lines of the NSS key log format: `<label> <client random> <secret>`, in hex; anything
    /// else (comments, cut lines) skipped.
    pub(super) fn parse(text: &str) -> Self {
        let mut log = Self::default();
        for line in text.lines() {
            let mut parts = line.split_ascii_whitespace();
            let (Some(label), Some(random), Some(secret)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Some(random), Some(secret)) = (hex(random), hex(secret)) else {
                continue;
            };
            let Ok(random) = <[u8; 32]>::try_from(random) else {
                continue;
            };
            let session = log.sessions.entry(random).or_default();
            match label {
                "CLIENT_HANDSHAKE_TRAFFIC_SECRET" => session.client_handshake = Some(secret),
                "SERVER_HANDSHAKE_TRAFFIC_SECRET" => session.server_handshake = Some(secret),
                "CLIENT_TRAFFIC_SECRET_0" => session.client_traffic = Some(secret),
                "SERVER_TRAFFIC_SECRET_0" => session.server_traffic = Some(secret),
                "CLIENT_EARLY_TRAFFIC_SECRET" => session.client_early = Some(secret),
                "CLIENT_RANDOM" => session.master = true,
                _ => {}
            }
        }
        log
    }

    pub(super) fn sessions(&self) -> usize {
        self.sessions.len()
    }
}

fn hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

/// A TLS 1.3 cipher suite the box negotiates: its AEAD and its hash.
#[derive(Clone, Copy)]
struct Suite {
    aead: &'static aead::Algorithm,
    hkdf: hkdf::Algorithm,
    hash_length: usize,
}

fn suite(code: u16) -> Option<Suite> {
    Some(match code {
        0x1301 => Suite {
            aead: &aead::AES_128_GCM,
            hkdf: hkdf::HKDF_SHA256,
            hash_length: 32,
        },
        0x1302 => Suite {
            aead: &aead::AES_256_GCM,
            hkdf: hkdf::HKDF_SHA384,
            hash_length: 48,
        },
        0x1303 => Suite {
            aead: &aead::CHACHA20_POLY1305,
            hkdf: hkdf::HKDF_SHA256,
            hash_length: 32,
        },
        _ => return None,
    })
}

struct Length(usize);

impl hkdf::KeyType for Length {
    fn len(&self) -> usize {
        self.0
    }
}

/// HKDF-Expand-Label (RFC 8446 §7.1), with an empty context.
fn expand_label(suite: Suite, secret: &[u8], label: &str, length: usize) -> Option<Vec<u8>> {
    let mut info = Vec::with_capacity(4 + 6 + label.len());
    info.extend_from_slice(&u16::try_from(length).ok()?.to_be_bytes());
    info.push(u8::try_from(6 + label.len()).ok()?);
    info.extend_from_slice(b"tls13 ");
    info.extend_from_slice(label.as_bytes());
    info.push(0);
    let prk = hkdf::Prk::new_less_safe(suite.hkdf, secret);
    let mut out = vec![0; length];
    prk.expand(&[&info], Length(length))
        .ok()?
        .fill(&mut out)
        .ok()?;
    Some(out)
}

/// One side's keys for one traffic secret: its key, its IV, and how many records it opened.
struct Keys {
    suite: Suite,
    secret: Vec<u8>,
    key: aead::LessSafeKey,
    iv: [u8; 12],
    sequence: u64,
}

impl Keys {
    fn derive(suite: Suite, secret: &[u8]) -> Option<Self> {
        let key = expand_label(suite, secret, "key", suite.aead.key_len())?;
        let iv = expand_label(suite, secret, "iv", 12)?;
        Some(Self {
            suite,
            secret: secret.to_vec(),
            key: aead::LessSafeKey::new(aead::UnboundKey::new(suite.aead, &key).ok()?),
            iv: iv.try_into().ok()?,
            sequence: 0,
        })
    }

    /// The next traffic secret's keys, after a KeyUpdate (RFC 8446 §7.2).
    fn updated(&self) -> Option<Self> {
        let next = expand_label(
            self.suite,
            &self.secret,
            "traffic upd",
            self.suite.hash_length,
        )?;
        Self::derive(self.suite, &next)
    }

    /// A record's inner content type and plaintext; `None` if it doesn't open with these keys.
    /// The sequence number counts an attempt that failed too, as the record can't be opened
    /// again anyway.
    fn open(&mut self, header: &[u8], body: &[u8]) -> Option<(u8, Vec<u8>)> {
        let mut nonce = self.iv;
        for (byte, counter) in nonce[4..].iter_mut().zip(self.sequence.to_be_bytes()) {
            *byte ^= counter;
        }
        self.sequence += 1;
        let mut buffer = body.to_vec();
        let opened = self
            .key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(header),
                &mut buffer,
            )
            .ok()?
            .len();
        buffer.truncate(opened);
        // The padding: zeros after the content type.
        while buffer.last() == Some(&0) {
            buffer.pop();
        }
        let content_type = buffer.pop()?;
        Some((content_type, buffer))
    }
}

const CHANGE_CIPHER_SPEC: u8 = 20;
const ALERT: u8 = 21;
const HANDSHAKE: u8 = 22;
const APPLICATION_DATA: u8 = 23;

const CLIENT_HELLO: u8 = 1;
const SERVER_HELLO: u8 = 2;
const ENCRYPTED_EXTENSIONS: u8 = 8;
const FINISHED: u8 = 20;
const KEY_UPDATE: u8 = 24;

/// What the handshake said that the decoding keeps.
#[derive(Default)]
pub(super) struct Handshake {
    pub(super) hello: Hello,
    pub(super) random: Option<[u8; 32]>,
    pub(super) version: Option<String>,
    pub(super) suite: Option<String>,
    pub(super) alpn: Option<String>,
}

/// A connection's two sides decoded: its handshake, its plaintext records in the order they
/// came, and why it stopped if it did.
pub(super) struct Decoded {
    pub(super) handshake: Handshake,
    pub(super) records: Vec<Record>,
    pub(super) failed: Option<String>,
}

/// Where one side's decoding stands.
enum Phase {
    /// Before the hello, or the handshake's plaintext part.
    Plain,
    /// The handshake's encrypted part, up to this side's Finished.
    Handshaking(Keys),
    /// Application data.
    Traffic(Keys),
    Stopped,
}

/// One side of the connection as the decoding reads it.
struct Reader {
    direction: Direction,
    phase: Phase,
    /// Handshake messages' bytes not yet whole, across records.
    handshake: Vec<u8>,
}

/// Opens `sent` and `received`, the client's and the server's bytes, with `log`'s secrets.
pub(super) fn decode(sent: &Stream, received: &Stream, log: &KeyLog) -> Decoded {
    let mut decoded = Decoded {
        handshake: Handshake::default(),
        records: Vec::new(),
        failed: None,
    };
    let mut secrets = None;
    let mut suite_chosen = None;
    let mut client = Reader {
        direction: Direction::Sent,
        phase: Phase::Plain,
        handshake: Vec::new(),
    };
    let mut server = Reader {
        direction: Direction::Received,
        phase: Phase::Plain,
        handshake: Vec::new(),
    };
    // The records of both sides, each in its own order, taken in the order they came: the
    // server's answer to a hello comes after the hello, which the client's keys need.
    let mut records: Vec<(u64, Direction, &[u8], &[u8])> = Vec::new();
    for (stream, direction) in [(sent, Direction::Sent), (received, Direction::Received)] {
        let mut at = 0;
        while let Some(header) = stream.bytes.get(at..at + 5) {
            let length = usize::from(u16::from_be_bytes([header[3], header[4]]));
            let Some(body) = stream.bytes.get(at + 5..at + 5 + length) else {
                break;
            };
            records.push((stream.at(at), direction, header, body));
            at += 5 + length;
        }
    }
    records.sort_by_key(|&(at, direction, _, _)| (at, direction == Direction::Received));
    let mut finished = false;

    for (at, direction, header, body) in records {
        let reader = match direction {
            Direction::Sent => &mut client,
            _ => &mut server,
        };
        let content_type = header[0];
        let (inner_type, plaintext): (u8, Vec<u8>) = match &mut reader.phase {
            Phase::Stopped => continue,
            Phase::Plain => {
                if content_type == CHANGE_CIPHER_SPEC {
                    continue;
                }
                (content_type, body.to_vec())
            }
            Phase::Handshaking(keys) | Phase::Traffic(keys) => {
                if content_type == CHANGE_CIPHER_SPEC {
                    continue;
                }
                if content_type != APPLICATION_DATA {
                    // Plaintext after the keys: an alert before the handshake finished.
                    if content_type == ALERT {
                        continue;
                    }
                    decoded.failed.get_or_insert_with(|| {
                        format!("a plaintext record of type {content_type} under encryption")
                    });
                    reader.phase = Phase::Stopped;
                    continue;
                }
                match keys.open(header, body) {
                    Some(opened) => opened,
                    None => {
                        decoded.failed.get_or_insert_with(|| {
                            format!("a record of {direction:?} didn't open")
                        });
                        reader.phase = Phase::Stopped;
                        continue;
                    }
                }
            }
        };
        match inner_type {
            APPLICATION_DATA => {
                if matches!(reader.phase, Phase::Traffic(_)) {
                    decoded.records.push(Record {
                        at,
                        direction: reader.direction,
                        bytes: plaintext,
                    });
                }
            }
            HANDSHAKE => {
                reader.handshake.extend_from_slice(&plaintext);
                while let Some((kind, message, rest)) = next_message(&reader.handshake) {
                    let message = message.to_vec();
                    reader.handshake = rest.to_vec();
                    match kind {
                        CLIENT_HELLO => {
                            client_hello(&message, &mut decoded.handshake);
                            secrets = decoded
                                .handshake
                                .random
                                .and_then(|random| log.sessions.get(&random).cloned());
                        }
                        SERVER_HELLO => {
                            let chosen = server_hello(&message, &mut decoded.handshake);
                            suite_chosen = chosen;
                            // From here on both sides encrypt: the server its handshake, the
                            // client its Finished.
                            let Some(secrets) = &secrets else {
                                decoded.failed = Some("no secrets for the session".to_owned());
                                client.phase = Phase::Stopped;
                                server.phase = Phase::Stopped;
                                break;
                            };
                            if decoded.handshake.version.as_deref() != Some("TLSv1_3") {
                                decoded.failed = Some(format!(
                                    "{}, which isn't decoded",
                                    decoded.handshake.version.as_deref().unwrap_or("no version")
                                ));
                                client.phase = Phase::Stopped;
                                server.phase = Phase::Stopped;
                                break;
                            }
                            let Some(suite) = chosen.and_then(suite) else {
                                decoded.failed = Some(format!(
                                    "the suite {}, which isn't decoded",
                                    decoded.handshake.suite.as_deref().unwrap_or("?")
                                ));
                                client.phase = Phase::Stopped;
                                server.phase = Phase::Stopped;
                                break;
                            };
                            let keys = |secret: &Option<Vec<u8>>| {
                                secret
                                    .as_deref()
                                    .and_then(|secret| Keys::derive(suite, secret))
                            };
                            match (
                                keys(&secrets.server_handshake),
                                keys(&secrets.client_handshake),
                            ) {
                                (Some(server_keys), Some(client_keys)) => {
                                    server.phase = Phase::Handshaking(server_keys);
                                    client.phase = Phase::Handshaking(client_keys);
                                }
                                _ => {
                                    decoded.failed = Some(if secrets.master {
                                        "a TLS 1.2 session, which isn't decoded".to_owned()
                                    } else {
                                        "the session's handshake secrets are missing".to_owned()
                                    });
                                    client.phase = Phase::Stopped;
                                    server.phase = Phase::Stopped;
                                }
                            }
                            break;
                        }
                        ENCRYPTED_EXTENSIONS => {
                            decoded.handshake.alpn = alpn(&message);
                        }
                        FINISHED => {
                            // This side's traffic keys from here on.
                            let traffic = match reader.direction {
                                Direction::Sent => {
                                    secrets.as_ref().and_then(|s| s.client_traffic.clone())
                                }
                                _ => secrets.as_ref().and_then(|s| s.server_traffic.clone()),
                            };
                            let keys = suite_chosen
                                .and_then(suite)
                                .zip(traffic)
                                .and_then(|(suite, secret)| Keys::derive(suite, &secret));
                            match keys {
                                Some(keys) => {
                                    // The client's Finished ends the handshake.
                                    finished |= reader.direction == Direction::Sent;
                                    reader.phase = Phase::Traffic(keys);
                                }
                                None => {
                                    decoded.failed.get_or_insert_with(|| {
                                        "the session's traffic secrets are missing".to_owned()
                                    });
                                    reader.phase = Phase::Stopped;
                                }
                            }
                        }
                        KEY_UPDATE => {
                            if let Phase::Traffic(keys) = &reader.phase {
                                match keys.updated() {
                                    Some(updated) => reader.phase = Phase::Traffic(updated),
                                    None => reader.phase = Phase::Stopped,
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Alerts, and anything else: nothing the decoding keeps.
            _ => {}
        }
    }
    if decoded.failed.is_none() && !finished {
        decoded.failed = Some(if decoded.handshake.random.is_none() {
            "no client hello".to_owned()
        } else if decoded.handshake.suite.is_none() {
            "no server hello".to_owned()
        } else {
            "the handshake didn't finish".to_owned()
        });
    }
    decoded
}

/// The next whole handshake message in `bytes`: its type, its body, and what follows.
fn next_message(bytes: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let header = bytes.get(..4)?;
    let length =
        usize::from(header[1]) << 16 | usize::from(header[2]) << 8 | usize::from(header[3]);
    let body = bytes.get(4..4 + length)?;
    Some((header[0], body, &bytes[4 + length..]))
}

/// A length-prefixed vector: the bytes after a prefix of `width` bytes, and what follows.
fn vector(bytes: &[u8], width: usize) -> Option<(&[u8], &[u8])> {
    let prefix = bytes.get(..width)?;
    let length = prefix
        .iter()
        .fold(0usize, |length, &byte| length << 8 | usize::from(byte));
    let body = bytes.get(width..width + length)?;
    Some((body, &bytes[width + length..]))
}

/// The client's hello: its random, and what tells its stack apart.
fn client_hello(message: &[u8], handshake: &mut Handshake) {
    let Some(random) = message.get(2..34) else {
        return;
    };
    handshake.random = random.try_into().ok();
    let Some((_session, rest)) = vector(&message[34..], 1) else {
        return;
    };
    let Some((suites, rest)) = vector(rest, 2) else {
        return;
    };
    handshake.hello.cipher_suites = suites
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| suite_name(u16::from_be_bytes(pair)))
        .collect();
    let Some((_compression, rest)) = vector(rest, 1) else {
        return;
    };
    let Some((mut extensions, _)) = vector(rest, 2) else {
        return;
    };
    while let Some(kind) = extensions.get(..2) {
        let kind = u16::from_be_bytes([kind[0], kind[1]]);
        let Some((body, rest)) = vector(&extensions[2..], 2) else {
            break;
        };
        extensions = rest;
        match kind {
            // server_name: a list of one name of type 0.
            0 => {
                if let Some((list, _)) = vector(body, 2)
                    && list.first() == Some(&0)
                    && let Some((name, _)) = vector(&list[1..], 2)
                {
                    handshake.hello.server_name = Some(String::from_utf8_lossy(name).into_owned());
                }
            }
            // signature_algorithms.
            13 => {
                if let Some((schemes, _)) = vector(body, 2) {
                    handshake.hello.signature_schemes = schemes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&pair| scheme_name(u16::from_be_bytes(pair)))
                        .collect();
                }
            }
            // application_layer_protocol_negotiation.
            16 => {
                handshake.hello.alpn = protocols(body);
            }
            _ => {}
        }
    }
}

/// The names in an ALPN extension's body.
fn protocols(body: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    if let Some((mut list, _)) = vector(body, 2) {
        while let Some((name, rest)) = vector(list, 1) {
            names.push(String::from_utf8_lossy(name).into_owned());
            list = rest;
        }
    }
    names
}

/// The server's hello: the suite it chose (returned, for the keys) and the version.
fn server_hello(message: &[u8], handshake: &mut Handshake) -> Option<u16> {
    let (_session, rest) = vector(message.get(34..)?, 1)?;
    let code = u16::from_be_bytes([*rest.first()?, *rest.get(1)?]);
    handshake.suite = Some(suite_name(code));
    // Legacy version 0x0303 everywhere; TLS 1.3 says so in supported_versions.
    let mut version = legacy_version(message.get(..2)?);
    let (mut extensions, _) = vector(rest.get(3..)?, 2)?;
    while let Some(kind) = extensions.get(..2) {
        let kind = u16::from_be_bytes([kind[0], kind[1]]);
        let Some((body, rest)) = vector(&extensions[2..], 2) else {
            break;
        };
        extensions = rest;
        if kind == 43 && body.len() == 2 {
            version = legacy_version(body);
        }
    }
    handshake.version = Some(version);
    Some(code)
}

fn legacy_version(bytes: &[u8]) -> String {
    match bytes {
        [3, 4] => "TLSv1_3".to_owned(),
        [3, 3] => "TLSv1_2".to_owned(),
        [3, 2] => "TLSv1_1".to_owned(),
        [3, 1] => "TLSv1_0".to_owned(),
        other => format!("{other:02x?}"),
    }
}

/// The protocol the server chose, from its encrypted extensions.
fn alpn(message: &[u8]) -> Option<String> {
    let (mut extensions, _) = vector(message, 2)?;
    while let Some(kind) = extensions.get(..2) {
        let kind = u16::from_be_bytes([kind[0], kind[1]]);
        let (body, rest) = vector(&extensions[2..], 2)?;
        extensions = rest;
        if kind == 16 {
            return protocols(body).into_iter().next();
        }
    }
    None
}

/// A cipher suite's name as rustls prints it (`CipherSuite`'s `Debug`), which the records
/// from before have: the same client, the same fingerprint.
fn suite_name(code: u16) -> String {
    let name = match code {
        0x1301 => "TLS13_AES_128_GCM_SHA256",
        0x1302 => "TLS13_AES_256_GCM_SHA384",
        0x1303 => "TLS13_CHACHA20_POLY1305_SHA256",
        0xC02B => "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
        0xC02C => "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
        0xCCA9 => "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256",
        0xC02F => "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
        0xC030 => "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
        0xCCA8 => "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256",
        0x00FF => "TLS_EMPTY_RENEGOTIATION_INFO_SCSV",
        0xC027 => "TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA256",
        0xC028 => "TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA384",
        0xC023 => "TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA256",
        0xC024 => "TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA384",
        0xC009 => "TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA",
        0xC00A => "TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA",
        0xC013 => "TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA",
        0xC014 => "TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA",
        other => return format!("CipherSuite({other:#x})"),
    };
    name.to_owned()
}

/// A signature scheme's name as rustls prints it (`SignatureScheme`'s `Debug`).
fn scheme_name(code: u16) -> String {
    let name = match code {
        0x0201 => "RSA_PKCS1_SHA1",
        0x0203 => "ECDSA_SHA1_Legacy",
        0x0401 => "RSA_PKCS1_SHA256",
        0x0501 => "RSA_PKCS1_SHA384",
        0x0601 => "RSA_PKCS1_SHA512",
        0x0403 => "ECDSA_NISTP256_SHA256",
        0x0503 => "ECDSA_NISTP384_SHA384",
        0x0603 => "ECDSA_NISTP521_SHA512",
        0x0804 => "RSA_PSS_SHA256",
        0x0805 => "RSA_PSS_SHA384",
        0x0806 => "RSA_PSS_SHA512",
        0x0807 => "ED25519",
        0x0808 => "ED448",
        0x0904 => "ML_DSA_44",
        0x0905 => "ML_DSA_65",
        0x0906 => "ML_DSA_87",
        other => return format!("SignatureScheme({other:#x})"),
    };
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Handshake, KeyLog, client_hello, expand_label, hex, suite};

    #[test]
    fn the_key_log_is_read_by_session() {
        let log = KeyLog::parse(
            "# a comment\n\
             CLIENT_HANDSHAKE_TRAFFIC_SECRET 0000000000000000000000000000000000000000000000000000000000000001 aa\n\
             SERVER_TRAFFIC_SECRET_0 0000000000000000000000000000000000000000000000000000000000000001 bb\n\
             CLIENT_RANDOM 0000000000000000000000000000000000000000000000000000000000000002 cc\n\
             nonsense\n",
        );
        assert_eq!(log.sessions(), 2);
        let mut one = [0; 32];
        one[31] = 1;
        let session = &log.sessions[&one];
        assert_eq!(session.client_handshake.as_deref(), Some(&[0xAA][..]));
        assert_eq!(session.server_traffic.as_deref(), Some(&[0xBB][..]));
        assert!(!session.master);
        let mut two = [0; 32];
        two[31] = 2;
        assert!(log.sessions[&two].master);
        assert_eq!(hex("0g"), None);
    }

    /// RFC 8448 §3, the simple 1-RTT handshake: the server's handshake traffic secret and the
    /// key and IV it expands to.
    #[test]
    fn expand_label_as_rfc_8448() {
        let secret =
            hex("b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38").unwrap();
        let suite = suite(0x1301).unwrap();
        assert_eq!(
            expand_label(suite, &secret, "key", 16).unwrap(),
            hex("3fce516009c21727d0f2e4e86ee403bc").unwrap()
        );
        assert_eq!(
            expand_label(suite, &secret, "iv", 12).unwrap(),
            hex("5d313eb2671276ee13000b30").unwrap()
        );
    }

    #[test]
    fn a_client_hello_is_read() {
        let mut message = vec![3, 3];
        message.extend_from_slice(&[7; 32]);
        message.push(0);
        message.extend_from_slice(&[0, 4, 0x13, 0x02, 0x0a, 0x0a]);
        message.extend_from_slice(&[1, 0]);
        let mut extensions = Vec::new();
        // server_name: localhost.
        extensions.extend_from_slice(&[0, 0, 0, 14, 0, 12, 0, 0, 9]);
        extensions.extend_from_slice(b"localhost");
        // signature_algorithms: ed25519, and one rustls doesn't name.
        extensions.extend_from_slice(&[0, 13, 0, 6, 0, 4, 0x08, 0x07, 0x08, 0x1a]);
        // alpn: h2, http/1.1.
        extensions.extend_from_slice(&[0, 16, 0, 14, 0, 12, 2]);
        extensions.extend_from_slice(b"h2");
        extensions.push(8);
        extensions.extend_from_slice(b"http/1.1");
        message.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        message.extend_from_slice(&extensions);
        let mut handshake = Handshake::default();
        client_hello(&message, &mut handshake);
        assert_eq!(handshake.random, Some([7; 32]));
        assert_eq!(handshake.hello.server_name.as_deref(), Some("localhost"));
        assert_eq!(
            handshake.hello.cipher_suites,
            ["TLS13_AES_256_GCM_SHA384", "CipherSuite(0xa0a)"]
        );
        assert_eq!(
            handshake.hello.signature_schemes,
            ["ED25519", "SignatureScheme(0x81a)"]
        );
        assert_eq!(handshake.hello.alpn, ["h2", "http/1.1"]);
    }
}
