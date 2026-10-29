//! What `--capture`'s tap wrote down (testbox/tools `tap`), decoded: each request a client or
//! the server made, on which connection, with what it carried, as `events.jsonl` in the box's
//! record's `wire/`, a transcript per client beside it, and each connection's summary
//! (`connections.jsonl`). The point is the comparison of Signal's own clients with b33p
//! ([`compare`]): which requests each makes, how, and how often.
//!
//! The layers: TLS's plaintext, HTTP/1.1 ([`h1`]) or HTTP/2 ([`h2`]); WebSocket ([`ws`]), after
//! an upgrade or in an extended CONNECT; Signal's `WebSocketMessage` and protobuf without its
//! schema ([`proto`]); gRPC's messages. What lies inside an envelope is end-to-end encrypted.

mod compare;
mod h1;
mod h2;
mod proto;
mod stream;
mod template;
mod ws;

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use testbox_tools::tap::{Connection, Record};

use crate::checks::record::Span;
use crate::error::{Context, Result, WithContext};
use h1::{Headers, header};
use proto::ChatMessage;
use stream::Stream;

/// What a request carried, or its response: its size, what it is, and its shape (JSON's key
/// paths, protobuf's field numbers).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Body {
    pub(crate) size: usize,
    pub(crate) kind: &'static str,
    pub(crate) shape: Vec<String>,
}

/// A request and its answer, on the wire as a client or the server made it.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Event {
    pub(crate) at: u64,
    pub(crate) connection: u64,
    pub(crate) port: u16,
    pub(crate) client: String,
    /// `h1`, `h2`, `ws` (a chat message), `grpc`, or a control frame's: `ws-control`,
    /// `h2-control`.
    pub(crate) transport: &'static str,
    /// The server asked, the client answered: the chat's deliveries.
    pub(crate) from_server: bool,
    pub(crate) verb: String,
    pub(crate) path: String,
    pub(crate) template: String,
    pub(crate) status: Option<u32>,
    pub(crate) took_ms: Option<f64>,
    /// The request's headers' names, sorted.
    pub(crate) headers: Vec<String>,
    /// The values of those worth comparing ([`NOTED`]).
    pub(crate) noted: Vec<String>,
    pub(crate) request: Body,
    pub(crate) response: Body,
}

/// A connection as decoded: whose it was, what it was, and what of it didn't decode.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Summary {
    pub(crate) n: u64,
    pub(crate) port: u16,
    pub(crate) client: String,
    /// What it said of who made it: its agent, its user.
    pub(crate) agent: Option<String>,
    pub(crate) user: Option<String>,
    /// The users of the credentials it was issued (`GET /v1/storage/auth`).
    pub(crate) issued: Vec<String>,
    pub(crate) alpn: Option<String>,
    pub(crate) offered: Vec<String>,
    /// A short hash of the client's TLS hello: its suites, schemes and ALPN.
    pub(crate) fingerprint: String,
    pub(crate) opened: u64,
    pub(crate) closed: Option<u64>,
    pub(crate) sent: usize,
    pub(crate) received: usize,
    pub(crate) events: usize,
    pub(crate) failed: Option<String>,
    pub(crate) undecoded: Vec<String>,
}

/// Headers whose values are worth comparing between clients; of the rest, only names.
const NOTED: [&str; 11] = [
    "user-agent",
    "x-signal-agent",
    "accept-language",
    "x-signal-receive-stories",
    "content-type",
    "content-encoding",
    "accept-encoding",
    "grpc-encoding",
    "grpc-accept-encoding",
    "sec-websocket-extensions",
    "te",
];

/// Decodes the box record `record`'s `wire/raw/` into its `wire/`, and says what it found.
pub(crate) fn decode(record: &Path) -> Result<()> {
    let wire = record.join("wire");
    let raw = wire.join("raw");
    let mut metas: Vec<Connection> = Vec::new();
    for entry in std::fs::read_dir(&raw).context(raw.display().to_string())? {
        let path = entry.context(raw.display().to_string())?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let json = testbox_tools::read(&path)?;
            metas.push(serde_json::from_str(&json).with_context(|| path.display().to_string())?);
        }
    }
    metas.sort_by_key(|meta| meta.n);
    let mut events = Vec::new();
    let mut summaries = Vec::new();
    let mut identities = Vec::new();
    for meta in &metas {
        let (bytes_name, _) = testbox_tools::tap::file_names(meta.n, meta.port);
        let records = read_records(&raw.join(bytes_name))?;
        let (found, summary, identity) = connection(meta, &records);
        events.extend(found);
        summaries.push(summary);
        identities.push(identity);
    }
    attribute(&mut summaries, &identities, &mut events);
    events.sort_by_key(|event: &Event| event.at);
    let spans = spans(record)?;
    write_lines(&wire.join("events.jsonl"), &events)?;
    write_lines(&wire.join("connections.jsonl"), &summaries)?;
    let mut clients: BTreeMap<&str, Vec<&Event>> = BTreeMap::new();
    for event in &events {
        clients.entry(&event.client).or_default().push(event);
    }
    for (client, events) in &clients {
        testbox_tools::write(
            &wire.join(format!("transcript-{client}.txt")),
            transcript(events, &spans),
        )?;
    }
    testbox_tools::write(
        &wire.join("compare.md"),
        compare::report(&events, &summaries, &spans),
    )?;
    let undecoded = summaries
        .iter()
        .filter(|summary| !summary.undecoded.is_empty())
        .count();
    let counts: Vec<String> = clients
        .iter()
        .map(|(client, events)| format!("{client} {}", events.len()))
        .collect();
    println!(
        "wire: {} connections, {} events ({}), {undecoded} connections not wholly decoded: {}",
        summaries.len(),
        events.len(),
        counts.join(", "),
        wire.display()
    );
    Ok(())
}

fn read_records(path: &Path) -> Result<Vec<Record>> {
    // A connection that failed before relaying has no bytes.
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(Vec::new());
    };
    let mut input = std::io::BufReader::new(file);
    let mut records = Vec::new();
    while let Some(record) = Record::read_from(&mut input).context(path.display().to_string())? {
        records.push(record);
    }
    Ok(records)
}

/// Every part's checks, in the order they ran.
fn spans(record: &Path) -> Result<Vec<Span>> {
    let mut spans = Vec::new();
    for entry in std::fs::read_dir(record).context(record.display().to_string())? {
        let path = entry
            .context(record.display().to_string())?
            .path()
            .join("spans.jsonl");
        if !path.exists() {
            continue;
        }
        for line in testbox_tools::read(&path)?.lines() {
            spans.push(serde_json::from_str(line).with_context(|| path.display().to_string())?);
        }
    }
    spans.sort_by_key(|span: &Span| span.started);
    Ok(spans)
}

fn write_lines<T: Serialize>(path: &Path, items: &[T]) -> Result<()> {
    let mut out = String::new();
    for item in items {
        out.push_str(&serde_json::to_string(item).with_context(|| path.display().to_string())?);
        out.push('\n');
    }
    testbox_tools::write(path, out)
}

/// What the agent and the user can't tell, told by what else is known of a connection: a
/// storage credential's user by the clients that were issued it (an account's, every device's:
/// of those, the one that says the same agent), a TLS hello by the one client whose it is.
fn attribute(summaries: &mut [Summary], identities: &[Identities], events: &mut [Event]) {
    let vague = |client: &str| {
        matches!(client, "unknown" | "presage-rs" | "presage-cli") || client.starts_with("other")
    };
    let mut issued: BTreeMap<&str, std::collections::BTreeSet<String>> = BTreeMap::new();
    for (summary, identity) in summaries.iter().zip(identities) {
        if summary.client != "unknown" {
            for user in &identity.issued {
                issued
                    .entry(user)
                    .or_default()
                    .insert(summary.client.clone());
            }
        }
    }
    let mut hellos: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    for summary in summaries.iter().filter(|summary| !vague(&summary.client)) {
        hellos
            .entry(summary.fingerprint.clone())
            .or_default()
            .insert(summary.client.clone());
    }
    let mut renamed: BTreeMap<u64, String> = BTreeMap::new();
    for (summary, identity) in summaries.iter_mut().zip(identities) {
        if !vague(&summary.client) {
            continue;
        }
        // libsignal-service's two say the same agent.
        let presage = identity
            .agent
            .as_deref()
            .is_some_and(|agent| agent.starts_with("presage"));
        // Only the storage service takes those credentials; their user is the account's ACI,
        // which a primary's own login is too.
        let by_credential = identity
            .user
            .as_deref()
            .filter(|_| summary.port == testbox_tools::keys::STORAGE_PORT)
            .and_then(|user| issued.get(user))
            .map(|clients| {
                clients
                    .iter()
                    .filter(|client| !presage || matches!(client.as_str(), "b33p" | "presage-cli"))
                    .collect::<Vec<_>>()
            })
            .filter(|clients| clients.len() == 1)
            .map(|clients| clients[0].clone());
        let by_hello = (summary.client == "unknown")
            .then(|| hellos.get(&summary.fingerprint))
            .flatten()
            .filter(|clients| clients.len() == 1)
            .and_then(|clients| clients.first().cloned());
        if let Some(client) = by_credential.or(by_hello) {
            summary.client.clone_from(&client);
            renamed.insert(summary.n, client);
        }
    }
    for event in events {
        if let Some(client) = renamed.get(&event.connection) {
            event.client.clone_from(client);
        }
    }
}

/// One connection's events, its summary, and what it said of who made it.
fn connection(meta: &Connection, records: &[Record]) -> (Vec<Event>, Summary, Identities) {
    let (sent, received) = Stream::sides(records);
    let mut undecoded = Vec::new();
    let mut found = Vec::new();
    let mut identities = Identities::default();
    if !sent.bytes.is_empty() {
        if meta.alpn.as_deref() == Some("h2") {
            http2(
                meta,
                &sent,
                &received,
                &mut found,
                &mut identities,
                &mut undecoded,
            );
        } else {
            http1(
                meta,
                &sent,
                &received,
                &mut found,
                &mut identities,
                &mut undecoded,
            );
        }
    }
    let client = identities.client();
    for event in &mut found {
        if let Some(user) = issued_user(event) {
            identities.issued.push(user);
        }
        event.client.clone_from(&client);
    }
    let summary = Summary {
        n: meta.n,
        port: meta.port,
        client,
        agent: identities.agent.clone(),
        user: identities.user.clone(),
        issued: identities.issued.clone(),
        alpn: meta.alpn.clone(),
        offered: meta.hello.alpn.clone(),
        fingerprint: fingerprint(meta),
        opened: meta.opened,
        closed: sent.closed.max(received.closed),
        sent: sent.bytes.len(),
        received: received.bytes.len(),
        events: found.len(),
        failed: meta.failed.clone(),
        undecoded,
    };
    (found, summary, identities)
}

/// The user of a credential the event's answer issued (`GET /v1/storage/auth` and its like),
/// which [`chat`] noted as `issued=<user>`.
fn issued_user(event: &mut Event) -> Option<String> {
    let index = event
        .noted
        .iter()
        .position(|noted| noted.starts_with("issued="))?;
    let noted = event.noted.remove(index);
    noted.strip_prefix("issued=").map(str::to_owned)
}

/// What a connection said of who made it.
#[derive(Default)]
struct Identities {
    agent: Option<String>,
    /// The Basic auth or `login` user: `<aci>` or `<aci>.<device>`.
    user: Option<String>,
    /// The users of credentials it was issued: a storage service's.
    issued: Vec<String>,
}

impl Identities {
    fn note(&mut self, headers: &Headers, path: &str) {
        if self.agent.is_none() {
            self.agent = header(headers, "user-agent")
                .or_else(|| header(headers, "x-signal-agent"))
                .map(str::to_owned);
        }
        if self.user.is_none() {
            self.user = header(headers, "authorization")
                .and_then(basic_user)
                .or_else(|| query_value(path, "login"));
        }
    }

    /// `desktop`, `android`, `b33p` (by its agent, `b33p/<version>`; before it had one, a linked
    /// device of libsignal-service's) or `presage-cli` (a primary's); `presage-rs`
    /// unauthenticated, when b33p's said the same agent as presage-cli's.
    fn client(&self) -> String {
        let agent = self.agent.as_deref().unwrap_or_default();
        if agent.contains("Signal-Desktop") {
            "desktop".to_owned()
        } else if agent.contains("Signal-Android") {
            "android".to_owned()
        } else if agent.starts_with("b33p/") || agent == "b33p" {
            // `b33p` alone: b33p's own downloads (b33p-core transfer.rs) before it had an agent.
            "b33p".to_owned()
        } else if agent.starts_with("presage") {
            match self.user.as_deref().and_then(|user| user.rsplit_once('.')) {
                Some((_, device)) if device != "1" => "b33p".to_owned(),
                Some(_) => "presage-cli".to_owned(),
                None if self.user.is_some() => "presage-cli".to_owned(),
                None => "presage-rs".to_owned(),
            }
        } else if agent == "undici" {
            // wrangler's dev server, in front of CDN3, asking its own worker (`/cdn-cgi/`).
            "cdn3-itself".to_owned()
        } else if agent.is_empty() {
            "unknown".to_owned()
        } else {
            format!("other ({agent})")
        }
    }
}

fn basic_user(authorization: &str) -> Option<String> {
    use base64::Engine as _;
    let encoded = authorization.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    Some(decoded.split_once(':')?.0.to_owned())
}

fn query_value(path: &str, name: &str) -> Option<String> {
    let (_, query) = path.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (found, value) = pair.split_once('=')?;
        (found == name).then(|| value.replace("%2E", ".").replace("%2e", "."))
    })
}

fn http1(
    meta: &Connection,
    sent: &Stream,
    received: &Stream,
    found: &mut Vec<Event>,
    identities: &mut Identities,
    undecoded: &mut Vec<String>,
) {
    let connection = h1::parse(sent, received);
    for exchange in &connection.exchanges {
        identities.note(&exchange.headers, &exchange.path);
        let response = exchange.response.as_ref();
        if let Some(user) =
            response.and_then(|response| issued(&exchange.path, Some(&response.body)))
        {
            identities.issued.push(user);
        }
        found.push(event(
            meta,
            "h1",
            false,
            exchange.at,
            &exchange.method,
            &exchange.path,
            response.map(|response| u32::from(response.status)),
            response.map(|response| response.at),
            &exchange.headers,
            body(&exchange.body, header(&exchange.headers, "content-type")),
            response.map_or_else(Body::default, |response| {
                body(&response.body, header(&response.headers, "content-type"))
            }),
        ));
    }
    undecoded.extend(connection.left);
    if let Some(upgrade) = connection.upgrade {
        // From the handshake's end on, each part with when it came.
        let client = reslice(sent, upgrade.sent);
        let server = reslice(received, upgrade.received);
        chat(
            meta,
            &client,
            &server,
            upgrade.extensions.as_deref(),
            found,
            undecoded,
        );
    }
}

/// `stream` from `start`, with its times.
fn reslice(stream: &Stream, start: usize) -> Stream {
    let mut sliced = Stream::default();
    let mut offset = start;
    while offset < stream.bytes.len() {
        let at = stream.at(offset);
        let mut end = offset + 1;
        while end < stream.bytes.len() && stream.at(end) == at {
            end += 1;
        }
        sliced.push(at, &stream.bytes[offset..end]);
        offset = end;
    }
    sliced.closed = stream.closed;
    sliced
}

fn http2(
    meta: &Connection,
    sent: &Stream,
    received: &Stream,
    found: &mut Vec<Event>,
    identities: &mut Identities,
    undecoded: &mut Vec<String>,
) {
    let connection = h2::parse(sent, received);
    undecoded.extend(connection.left);
    for control in &connection.controls {
        found.push(Event {
            from_server: !control.from_client,
            ..event(
                meta,
                "h2-control",
                false,
                control.at,
                &control.what,
                "",
                None,
                None,
                &Headers::new(),
                Body::default(),
                Body::default(),
            )
        });
    }
    for stream in &connection.streams {
        let request = &stream.client.headers;
        let path = header(request, ":path").unwrap_or_default();
        identities.note(request, path);
        let method = header(request, ":method").unwrap_or_default();
        let at = stream.client.at.unwrap_or(meta.opened);
        let response_at = stream.server.at;
        let content_type = |headers: &Headers| header(headers, "content-type").map(str::to_owned);
        if stream.websocket() {
            found.push(event(
                meta,
                "h2",
                false,
                at,
                method,
                path,
                http_status(&stream.server.headers),
                response_at,
                request,
                Body::default(),
                Body::default(),
            ));
            let extensions = header(&stream.server.headers, "sec-websocket-extensions");
            chat(
                meta,
                &stream.client.data,
                &stream.server.data,
                extensions,
                found,
                undecoded,
            );
        } else if stream.grpc() {
            let (requests, left_request) = h2::grpc_messages(&stream.client.data.bytes);
            undecoded.extend(left_request);
            // A server that doesn't serve gRPC there answers HTTP: its status, its page.
            let answered_grpc = content_type(&stream.server.headers)
                .is_some_and(|kind| kind.starts_with("application/grpc"));
            let response = if answered_grpc {
                let (responses, left_response) = h2::grpc_messages(&stream.server.data.bytes);
                undecoded.extend(left_response);
                grpc_body(&responses)
            } else {
                body(
                    &stream.server.data.bytes,
                    content_type(&stream.server.headers).as_deref(),
                )
            };
            let status = header(&stream.server.trailers, "grpc-status")
                .or_else(|| header(&stream.server.headers, "grpc-status"))
                .and_then(|status| status.parse().ok())
                .or_else(|| http_status(&stream.server.headers));
            found.push(event(
                meta,
                "grpc",
                false,
                at,
                method,
                path,
                status,
                response_at,
                request,
                grpc_body(&requests),
                response,
            ));
        } else {
            found.push(event(
                meta,
                "h2",
                false,
                at,
                method,
                path,
                http_status(&stream.server.headers),
                response_at,
                request,
                body(&stream.client.data.bytes, content_type(request).as_deref()),
                body(
                    &stream.server.data.bytes,
                    content_type(&stream.server.headers).as_deref(),
                ),
            ));
        }
    }
}

fn http_status(headers: &Headers) -> Option<u32> {
    header(headers, ":status").and_then(|status| status.parse().ok())
}

/// A WebSocket's chat messages: each request with its response, the client's and the server's.
fn chat(
    meta: &Connection,
    client: &Stream,
    server: &Stream,
    extensions: Option<&str>,
    found: &mut Vec<Event>,
    undecoded: &mut Vec<String>,
) {
    let (from_client, left) = ws::messages(client, 0, ws::Deflate::agreed(extensions, true));
    undecoded.extend(left.map(|left| format!("client's websocket: {left}")));
    let (from_server, left) = ws::messages(server, 0, ws::Deflate::agreed(extensions, false));
    undecoded.extend(left.map(|left| format!("server's websocket: {left}")));
    // Responses by the side that sent them and their request's id.
    let mut responses: BTreeMap<(bool, u64), Answer> = BTreeMap::new();
    let mut requests = Vec::new();
    for (by_client, messages) in [(true, &from_client), (false, &from_server)] {
        for message in messages {
            match message.opcode {
                ws::Opcode::Binary => match ChatMessage::parse(&message.payload) {
                    Some(ChatMessage::Request {
                        id,
                        verb,
                        path,
                        headers,
                        body,
                    }) => requests.push((by_client, message.at, id, verb, path, headers, body)),
                    Some(ChatMessage::Response {
                        id: Some(id),
                        status,
                        headers,
                        body,
                        ..
                    }) => {
                        responses.insert(
                            (by_client, id),
                            Answer {
                                at: message.at,
                                status,
                                headers,
                                body,
                            },
                        );
                    }
                    Some(ChatMessage::Response { id: None, .. }) | None => undecoded.push(format!(
                        "a {} byte websocket message that isn't Signal's",
                        message.payload.len()
                    )),
                },
                opcode => found.push(Event {
                    from_server: !by_client,
                    request: Body {
                        size: message.wire_size,
                        kind: "frame",
                        shape: Vec::new(),
                    },
                    ..event(
                        meta,
                        "ws-control",
                        false,
                        message.at,
                        &format!("{opcode:?}").to_uppercase(),
                        "",
                        None,
                        None,
                        &Headers::new(),
                        Body::default(),
                        Body::default(),
                    )
                }),
            }
        }
    }
    for (by_client, at, id, verb, path, headers, request_body) in requests {
        let headers = signal_headers(&headers);
        let response = id.and_then(|id| responses.get(&(!by_client, id)));
        let request_type = header(&headers, "content-type").map(str::to_owned);
        let request_body = request_body.unwrap_or_default();
        let issued = response
            .filter(|_| by_client)
            .and_then(|response| issued(&path, response.body.as_deref()))
            .map(|user| format!("issued={user}"));
        // The server's deliveries: an envelope each.
        let delivered = (!by_client && path == "/api/v1/message")
            .then(|| proto::envelope_shape(&request_body))
            .flatten()
            .map(|shape| Body {
                size: request_body.len(),
                kind: "envelope",
                shape,
            });
        let mut chat_event = event(
            meta,
            "ws",
            !by_client,
            at,
            &verb,
            &path,
            response.and_then(|response| response.status),
            response.map(|response| response.at),
            &headers,
            delivered.unwrap_or_else(|| body(&request_body, request_type.as_deref())),
            response.map_or_else(Body::default, |response| {
                let headers = signal_headers(&response.headers);
                body(
                    response.body.as_deref().unwrap_or_default(),
                    header(&headers, "content-type"),
                )
            }),
        );
        chat_event.noted.extend(issued);
        found.push(chat_event);
    }
}

/// A credential an answer issues: `GET /v1/storage/auth`'s and its like.
#[derive(serde::Deserialize)]
struct Credential {
    username: String,
}

/// The user of the credential the answer `body` to `path` issued, if it issued one.
fn issued(path: &str, body: Option<&[u8]>) -> Option<String> {
    let path = path.split('?').next().unwrap_or_default();
    if !path.ends_with("/auth") {
        return None;
    }
    serde_json::from_slice::<Credential>(body?)
        .ok()
        .map(|credential| credential.username)
}

/// A chat response, waiting for its request.
struct Answer {
    at: u64,
    status: Option<u32>,
    headers: Vec<String>,
    body: Option<Vec<u8>>,
}

/// A chat message's headers, `name:value` each, as HTTP's.
fn signal_headers(headers: &[String]) -> Headers {
    headers
        .iter()
        .map(|header| match header.split_once(':') {
            Some((name, value)) => (name.trim().to_ascii_lowercase(), value.trim().to_owned()),
            None => (header.trim().to_ascii_lowercase(), String::new()),
        })
        .collect()
}

#[expect(clippy::too_many_arguments, reason = "an event's fields, each its own")]
fn event(
    meta: &Connection,
    transport: &'static str,
    from_server: bool,
    at: u64,
    verb: &str,
    path: &str,
    status: Option<u32>,
    answered: Option<u64>,
    headers: &Headers,
    request: Body,
    response: Body,
) -> Event {
    let mut names: Vec<String> = headers
        .iter()
        .map(|(name, _)| name.clone())
        .filter(|name| !name.starts_with(':'))
        .collect();
    names.sort();
    names.dedup();
    let noted = headers
        .iter()
        .filter(|(name, _)| NOTED.contains(&name.as_str()) || name == ":protocol")
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    Event {
        at,
        connection: meta.n,
        port: meta.port,
        client: String::new(),
        transport,
        from_server,
        verb: verb.to_owned(),
        path: path.to_owned(),
        template: template::template(path),
        status,
        took_ms: answered
            .filter(|&answered| answered >= at)
            .map(|answered| (answered - at) as f64 / 1e6),
        headers: names,
        noted,
        request,
        response,
    }
}

/// What a body is: JSON by its type or its look, protobuf where it parses as one, else text or
/// bytes.
fn body(bytes: &[u8], content_type: Option<&str>) -> Body {
    if bytes.is_empty() {
        return Body {
            size: 0,
            kind: "empty",
            shape: Vec::new(),
        };
    }
    let size = bytes.len();
    let looks_json = matches!(bytes.first(), Some(b'{' | b'['));
    if (content_type.is_some_and(|kind| kind.contains("json")) || looks_json)
        && let Some(shape) = json_shape(bytes)
    {
        return Body {
            size,
            kind: "json",
            shape,
        };
    }
    let says_protobuf = content_type.is_some_and(|kind| kind.contains("protobuf"));
    if let Some(fields) = proto::parse(bytes).filter(|fields| says_protobuf || !fields.is_empty())
        && (says_protobuf || std::str::from_utf8(bytes).is_err())
    {
        return Body {
            size,
            kind: "protobuf",
            shape: proto::shape(&fields),
        };
    }
    Body {
        size,
        kind: if std::str::from_utf8(bytes).is_ok() {
            "text"
        } else {
            "binary"
        },
        shape: Vec::new(),
    }
}

fn grpc_body(messages: &[(bool, Vec<u8>)]) -> Body {
    let size = messages.iter().map(|(_, message)| message.len()).sum();
    let mut shape: Vec<String> = messages
        .iter()
        .flat_map(|(compressed, message)| {
            if *compressed {
                vec!["(compressed)".to_owned()]
            } else {
                proto::parse(message).map_or_else(
                    || vec!["(not protobuf)".to_owned()],
                    |fields| proto::shape(&fields),
                )
            }
        })
        .collect();
    shape.sort();
    shape.dedup();
    Body {
        size,
        kind: if messages.is_empty() {
            "empty"
        } else {
            "protobuf"
        },
        shape,
    }
}

/// A JSON body's key paths with their values' types: `messages[].type:number`.
#[expect(
    clippy::disallowed_types,
    reason = "the bodies of a protocol the box observes, not one of its own: their shape is the point"
)]
fn json_shape(bytes: &[u8]) -> Option<Vec<String>> {
    fn walk(value: &serde_json::Value, path: &str, paths: &mut Vec<String>) {
        use serde_json::Value;
        let kind = match value {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(items) => {
                for item in items {
                    walk(item, &format!("{path}[]"), paths);
                }
                "array"
            }
            Value::Object(fields) => {
                for (name, field) in fields {
                    let inner = if path.is_empty() {
                        name.clone()
                    } else {
                        format!("{path}.{name}")
                    };
                    walk(field, &inner, paths);
                }
                "object"
            }
        };
        if !path.is_empty() {
            paths.push(format!("{path}:{kind}"));
        }
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let mut paths = Vec::new();
    walk(&value, "", &mut paths);
    paths.sort();
    paths.dedup();
    Some(paths)
}

/// The client's hello in eight hex digits (FNV-1a): the same TLS stack, the same digits.
fn fingerprint(meta: &Connection) -> String {
    let hello = &meta.hello;
    let text = format!(
        "{}|{}|{}",
        hello.cipher_suites.join(","),
        hello.signature_schemes.join(","),
        hello.alpn.join(",")
    );
    let hash = text.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    format!("{hash:08x}")
}

/// A client's events, one a line, under the check they happened in.
fn transcript(events: &[&Event], spans: &[Span]) -> String {
    let mut out = String::new();
    let mut current: Option<&str> = None;
    for event in events {
        let span = spans.iter().find(|span| {
            span.started <= u128::from(event.at) && u128::from(event.at) <= span.ended
        });
        let check = span.map(|span| span.check.as_str());
        if check != current {
            out.push_str(&format!("\n== {}\n", check.unwrap_or("(between checks)")));
            current = check;
        }
        let when = clock(event.at);
        let arrow = if event.from_server { "<=" } else { "=>" };
        let status = event
            .status
            .map_or("-".to_owned(), |status| status.to_string());
        let took = event
            .took_ms
            .map_or(String::new(), |took| format!(" {took:.0} ms"));
        out.push_str(&format!(
            "{when} #{} {} {} {arrow} {} {} -> {status}{took}\n",
            event.connection, event.port, event.transport, event.verb, event.path
        ));
        for (what, body) in [("req", &event.request), ("resp", &event.response)] {
            if body.kind != "empty" && !body.kind.is_empty() {
                out.push_str(&format!(
                    "      {what} {} {} B: {}\n",
                    body.kind,
                    body.size,
                    body.shape.join(" ")
                ));
            }
        }
        if !event.noted.is_empty() {
            out.push_str(&format!("      {}\n", event.noted.join("  ")));
        }
    }
    out
}

/// A time of day, UTC, to the millisecond.
pub(crate) fn clock(at: u64) -> String {
    let millis = at / 1_000_000;
    let day = millis % 86_400_000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        day / 3_600_000,
        day / 60_000 % 60,
        day / 1000 % 60,
        day % 1000
    )
}
