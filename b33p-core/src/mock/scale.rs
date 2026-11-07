//! More of everything in the mock, for measuring a window with data of a size a real account
//! has: `B33P_MOCK_SCALE=chats=200,messages=20000,members=500,picture=640,incoming=5` (debug
//! assertions only, as the mock's other switches are). Each key is optional.
//!
//! - `chats`: that many more conversations, contacts and groups alternating, of thirty
//!   messages each;
//! - `messages`: one more conversation, "Long thread", with that many, of the kinds a chat has
//!   (short and long, links, quotes, reactions, now and then a picture or a file);
//! - `members`: one more group, "Big group", with that many members;
//! - `picture`: the side, in pixels, of the pictures downloaded (320 otherwise);
//! - `incoming`: messages a second arriving in "Long thread" (or Bob's chat without it);
//! - `backlog`: that many messages, spread over the conversations, sent in one burst before the
//!   mock says it has synced, as the real backend's catching up with what came while the
//!   client was away.
//!   They are dated as a server delivers them (oldest first, now and then one late): a burst
//!   in the opposite order puts every message at the front of its chat's history, which
//!   moves all that is there each time and says nothing about what the backend does.
//!
//! Made up the same way every time, so that two windows are shown the same.

use std::sync::OnceLock;

use super::{attachment, message};
use crate::{AttachmentKind, ChatMessage, Person, Quote, Reaction};

/// How much the mock makes up; see the module's documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    pub chats: usize,
    pub messages: usize,
    pub members: usize,
    pub picture: u32,
    pub incoming: f32,
    pub backlog: usize,
}

impl Default for Scale {
    fn default() -> Self {
        Self {
            chats: 0,
            messages: 0,
            members: 0,
            picture: 320,
            incoming: 0.0,
            backlog: 0,
        }
    }
}

impl Scale {
    /// What `spec` says: `key=number` pairs, comma-separated. A key it doesn't know, or a
    /// number that isn't one, leaves that setting as it was.
    pub fn parse(spec: &str) -> Self {
        let mut scale = Self::default();
        for pair in spec.split(',') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "chats" => scale.chats = value.parse().unwrap_or(scale.chats),
                "messages" => scale.messages = value.parse().unwrap_or(scale.messages),
                "members" => scale.members = value.parse().unwrap_or(scale.members),
                "picture" => {
                    // A picture side that is too small or too large to be one is left out.
                    scale.picture = value
                        .parse()
                        .ok()
                        .filter(|side| (16..=4096).contains(side))
                        .unwrap_or(scale.picture);
                }
                "incoming" => scale.incoming = value.parse().unwrap_or(scale.incoming),
                "backlog" => scale.backlog = value.parse().unwrap_or(scale.backlog),
                _ => {}
            }
        }
        scale
    }

    /// The scale the environment asks for: none, in a release build.
    pub fn get() -> &'static Self {
        static SCALE: OnceLock<Scale> = OnceLock::new();
        SCALE.get_or_init(|| {
            if cfg!(debug_assertions)
                && let Some(spec) = std::env::var_os("B33P_MOCK_SCALE")
            {
                return Self::parse(&spec.to_string_lossy());
            }
            Self::default()
        })
    }

    /// The conversations this adds to the mock's own: id, title and whether it is a group.
    pub fn conversations(&self) -> Vec<(String, String, bool)> {
        let mut all = Vec::new();
        for n in 0..self.chats {
            if n % 2 == 0 {
                all.push((format!("group-{n}"), format!("Group {n}"), true));
            } else {
                all.push((format!("chat-{n}"), format!("Contact {n}"), false));
            }
        }
        if self.messages > 0 {
            all.push((LONG.into(), "Long thread".into(), false));
        }
        if self.members > 0 {
            all.push((CROWD.into(), "Big group".into(), true));
        }
        all
    }

    /// Where `incoming` messages arrive.
    pub fn incoming_chat(&self) -> &'static str {
        if self.messages > 0 { LONG } else { "bob" }
    }
}

/// The long conversation's name, and the big group's.
const LONG: &str = "long";
const CROWD: &str = "group-crowd";

/// The `n`th of `total` messages of the backlog, an incoming one in one of `chats` (their mock
/// names), made up the same way every time. They are dated as a server delivers what came while
/// the client was away: oldest first, over the last hour, and now and then one that took longer
/// (sent half a minute earlier than those around it).
pub fn backlog_message(n: usize, total: usize, chats: &[&str], now: u64) -> ChatMessage {
    let mut random = Random(0xdead_beef_cafe_f00d ^ n as u64);
    let chat = chats[n % chats.len()];
    let body = line(&mut random, 12);
    let sender = if chat.starts_with("group-") {
        "Friend"
    } else {
        chat
    };
    const HOUR: u64 = 3_600_000;
    let late = if n % 16 == 15 { 30_000 } else { 0 };
    let sent = now - HOUR + n as u64 * HOUR / total.max(1) as u64 - late;
    message(chat, sender, &body, sent, false)
}

/// The members of the big group.
pub fn members(count: usize) -> Vec<Person> {
    (0..count)
        .map(|n| Person {
            service_id: format!("member-{n}"),
            name: format!("Member {n}"),
        })
        .collect()
}

/// The stored history of a conversation this adds, or `None` for one of the mock's own.
pub fn history(name: &str, started: u64) -> Option<Vec<ChatMessage>> {
    let scale = Scale::get();
    if name == LONG && scale.messages > 0 {
        Some(thread(started, scale.messages))
    } else if name == CROWD && scale.members > 0 {
        Some(crowd(started, scale.members))
    } else if name.starts_with("chat-") || (name.starts_with("group-") && name != CROWD) {
        Some(small(name, started))
    } else {
        None
    }
}

/// A small LCG: enough to look irregular, and the same on every run.
struct Random(u64);

impl Random {
    fn below(&mut self, below: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % below.max(1)
    }
}

const WORDS: &[&str] = &[
    "the", "meeting", "tomorrow", "was", "moved", "to", "friday", "lunch", "maybe", "see", "you",
    "there", "thanks", "sounds", "good", "I", "can't", "make", "it", "tonight", "did", "anyone",
    "send", "the", "photos", "from", "last", "weekend", "yes", "no", "really", "ok", "great",
    "later", "call", "me", "when", "you", "land", "train", "is", "late", "again",
];

/// A line of up to `longest` words, now and then with a link or an emoji.
fn line(random: &mut Random, longest: u64) -> String {
    let words = 1 + random.below(longest);
    let mut text = (0..words)
        .map(|_| WORDS[random.below(WORDS.len() as u64) as usize])
        .collect::<Vec<_>>()
        .join(" ");
    match random.below(40) {
        0 => text.push_str(" https://example.org/some/page"),
        1 => text.push_str(" 😂"),
        2 => text.push_str(" 👍"),
        _ => {}
    }
    text
}

/// One conversation of `count` messages, two minutes apart, ending a minute ago.
fn thread(started: u64, count: usize) -> Vec<ChatMessage> {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let mut messages: Vec<ChatMessage> = Vec::with_capacity(count);
    for n in 0..count {
        let timestamp = started - (count - n) as u64 * 120_000 + 60_000;
        let outgoing = random.below(3) == 0;
        let sender = if outgoing { "You" } else { "Casey" };
        // Mostly one line, now and then a paragraph.
        let longest = if random.below(15) == 0 { 60 } else { 12 };
        let body = line(&mut random, longest);
        let mut m = ChatMessage {
            delivery: if outgoing {
                crate::Delivery::Read
            } else {
                crate::Delivery::default()
            },
            ..message(LONG, sender, &body, timestamp, outgoing)
        };
        let id = format!("long-{n}");
        match random.below(120) {
            0..2 => m.attachments.push(attachment(
                &id,
                AttachmentKind::Image,
                "image/jpeg",
                None,
                (800, 600),
            )),
            2 => m.attachments.push(attachment(
                &id,
                AttachmentKind::File,
                "application/pdf",
                Some("tickets.pdf"),
                (0, 0),
            )),
            _ => {}
        }
        if random.below(15) == 0
            && let Some(before) = messages.last()
        {
            m.quote = Some(Quote {
                author: before.sender.clone(),
                text: before.body.clone(),
                ..Default::default()
            });
        }
        if random.below(25) == 0 {
            m.reactions.push(Reaction {
                emoji: "👍".into(),
                sender_id: "casey".into(),
                sender: "Casey".into(),
                outgoing: false,
            });
        }
        messages.push(m);
    }
    messages
}

/// A few hundred messages from the members of the big group, which has `members`.
fn crowd(started: u64, members: usize) -> Vec<ChatMessage> {
    let mut random = Random(0x1234_5678_9abc_def1);
    (0..300u64)
        .map(|n| {
            let sender = format!("Member {}", random.below(members as u64));
            let body = line(&mut random, 14);
            message(CROWD, &sender, &body, started - (300 - n) * 90_000, false)
        })
        .collect()
}

/// Thirty messages of an added contact or group.
fn small(name: &str, started: u64) -> Vec<ChatMessage> {
    let mut random = Random(name.bytes().fold(7, |acc, b| acc * 31 + u64::from(b)));
    (0..30u64)
        .map(|n| {
            let outgoing = n % 3 == 2;
            let body = line(&mut random, 10);
            message(
                name,
                if outgoing { "You" } else { "Friend" },
                &body,
                started - (30 - n) * 600_000,
                outgoing,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_sets_what_it_names() {
        let scale = Scale::parse("chats=200, messages=20000,members=500,picture=640,incoming=2.5");
        assert_eq!(
            scale,
            Scale {
                chats: 200,
                messages: 20_000,
                members: 500,
                picture: 640,
                incoming: 2.5,
                backlog: 0,
            }
        );
    }

    #[test]
    fn what_it_does_not_understand_is_left_as_it_was() {
        let scale = Scale::parse("chats=many,picture=1,colour=red,messages");
        assert_eq!(scale, Scale::default());
    }

    #[test]
    fn the_conversations_added_are_as_many_as_asked() {
        let scale = Scale::parse("chats=4,messages=10,members=3");
        let all = scale.conversations();
        assert_eq!(all.len(), 6);
        assert_eq!(all.iter().filter(|(_, _, group)| *group).count(), 3);
        assert!(all.iter().any(|(id, _, _)| id == LONG));
    }

    #[test]
    fn the_backlog_goes_round_the_chats() {
        let chats = ["alice", "group-1", "chat-3"];
        let a = backlog_message(4, 100, &chats, 1_000_000_000_000);
        assert_eq!(a.conversation, super::super::chat_id("group-1"));
        assert!(!a.outgoing);
        assert_eq!(
            a.body,
            backlog_message(4, 100, &chats, 1_000_000_000_000).body
        );
    }

    #[test]
    fn the_backlog_is_dated_oldest_first_with_some_late() {
        let now = 1_000_000_000_000;
        let chats = ["alice"];
        let times: Vec<u64> = (0..200)
            .map(|n| backlog_message(n, 200, &chats, now).timestamp)
            .collect();
        assert!(
            times
                .iter()
                .all(|&t| t < now && t >= now - 3_600_000 - 30_000)
        );
        let late = times.windows(2).filter(|w| w[1] < w[0]).count();
        assert!((1..=200 / 16 + 1).contains(&late), "{late} late ones");
        assert!(times[0] < times[190]);
    }

    #[test]
    fn a_thread_is_made_up_the_same_way_every_time() {
        let a = thread(1_000_000_000_000, 500);
        let b = thread(1_000_000_000_000, 500);
        assert_eq!(a.len(), 500);
        assert!(a.windows(2).all(|w| w[0].timestamp < w[1].timestamp));
        assert!(
            a.iter().zip(&b).all(|(x, y)| x.body == y.body),
            "the same text each time"
        );
        assert!(a.iter().any(|m| !m.attachments.is_empty()));
    }
}
