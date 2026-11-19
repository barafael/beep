//! `compare.md`: the clients side by side. Their connections and TLS; which requests each made
//! and how often; how they keep their chat connection alive; where the same request differs in
//! its headers or its body's shape; what each moved through the CDNs; and, check by check, what
//! each did.

use std::collections::{BTreeMap, BTreeSet};

use super::{Event, Summary, clock};
use crate::checks::record::Span;
use testbox_tools::keys;

/// The transports of requests, as against control frames.
const REQUESTS: [&str; 4] = ["h1", "h2", "ws", "grpc"];

pub(crate) fn report(events: &[Event], summaries: &[Summary], spans: &[Span]) -> String {
    let clients: BTreeSet<&str> = summaries
        .iter()
        .map(|summary| summary.client.as_str())
        .collect();
    let mut out = String::from("# What the clients said to the servers\n\n");
    out.push_str(
        "From `--capture`'s tap. `=>` a client's request, `<=` the server's (the chat's \
         deliveries). Paths as templates (xtask/src/wire/template.rs).\n",
    );
    connections(&mut out, summaries, &clients);
    requests(&mut out, events, &clients);
    keepalive(&mut out, events, &clients);
    differences(&mut out, events);
    attachments(&mut out, events, spans);
    checks(&mut out, events, spans);
    out
}

fn connections(out: &mut String, summaries: &[Summary], clients: &BTreeSet<&str>) {
    out.push_str("\n## Connections\n\n");
    out.push_str(
        "| client | connections | ports | ALPN | TLS hellos | longest (s) | not decoded |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|\n");
    // Connections that carried no byte have no client and no hello to tell: only how many.
    let empty = |summary: &Summary| summary.sent == 0 && summary.received == 0;
    for client in clients {
        let own: Vec<&Summary> = summaries
            .iter()
            .filter(|summary| summary.client == *client && !empty(summary))
            .collect();
        if own.is_empty() {
            continue;
        }
        let ports: BTreeSet<u16> = own.iter().map(|summary| summary.port).collect();
        let alpn: BTreeSet<String> = own
            .iter()
            .map(|summary| summary.alpn.clone().unwrap_or_else(|| "-".to_owned()))
            .collect();
        let hellos: BTreeSet<&str> = own
            .iter()
            .map(|summary| summary.fingerprint.as_str())
            .collect();
        let longest = own
            .iter()
            .filter_map(|summary| Some(summary.closed?.saturating_sub(summary.opened)))
            .max()
            .unwrap_or(0);
        let undecoded = own
            .iter()
            .filter(|summary| !summary.undecoded.is_empty())
            .count();
        out.push_str(&format!(
            "| {client} | {} | {} | {} | {} | {:.0} | {undecoded} |\n",
            own.len(),
            join(&ports),
            join(&alpn),
            join(&hellos),
            longest as f64 / 1e9,
        ));
    }
    let empties = summaries.iter().filter(|summary| empty(summary)).count();
    if empties > 0 {
        out.push_str(&format!(
            "\n{empties} more connections carried no byte either way.\n"
        ));
    }
    let problems: Vec<String> = summaries
        .iter()
        .filter(|summary| !empty(summary))
        .filter(|summary| !summary.undecoded.is_empty() || summary.failed.is_some())
        .map(|summary| {
            format!(
                "- #{} {} ({}): {}",
                summary.n,
                summary.port,
                summary.client,
                summary
                    .failed
                    .iter()
                    .chain(&summary.undecoded)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })
        .collect();
    if !problems.is_empty() {
        out.push_str("\nNot wholly decoded, or not relayed:\n\n");
        out.push_str(&problems.join("\n"));
        out.push('\n');
    }
}

/// `verb template` of a request, with `<=` for the server's.
fn request_key(event: &Event) -> String {
    let arrow = if event.from_server { "<= " } else { "" };
    format!("{arrow}{} {}", event.verb, event.template)
}

fn requests(out: &mut String, events: &[Event], clients: &BTreeSet<&str>) {
    // By request, by client: how many, on which ports.
    let mut counts: BTreeMap<String, BTreeMap<&str, (usize, BTreeSet<u16>)>> = BTreeMap::new();
    for event in events
        .iter()
        .filter(|event| REQUESTS.contains(&event.transport))
    {
        let (count, ports) = counts
            .entry(request_key(event))
            .or_default()
            .entry(&event.client)
            .or_default();
        *count += 1;
        ports.insert(event.port);
    }
    out.push_str("\n## Requests\n\nHow many of each request each client made, and the port ");
    out.push_str("where it isn't Signal-Server's own. **Bold**: only one client made it.\n\n");
    out.push_str(&format!(
        "| request | {} |\n|---|{}\n",
        join(clients).replace(", ", " | "),
        "---|".repeat(clients.len())
    ));
    let server = [
        keys::LIBSIGNAL_NET_PORT,
        keys::LIBSIGNAL_SERVICE_PORT,
        keys::OMNIBUS_PORT,
    ];
    for (key, by_client) in &counts {
        let alone = by_client.len() == 1 && clients.len() > 1;
        let key = if alone {
            format!("**`{key}`**")
        } else {
            format!("`{key}`")
        };
        let cells: Vec<String> = clients
            .iter()
            .map(|client| {
                by_client
                    .get(client)
                    .map_or(String::new(), |(count, ports)| {
                        if ports.iter().all(|port| server.contains(port)) {
                            count.to_string()
                        } else {
                            format!("{count} ({})", join(ports))
                        }
                    })
            })
            .collect();
        out.push_str(&format!("| {key} | {} |\n", cells.join(" | ")));
    }
}

fn keepalive(out: &mut String, events: &[Event], clients: &BTreeSet<&str>) {
    out.push_str("\n## Keeping the chat alive\n\n");
    out.push_str("| client | GET /v1/keepalive | its interval (s, median) | WebSocket pings | HTTP/2 pings |\n");
    out.push_str("|---|---|---|---|---|\n");
    for client in clients {
        let own = || events.iter().filter(|event| event.client == *client);
        let mut keepalives: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
        for event in own().filter(|event| event.template.starts_with("/v1/keepalive")) {
            keepalives
                .entry(event.connection)
                .or_default()
                .push(event.at);
        }
        let mut gaps: Vec<u64> = keepalives
            .values()
            .flat_map(|ats| ats.windows(2).map(|pair| pair[1] - pair[0]))
            .collect();
        gaps.sort_unstable();
        let median = gaps
            .get(gaps.len() / 2)
            .map_or("-".to_owned(), |gap| format!("{:.1}", *gap as f64 / 1e9));
        let ws_pings = own()
            .filter(|event| event.transport == "ws-control" && event.verb == "PING")
            .count();
        let h2_pings = own()
            .filter(|event| event.transport == "h2-control" && event.verb == "PING")
            .count();
        out.push_str(&format!(
            "| {client} | {} | {median} | {ws_pings} | {h2_pings} |\n",
            keepalives.values().map(Vec::len).sum::<usize>()
        ));
    }
    let settings: BTreeSet<(String, String)> = events
        .iter()
        .filter(|event| event.transport == "h2-control" && event.verb.starts_with("SETTINGS "))
        .map(|event| {
            let side = if event.from_server {
                "server"
            } else {
                "client"
            };
            (format!("{} ({side})", event.client), event.verb.clone())
        })
        .collect();
    if !settings.is_empty() {
        out.push_str("\nHTTP/2 settings:\n\n");
        for (who, what) in settings {
            out.push_str(&format!("- {who}: `{what}`\n"));
        }
    }
}

/// Where two clients made the same request differently: its headers' names, the values worth
/// comparing, its body's and its answer's shapes.
fn differences(out: &mut String, events: &[Event]) {
    // By request, by client: what each aspect was, every way it was.
    type Seen<'a> = BTreeMap<&'a str, BTreeMap<String, BTreeSet<String>>>;
    let mut by_request: BTreeMap<String, Seen<'_>> = BTreeMap::new();
    for event in events
        .iter()
        .filter(|event| REQUESTS.contains(&event.transport))
    {
        let aspects = by_request
            .entry(request_key(event))
            .or_default()
            .entry(&event.client)
            .or_default();
        aspects
            .entry("headers".to_owned())
            .or_default()
            .insert(event.headers.join(" "));
        for noted in &event.noted {
            let (name, value) = noted.split_once('=').unwrap_or((noted, ""));
            aspects
                .entry(name.to_owned())
                .or_default()
                .insert(value.to_owned());
        }
        aspects
            .entry("body".to_owned())
            .or_default()
            .insert(format!(
                "{} {}",
                event.request.kind,
                event.request.shape.join(" ")
            ));
        aspects
            .entry("answer".to_owned())
            .or_default()
            .insert(format!(
                "{} {} {}",
                event
                    .status
                    .map_or("-".to_owned(), |status| status.to_string()),
                event.response.kind,
                event.response.shape.join(" ")
            ));
    }
    out.push_str("\n## The same request, made differently\n\n");
    let mut any = false;
    for (request, by_client) in &by_request {
        if by_client.len() < 2 {
            continue;
        }
        let aspects: BTreeSet<&String> = by_client.values().flat_map(BTreeMap::keys).collect();
        let mut lines = Vec::new();
        for aspect in aspects {
            let ways: BTreeMap<&str, &BTreeSet<String>> = by_client
                .iter()
                .filter_map(|(client, seen)| Some((*client, seen.get(aspect)?)))
                .collect();
            let distinct: BTreeSet<&BTreeSet<String>> = ways.values().copied().collect();
            if distinct.len() < 2 && ways.len() == by_client.len() {
                continue;
            }
            lines.push(format!("  - {aspect}:"));
            for client in by_client.keys() {
                let seen = ways.get(client);
                let shown = seen.map_or("(none)".to_owned(), |seen| {
                    seen.iter()
                        .map(|way| {
                            if way.trim().is_empty() {
                                "(empty)".to_owned()
                            } else {
                                format!("`{}`", way.trim())
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" / ")
                });
                lines.push(format!("    - {client}: {shown}"));
            }
        }
        if !lines.is_empty() {
            any = true;
            out.push_str(&format!("- `{request}`\n{}\n", lines.join("\n")));
        }
    }
    if !any {
        out.push_str("None: every request two clients made, they made alike.\n");
    }
}

/// What a client moved through the CDNs in one check.
#[derive(Default)]
struct Moved {
    downloads: usize,
    downloaded: usize,
    /// Downloads of an attachment the client had downloaded before, in this check or earlier.
    again: usize,
    uploads: usize,
    uploaded: usize,
}

/// An attachment's download from a CDN: `GET /attachments/{id}`.
fn is_download(event: &Event) -> bool {
    event.verb == "GET" && event.path.starts_with("/attachments/")
}

/// The bytes of an upload to a CDN: a tus POST that carries them (creation with upload), a tus
/// PATCH, or CDN2's PUT; not the requests that only ask where to upload or how far it got.
fn is_upload(event: &Event) -> bool {
    matches!(event.verb.as_str(), "POST" | "PATCH" | "PUT")
        && event.path.starts_with("/upload/")
        && event.request.size > 0
}

/// Check by check, what each client downloaded from and uploaded to the CDNs: how often, how
/// many bytes (as encrypted and padded), and how many downloads were of an attachment it had
/// already downloaded. A request whose body didn't decode counts with the bytes that did.
fn attachments(out: &mut String, events: &[Event], spans: &[Span]) {
    out.push_str("\n## Attachments\n\n");
    out.push_str(
        "Downloads (`GET /attachments/{id}`) and uploads (the requests carrying the bytes) per \
         check; *again*: downloads of an attachment the client had downloaded before.\n\n",
    );
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut rows = String::new();
    let mut totals: BTreeMap<&str, Moved> = BTreeMap::new();
    let add = |moved: &mut Moved, event: &Event, again: bool| {
        if is_download(event) {
            moved.downloads += 1;
            moved.downloaded += event.response.size;
            moved.again += usize::from(again);
        } else {
            moved.uploads += 1;
            moved.uploaded += event.request.size;
        }
    };
    let row = |rows: &mut String, check: &str, client: &str, moved: &Moved| {
        rows.push_str(&format!(
            "| {check} | {client} | {} | {} | {} | {} | {} |\n",
            moved.downloads,
            bytes(moved.downloaded),
            moved.again,
            moved.uploads,
            bytes(moved.uploaded),
        ));
    };
    let moving = |event: &&Event| {
        REQUESTS.contains(&event.transport) && (is_download(event) || is_upload(event))
    };
    let mut counted = 0;
    for span in spans {
        let mut by_client: BTreeMap<&str, Moved> = BTreeMap::new();
        for event in events.iter().filter(moving).filter(|event| {
            span.started <= u128::from(event.at) && u128::from(event.at) <= span.ended
        }) {
            let again = is_download(event) && !seen.insert((&event.client, &event.path));
            add(by_client.entry(&event.client).or_default(), event, again);
            add(totals.entry(&event.client).or_default(), event, again);
            counted += 1;
        }
        for (client, moved) in &by_client {
            row(&mut rows, &span.check, client, moved);
        }
    }
    let all = events.iter().filter(moving).count();
    if all == 0 {
        out.push_str("None.\n");
        return;
    }
    out.push_str("| check | client | downloads | bytes | again | uploads | bytes |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    for (client, moved) in &totals {
        row(&mut rows, "**all checks**", client, moved);
    }
    out.push_str(&rows);
    if counted < all {
        out.push_str(&format!(
            "\n{} more between checks (setting up, or after a check ended).\n",
            all - counted
        ));
    }
}

/// Check by check, each client's requests in order, a run of the same one counted once.
fn checks(out: &mut String, events: &[Event], spans: &[Span]) {
    out.push_str("\n## Check by check\n");
    for span in spans {
        let during: Vec<&Event> = events
            .iter()
            .filter(|event| {
                REQUESTS.contains(&event.transport)
                    && span.started <= u128::from(event.at)
                    && u128::from(event.at) <= span.ended
            })
            .collect();
        if during.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "\n### {} ({}, {:.1} s)\n\n",
            span.check,
            if span.passed { "passed" } else { "failed" },
            (span.ended - span.started) as f64 / 1e9
        ));
        let mut by_client: BTreeMap<&str, Vec<(String, usize, u64)>> = BTreeMap::new();
        for event in during {
            let sequence = by_client.entry(&event.client).or_default();
            let key = format!(
                "{}{}",
                request_key(event),
                event
                    .status
                    .map_or(String::new(), |status| format!(" {status}"))
            );
            match sequence.last_mut() {
                Some((last, count, _)) if *last == key => *count += 1,
                _ => sequence.push((key, 1, event.at)),
            }
        }
        for (client, sequence) in by_client {
            out.push_str(&format!("- {client}:\n"));
            for (key, count, at) in sequence {
                let times = if count > 1 {
                    format!(" ×{count}")
                } else {
                    String::new()
                };
                out.push_str(&format!("  - {} `{key}`{times}\n", clock(at)));
            }
        }
    }
}

/// A byte count as people read it: `8.1 MiB`, `152 KiB`, `592 B`.
fn bytes(n: usize) -> String {
    const KIB: f64 = 1024.0;
    let n = n as f64;
    if n >= KIB * KIB {
        format!("{:.1} MiB", n / (KIB * KIB))
    } else if n >= KIB {
        format!("{:.0} KiB", n / KIB)
    } else {
        format!("{n} B")
    }
}

fn join<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    items
        .into_iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Body;

    fn event(at: u64, client: &str, verb: &str, path: &str, sent: usize, got: usize) -> Event {
        let body = |size| Body {
            size,
            kind: "binary",
            shape: Vec::new(),
        };
        Event {
            at,
            connection: 0,
            port: keys::CDN3_PORT,
            client: client.to_owned(),
            transport: "h1",
            from_server: false,
            verb: verb.to_owned(),
            path: path.to_owned(),
            template: String::new(),
            status: Some(200),
            took_ms: None,
            headers: Vec::new(),
            noted: Vec::new(),
            request: body(sent),
            response: body(got),
        }
    }

    fn span(check: &str, started: u128, ended: u128) -> Span {
        Span {
            check: check.to_owned(),
            started,
            ended,
            passed: true,
        }
    }

    /// The media phase of 2026-11-19, cut down: b33p uploads a photo whole (its tus POST
    /// carries nothing, its PATCH the bytes), downloads it again itself, and again after a
    /// restart; Desktop's POST carries its scaled copy.
    #[test]
    fn attachments_count_downloads_again_and_uploads_with_their_bytes() {
        let events = [
            event(10, "desktop", "POST", "/upload/attachments", 155_424, 20),
            event(11, "b33p", "GET", "/attachments/a", 0, 155_424),
            event(20, "b33p", "POST", "/upload/attachments", 0, 0),
            event(21, "b33p", "HEAD", "/upload/attachments/b", 0, 0),
            event(22, "b33p", "PATCH", "/upload/attachments/b", 8_489_920, 0),
            event(23, "b33p", "GET", "/attachments/b", 0, 8_489_920),
            event(30, "b33p", "GET", "/attachments/a", 0, 155_424),
            event(31, "b33p", "GET", "/attachments/b", 0, 8_489_920),
            event(40, "b33p", "GET", "/attachments/c", 0, 1),
        ];
        let spans = [
            span("from Desktop", 5, 15),
            span("from b33p", 19, 25),
            span("restart", 29, 35),
        ];
        let mut out = String::new();
        attachments(&mut out, &events, &spans);
        for line in [
            "| from Desktop | b33p | 1 | 152 KiB | 0 | 0 | 0 B |",
            "| from Desktop | desktop | 0 | 0 B | 0 | 1 | 152 KiB |",
            "| from b33p | b33p | 1 | 8.1 MiB | 0 | 1 | 8.1 MiB |",
            "| restart | b33p | 2 | 8.2 MiB | 2 | 0 | 0 B |",
            "| **all checks** | b33p | 4 | 16.5 MiB | 2 | 1 | 8.1 MiB |",
            "1 more between checks",
        ] {
            assert!(out.contains(line), "{line:?} not in\n{out}");
        }
    }
}
