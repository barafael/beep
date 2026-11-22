//! Text: snippets, mentions, initials and matching chats by name.

use super::*;

/// A line of `body` around where it contains `query` (ignoring case), for search results; with
/// where in it the match is (in bytes), to highlight.
pub fn snippet(body: &str, query: &str) -> (String, Option<Range<usize>>) {
    const AROUND: usize = 40;
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    let lower: Vec<char> = flat.to_lowercase().chars().collect();
    let wanted: Vec<char> = query.trim().to_lowercase().chars().collect();
    // Lowercasing can change the length (rarely); then just show the start.
    let at = if lower.len() == chars.len() && !wanted.is_empty() {
        lower
            .windows(wanted.len())
            .position(|w| w == wanted.as_slice())
    } else {
        None
    };
    let start = at.map_or(0, |at| at.saturating_sub(AROUND));
    let end = (at.unwrap_or(0) + wanted.len() + AROUND * 2)
        .min(chars.len())
        .max(start);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    let found = match at {
        Some(at) => {
            out.extend(&chars[start..at]);
            let from = out.len();
            out.extend(&chars[at..at + wanted.len()]);
            let found = from..out.len();
            out.extend(&chars[at + wanted.len()..end]);
            Some(found)
        }
        None => {
            out.extend(&chars[start..end]);
            None
        }
    };
    if end < chars.len() {
        out.push('…');
    }
    (out, found)
}

/// The first line of `text` that has something on it, trimmed: what a one-line preview of a
/// message shows.
pub fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

/// "Ann is typing…", "Ann and Bob are typing…"; empty if nobody is.
pub fn typing_label(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("{one} is typing…"),
        [one, two] => format!("{one} and {two} are typing…"),
        _ => "Several people are typing…".into(),
    }
}

/// Whether `text` is typed like a phone number, to look up: digits (at least six), maybe a
/// leading "+", and the spaces, dashes, dots, slashes and brackets people write them with.
pub fn looks_like_number(text: &str) -> bool {
    let text = text.trim();
    let text = text.strip_prefix('+').unwrap_or(text);
    let digits = text.chars().filter(char::is_ascii_digit).count();
    (6..=15).contains(&digits)
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || " -./()".contains(c))
}

/// What the draft's last word says after "@", if it starts with one.
pub(super) fn mention_query(draft: &str) -> Option<&str> {
    let word = draft.rsplit(char::is_whitespace).next()?;
    word.strip_prefix('@')
}

/// Where the picked people's "@Name" stands in `body`.
pub(super) fn mentions_in(body: &str, people: &[Person]) -> Vec<Mention> {
    let mut mentions: Vec<Mention> = Vec::new();
    // Longer names first, so "@Ann Lee" isn't taken for "@Ann".
    let mut people: Vec<&Person> = people.iter().collect();
    people.sort_by_key(|p| std::cmp::Reverse(p.name.len()));
    for person in people {
        let tag = format!("@{}", person.name);
        for (start, _) in body.match_indices(&tag) {
            let end = start + tag.len();
            if !mentions.iter().any(|m| start < m.end && m.start < end) {
                mentions.push(Mention {
                    start,
                    end,
                    service_id: person.service_id.clone(),
                });
            }
        }
    }
    mentions.sort_by_key(|m| m.start);
    mentions
}

/// Up to two uppercase initials for an avatar, `?` if the title has none.
pub fn initials(title: &str) -> String {
    let letters: String = title
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        // Some letters are more than one in uppercase ("ﬃ" is "FFI").
        .take(2)
        .collect();
    if letters.is_empty() {
        "?".into()
    } else {
        letters
    }
}

/// One emoji among a message's reactions; see [`count_reactions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReactionCount<'a> {
    pub emoji: &'a str,
    /// How many chose it.
    pub count: usize,
    /// Whether we are one of them.
    pub mine: bool,
}

/// Reactions as Signal's apps show them: each emoji once, in the order they first came.
pub fn count_reactions(reactions: &[crate::Reaction]) -> Vec<ReactionCount<'_>> {
    let mut counts: Vec<ReactionCount> = Vec::new();
    for reaction in reactions {
        match counts.iter_mut().find(|c| c.emoji == reaction.emoji) {
            Some(count) => {
                count.count += 1;
                count.mine |= reaction.outgoing;
            }
            None => counts.push(ReactionCount {
                emoji: &reaction.emoji,
                count: 1,
                mine: reaction.outgoing,
            }),
        }
    }
    counts
}

/// A stable colour per person or group (`0xRRGGBB`), for avatars without a picture and group
/// senders' names: muted hues in the spirit of Signal's avatar palette, all readable with
/// white text.
pub fn avatar_color(id: &ConversationId) -> u32 {
    const PALETTE: [u32; 8] = [
        0x3a76f0, 0x7d4fc9, 0xc4314b, 0xd36c1c, 0x2b8a60, 0x1e8a9e, 0x9a5b3c, 0x5c6bc0,
    ];
    PALETTE[avatar_index(id, PALETTE.len())]
}

/// Stable index into a UI's avatar palette of `len` colors.
pub fn avatar_index(id: &ConversationId, len: usize) -> usize {
    let hash =
        id.0.bytes()
            .fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    hash as usize % len.max(1)
}

/// `filter` as the chat lists match it against titles: trimmed, case folded.
pub(super) fn search_term(filter: &str) -> String {
    filter.trim().to_lowercase()
}

/// Whether a chat's title contains a [`search_term`].
pub(super) fn title_matches(conversation: &Conversation, term: &str) -> bool {
    term.is_empty() || conversation.title.to_lowercase().contains(term)
}
