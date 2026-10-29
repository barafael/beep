//! `compare.md`: the clients side by side. Their connections and TLS; which requests each made
//! and how often; how they keep their chat connection alive; where the same request differs in
//! its headers or its body's shape; and, check by check, what each did.

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
    checks(&mut out, events, spans);
    out
}

fn connections(out: &mut String, summaries: &[Summary], clients: &BTreeSet<&str>) {
    out.push_str("\n## Connections\n\n");
    out.push_str(
        "| client | connections | ports | ALPN | TLS hellos | longest (s) | not decoded |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|\n");
    for client in clients {
        let own: Vec<&Summary> = summaries
            .iter()
            .filter(|summary| summary.client == *client)
            .collect();
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
    let problems: Vec<String> = summaries
        .iter()
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

fn join<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    items
        .into_iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
