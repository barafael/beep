//! Message formatting: typed as shortcuts (`*bold*`, `_italic_`, `~strikethrough~`,
//! `` `monospace` `` and `||spoiler||`, as in Signal's desktop app), and converted to and from
//! Signal's body ranges, which count UTF-16 code units and stand mentions in for U+FFFC.

use crate::{Mention, Style, StyleRange};

const MARKERS: [(&str, Style); 5] = [
    ("||", Style::Spoiler),
    ("*", Style::Bold),
    ("_", Style::Italic),
    ("~", Style::Strikethrough),
    ("`", Style::Monospace),
];

/// Takes the markers out of `text`, returning the plain text and its formatting in its bytes.
/// A marker only counts in a pair around non-blank text that doesn't start or end with a
/// space, so `2 * 3 * 4` and `snake_case_name` stay as they are. Inside `` ` `` nothing else is
/// parsed.
pub fn parse(text: &str) -> (String, Vec<StyleRange>) {
    let mut out = String::with_capacity(text.len());
    let mut styles = Vec::new();
    parse_into(text, &mut out, &mut styles);
    styles.sort_by_key(|s| (s.start, s.end));
    (out, styles)
}

fn parse_into(text: &str, out: &mut String, styles: &mut Vec<StyleRange>) {
    let mut rest = text;
    while !rest.is_empty() {
        if let Some((style, inner, after)) = span_at(rest, out) {
            let start = out.len();
            if style == Style::Monospace {
                out.push_str(inner);
            } else {
                parse_into(inner, out, styles);
            }
            styles.push(StyleRange {
                start,
                end: out.len(),
                style,
            });
            rest = after;
            continue;
        }
        let c = rest.chars().next().expect("not empty");
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
}

/// A formatted span starting at the beginning of `rest`: its style, inside and what
/// follows it.
fn span_at<'a>(rest: &'a str, before: &str) -> Option<(Style, &'a str, &'a str)> {
    // Inside a word, `_` and `*` are part of it (snake_case, 2*3).
    let in_word = before
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric);
    MARKERS.iter().find_map(|&(marker, style)| {
        let body = rest.strip_prefix(marker)?;
        if in_word && marker != "||" {
            return None;
        }
        if body.starts_with(char::is_whitespace) || body.starts_with(marker) {
            return None;
        }
        // The nearest closing marker that follows non-blank text and isn't inside a word.
        let mut from = 0;
        loop {
            let end = from + body[from..].find(marker)?;
            let inner = &body[..end];
            let after = &body[end + marker.len()..];
            let closes = !inner.is_empty()
                && !inner.ends_with(char::is_whitespace)
                && (marker == "||" || !after.starts_with(char::is_alphanumeric));
            if closes && !inner.contains('\n') {
                return Some((style, inner, after));
            }
            if inner.contains('\n') {
                return None;
            }
            from = end + marker.len();
        }
    })
}

/// What a Signal body range carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RangeValue {
    /// A service ID.
    Mention(String),
    Style(Style),
}

/// A body range as Signal sends it: start and length in UTF-16 code units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireRange {
    pub start: u32,
    pub length: u32,
    pub value: RangeValue,
}

/// Text received from Signal, made readable: each mention's placeholder becomes "@Name"
/// (`names` has a name per mention range; one without reads "@…"), and the styles and
/// mentions move to byte positions of the result.
pub fn decode(
    body: &str,
    ranges: &[WireRange],
    names: &[(u32, String)],
) -> (String, Vec<StyleRange>, Vec<Mention>) {
    let mut out = String::with_capacity(body.len());
    // Looked up by position rather than searched per character: senders choose how many
    // ranges there are. The first range or name at a position counts.
    let mut mentions = std::collections::HashMap::new();
    for range in ranges {
        if let RangeValue::Mention(who) = &range.value {
            mentions.entry(range.start).or_insert((range.length, who));
        }
    }
    let mut mentioned = Vec::new();
    let mut named = std::collections::HashMap::new();
    for (start, name) in names {
        named.entry(*start).or_insert(name.as_str());
    }
    // Where each UTF-16 position of `body` ended up in `out`.
    let mut at = Vec::with_capacity(body.len() + 1);
    let mut position = 0u32;
    let mut skip_until = 0u32;
    for c in body.chars() {
        at.extend(std::iter::repeat_n(out.len(), c.len_utf16()));
        if let Some((length, who)) = mentions.get(&position) {
            let start = out.len();
            out.push('@');
            out.push_str(named.get(&position).unwrap_or(&"…"));
            mentioned.push(Mention {
                start,
                end: out.len(),
                service_id: (*who).clone(),
            });
            skip_until = position.saturating_add(*length);
        }
        if position >= skip_until {
            match c {
                '\u{FFFC}' => out.push_str("@…"),
                c => out.push(c),
            }
        }
        position += c.len_utf16() as u32;
    }
    at.push(out.len());
    let byte = |p: u32| at.get(p as usize).copied().unwrap_or(out.len());
    let styles = ranges
        .iter()
        .filter_map(|r| match r.value {
            RangeValue::Style(style) => {
                let (start, end) = (byte(r.start), byte(r.start.saturating_add(r.length)));
                (start < end).then_some(StyleRange { start, end, style })
            }
            RangeValue::Mention(_) => None,
        })
        .collect();
    (out, styles, mentioned)
}

/// Text as Signal sends it: each mention ("@Name" at its bytes) becomes a placeholder, and
/// styles and mentions become body ranges.
pub fn encode(body: &str, styles: &[StyleRange], mentions: &[Mention]) -> (String, Vec<WireRange>) {
    let mut out = String::with_capacity(body.len());
    let mut ranges = Vec::new();
    let mut units = 0u32;
    // The UTF-16 position in `out` of each char boundary of `body`.
    let mut at = vec![0u32; body.len() + 1];
    let mut chars = body.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        at[i] = units;
        if let Some(mention) = mentions.iter().find(|m| m.start == i && m.end > i) {
            ranges.push(WireRange {
                start: units,
                length: 1,
                value: RangeValue::Mention(mention.service_id.clone()),
            });
            out.push('\u{FFFC}');
            units += 1;
            while let Some(&(j, _)) = chars.peek().filter(|(j, _)| *j < mention.end) {
                at[j] = units;
                chars.next();
            }
            continue;
        }
        out.push(c);
        units += c.len_utf16() as u32;
    }
    at[body.len()] = units;
    for range in styles {
        if range.end > body.len()
            || !body.is_char_boundary(range.start)
            || !body.is_char_boundary(range.end)
        {
            continue;
        }
        let (start, end) = (at[range.start], at[range.end]);
        if start < end {
            ranges.push(WireRange {
                start,
                length: end - start,
                value: RangeValue::Style(range.style),
            });
        }
    }
    (out, ranges)
}

/// The first web address in `text`, as a byte range: one with its scheme (http, https), or a
/// bare host name, `www.` or a domain under a well-known ending, as people type them;
/// trailing punctuation is left out.
pub fn find_url(text: &str) -> Option<(usize, usize)> {
    // Word by word: a bare host name is a word of its own (`a@b.org` is a mail address, and
    // `foo.example.com` is one word, found whole), while a scheme can follow anything.
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let Some(begin) = rest.find(|c: char| !c.is_whitespace()) else {
            break;
        };
        let word = &rest[begin..];
        let end = word
            .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
            .unwrap_or(word.len());
        let word = &word[..end];
        let start = at + begin;
        if let Some((from, to)) = url_in(word) {
            return Some((start + from, start + to));
        }
        at = start + end.max(1);
    }
    None
}

/// A name someone chose for themselves or a group (or a page title), fit to show on one line
/// among other text: line breaks and other control characters become spaces, and direction
/// overrides and isolates go, which would otherwise reorder the text around the name.
pub fn name(text: &str) -> String {
    let directional = |c: char| matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    let line_break = |c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}');
    let spaced: String = text
        .chars()
        .filter(|c| !directional(*c))
        .map(|c| if line_break(c) { ' ' } else { c })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `body` with its spoilers hidden, each of their characters but spaces shown as `▒`: where
/// they can't be revealed, like notifications, the chat list and quotes.
pub fn hide_spoilers(body: &str, styles: &[StyleRange]) -> String {
    let hidden = |at: usize| {
        styles
            .iter()
            .any(|s| s.style == Style::Spoiler && s.start <= at && at < s.end)
    };
    body.char_indices()
        .map(|(at, c)| {
            if hidden(at) && !c.is_whitespace() {
                '▒'
            } else {
                c
            }
        })
        .collect()
}

/// Text that [`parse`] reads back as `body` with `styles`: the formatting as typed shortcuts,
/// for editing a message. Formatting the shortcuts can't say is left out, but spoilers are kept
/// whenever they can be, so that editing doesn't reveal them.
pub fn typed(body: &str, styles: &[StyleRange]) -> String {
    let valid: Vec<&StyleRange> = styles
        .iter()
        .filter(|s| {
            s.start < s.end
                && s.end <= body.len()
                && body.is_char_boundary(s.start)
                && body.is_char_boundary(s.end)
        })
        .collect();
    let spoilers: Vec<&StyleRange> = valid
        .iter()
        .copied()
        .filter(|s| s.style == Style::Spoiler)
        .collect();
    for kept in [valid, spoilers] {
        let text = with_markers(body, &kept);
        let mut expected: Vec<StyleRange> = kept.into_iter().cloned().collect();
        expected.sort_by_key(|s| (s.start, s.end));
        if parse(&text) == (body.to_owned(), expected) {
            return text;
        }
    }
    body.to_owned()
}

/// `body` with each of `styles` as a pair of markers around its text.
fn with_markers(body: &str, styles: &[&StyleRange]) -> String {
    let marker = |style| {
        MARKERS
            .iter()
            .find(|(_, s)| *s == style)
            .map_or("", |(marker, _)| marker)
    };
    // At the same place: closing before opening, inner ones closing first and outer ones
    // opening first, so that they nest.
    let mut markers: Vec<(usize, bool, usize, &str)> = Vec::new();
    for s in styles {
        let length = s.end - s.start;
        markers.push((s.start, true, usize::MAX - length, marker(s.style)));
        markers.push((s.end, false, length, marker(s.style)));
    }
    markers.sort_by_key(|&(at, opens, order, _)| (at, opens, order));
    let mut text = String::with_capacity(body.len() + 4 * styles.len());
    let mut from = 0;
    for (at, _, _, marker) in markers {
        text.push_str(&body[from..at]);
        text.push_str(marker);
        from = at;
    }
    text.push_str(&body[from..]);
    text
}

/// The address in `word` (no whitespace), as a byte range in it.
fn url_in(word: &str) -> Option<(usize, usize)> {
    const TRAILING: [char; 11] = ['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"'];
    if let Some(start) = ["https://", "http://"]
        .iter()
        .filter_map(|scheme| word.find(scheme))
        .min()
    {
        let url = word[start..].trim_end_matches(TRAILING);
        // Just a scheme is no address.
        return (url.len() > "https://".len()).then_some((start, start + url.len()));
    }
    // Bare: `(www.example.org)` or `example.org/path.`
    let start = word
        .find(|c: char| !matches!(c, '(' | '[' | '{' | '\'' | '"'))
        .unwrap_or(word.len());
    let url = word[start..].trim_end_matches(TRAILING);
    let host_end = url
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        .unwrap_or(url.len());
    let host = &url[..host_end];
    // Only a port or a path may follow the host.
    let follows_well = url[host_end..]
        .chars()
        .next()
        .is_none_or(|c| matches!(c, '/' | '?' | '#' | ':'));
    (follows_well && is_host(host)).then_some((start, start + url.len()))
}

/// Whether `host` reads as a web host name without a scheme: labels of letters, digits and
/// dashes, under a well-known top-level domain, or any starting with `www.`.
fn is_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2
        || labels
            .iter()
            .any(|l| l.is_empty() || l.starts_with('-') || l.ends_with('-'))
    {
        return false;
    }
    let top = labels[labels.len() - 1];
    labels[0].eq_ignore_ascii_case("www")
        || (top.chars().all(|c| c.is_ascii_alphabetic())
            && KNOWN_TOPS.contains(&top.to_ascii_lowercase().as_str()))
}

/// Top-level domains under which a bare name is taken for a link. Not every one there is
/// (`node.js` and `e.g.` must stay text), the ones a message is likely to mean.
const KNOWN_TOPS: &[&str] = &[
    "com", "org", "net", "edu", "gov", "mil", "int", "io", "co", "me", "dev", "app", "info", "biz",
    "eu", "uk", "de", "at", "ch", "fr", "it", "es", "nl", "be", "lu", "se", "no", "dk", "fi", "is",
    "ie", "pt", "pl", "cz", "sk", "hu", "ro", "bg", "gr", "tr", "ru", "ua", "jp", "cn", "kr", "tw",
    "in", "sg", "us", "ca", "au", "nz", "br", "mx", "ar", "cl", "il", "za", "tv", "cc", "ly", "sh",
    "xyz", "ai", "gg", "to", "fm", "im", "ws", "link", "page", "site", "tech", "blog", "news",
    "store", "shop", "cloud", "social", "online", "email", "chat", "wiki", "zone", "art", "design",
    "studio", "live", "video", "games", "tools", "space", "world", "today", "one", "run", "rs",
    "fyi",
];

/// What `link`, as [`find_url`] found it, opens: with a scheme, if it was typed without.
pub fn link_target(link: &str) -> String {
    if link.starts_with("https://") || link.starts_with("http://") {
        link.to_owned()
    } else {
        format!("https://{link}")
    }
}

/// The web addresses in `text`, in order, each with its scheme (see [`link_target`]).
pub fn links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some((start, end)) = find_url(rest) {
        links.push(link_target(&rest[start..end]));
        rest = &rest[end..];
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Option<&str> {
        find_url(text).map(|(start, end)| &text[start..end])
    }

    #[test]
    fn finds_bare_host_names() {
        assert_eq!(found("see signal.org."), Some("signal.org"));
        assert_eq!(found("(www.example)"), Some("www.example"));
        assert_eq!(found("at docs.rs/foo?x=1, ok"), Some("docs.rs/foo?x=1"));
        assert_eq!(found("port host.dev:8080/a"), Some("host.dev:8080/a"));
        assert_eq!(found("\"Example.COM\""), Some("Example.COM"));
        assert_eq!(found("then https://x.y"), Some("https://x.y"));
        assert_eq!(found("here signal.org"), Some("signal.org"));
    }

    #[test]
    fn leaves_text_that_only_looks_like_an_address() {
        for text in [
            "mail me@example.com",
            "node.js and e.g. this",
            "version 1.2.3",
            "file.txt index.html",
            "a-.com -b.com .com com.",
            "example.com's",
            "foo.bar",
        ] {
            assert_eq!(found(text), None, "{text:?}");
        }
    }

    #[test]
    fn links_get_a_scheme() {
        assert_eq!(
            links("signal.org and http://x.y/z"),
            ["https://signal.org", "http://x.y/z"]
        );
    }

    /// Editing a message starts from its text as typed: the same formatting comes back out,
    /// and spoilers whenever they can.
    #[test]
    fn formatting_is_typed_back() {
        let range = |start, end, style| StyleRange { start, end, style };
        let spoiler = range(8, 12, Style::Spoiler);
        let nested = vec![range(0, 15, Style::Bold), range(9, 15, Style::Italic)];
        for (body, styles, typed_as, kept) in [
            ("plain", vec![], "plain", vec![]),
            (
                "the butler did it",
                vec![range(4, 10, Style::Spoiler)],
                "the ||butler|| did it",
                vec![range(4, 10, Style::Spoiler)],
            ),
            (
                "bold and italic",
                nested.clone(),
                "*bold and _italic_*",
                nested,
            ),
            (
                "mono",
                vec![range(0, 4, Style::Monospace)],
                "`mono`",
                vec![range(0, 4, Style::Monospace)],
            ),
            // Shortcuts other than spoilers can't start inside a word.
            (
                "unbelievable",
                vec![range(2, 8, Style::Bold), spoiler.clone()],
                "unbeliev||able||",
                vec![spoiler],
            ),
        ] {
            let typed = typed(body, &styles);
            assert_eq!(typed, typed_as);
            assert_eq!(parse(&typed), (body.to_owned(), kept));
        }
    }

    #[test]
    fn spoilers_are_hidden_where_they_cant_be_revealed() {
        let spoiler = StyleRange {
            start: 4,
            end: 14,
            style: Style::Spoiler,
        };
        assert_eq!(
            hide_spoilers("the butler 🦀 did it", &[spoiler]),
            "the ▒▒▒▒▒▒ ▒ did it"
        );
    }

    #[test]
    fn names_stay_on_their_line_and_in_their_place() {
        assert_eq!(name("Ann"), "Ann");
        assert_eq!(name("  Ann\nMarie\r\n\tB. "), "Ann Marie B.");
        // "Bob" written to look like "gnp.exe" read backwards.
        assert_eq!(name("Bob \u{202e}exe.gnp\u{202c}"), "Bob exe.gnp");
        assert_eq!(name("\u{2067}علي\u{2069}"), "علي");
        assert_eq!(name("\u{2028}"), "");
    }

    /// Ranges come from the sender, whatever their numbers.
    #[test]
    fn decoding_takes_any_ranges() {
        let mention = |start, length| WireRange {
            start,
            length,
            value: RangeValue::Mention("id".into()),
        };
        let (text, ..) = decode("\u{FFFC} hi", &[mention(0, u32::MAX)], &[]);
        assert_eq!(text, "@…");
        let (text, ..) = decode(
            "\u{FFFC} hi",
            &[mention(0, 1), mention(0, 5)],
            &[(0, "Ann".into()), (0, "Bob".into())],
        );
        assert_eq!(text, "@Ann hi");
    }

    #[test]
    fn mentions_and_styles_survive_the_round_trip() {
        // "é" is one UTF-16 unit but two bytes; "🦀" two units, four bytes.
        let body = "é🦀 hi @Ann, *look*";
        let (text, styles) = parse(body);
        assert_eq!(text, "é🦀 hi @Ann, look");
        let at = text.find("@Ann").unwrap();
        let mentions = [Mention {
            start: at,
            end: at + 4,
            service_id: "ann-id".into(),
        }];
        let (wire, ranges) = encode(&text, &styles, &mentions);
        assert_eq!(wire, "é🦀 hi \u{FFFC}, look");
        assert_eq!(
            ranges,
            vec![
                WireRange {
                    start: 7,
                    length: 1,
                    value: RangeValue::Mention("ann-id".into())
                },
                WireRange {
                    start: 10,
                    length: 4,
                    value: RangeValue::Style(Style::Bold)
                },
            ]
        );
        let (back, back_styles, _) = decode(&wire, &ranges, &[(7, "Ann".into())]);
        assert_eq!(back, text);
        assert_eq!(back_styles, styles);
    }

    #[test]
    fn mentions_become_names_counting_utf16() {
        let mention = |start| WireRange {
            start,
            length: 1,
            value: RangeValue::Mention("x".into()),
        };
        let decoded = decode(
            "Hi \u{FFFC}, see you",
            &[mention(3)],
            &[(3, "Alice".into())],
        );
        assert_eq!(decoded.0, "Hi @Alice, see you");
        let alice = &decoded.2[0];
        assert_eq!(&decoded.0[alice.start..alice.end], "@Alice");
        // An emoji before the mention takes two UTF-16 units.
        let decoded = decode("🎉 \u{FFFC}!", &[mention(3)], &[(3, "Bob".into())]);
        assert_eq!(decoded.0, "🎉 @Bob!");
    }

    #[test]
    fn unknown_mentions_read_as_ellipsis() {
        let ranges = [WireRange {
            start: 3,
            length: 1,
            value: RangeValue::Mention("x".into()),
        }];
        assert_eq!(decode("hi \u{FFFC}!", &ranges, &[]).0, "hi @…!");
        assert_eq!(decode("hi \u{FFFC}", &[], &[]).0, "hi @…");
    }

    fn range(start: usize, end: usize, style: Style) -> StyleRange {
        StyleRange { start, end, style }
    }

    #[test]
    fn markers_become_styles() {
        assert_eq!(
            parse("a *bold* and _it_"),
            (
                "a bold and it".into(),
                vec![range(2, 6, Style::Bold), range(11, 13, Style::Italic)]
            )
        );
        assert_eq!(
            parse("||secret|| `x*y*`"),
            (
                "secret x*y*".into(),
                vec![range(0, 6, Style::Spoiler), range(7, 11, Style::Monospace)]
            )
        );
        assert_eq!(
            parse("~*both*~"),
            (
                "both".into(),
                vec![range(0, 4, Style::Bold), range(0, 4, Style::Strikethrough)]
            )
        );
    }

    #[test]
    fn stray_markers_stay() {
        for text in [
            "2 * 3 * 4",
            "snake_case_name",
            "* list",
            "a *b",
            "**",
            "x_y_",
            "*a\nb*",
        ] {
            assert_eq!(parse(text), (text.to_string(), vec![]), "{text}");
        }
    }
}
