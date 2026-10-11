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
/// (`names` has a name per mention range; one without reads "@…"), and the styles move to
/// byte positions of the result.
pub fn decode(
    body: &str,
    ranges: &[WireRange],
    names: &[(u32, String)],
) -> (String, Vec<StyleRange>) {
    let mut out = String::with_capacity(body.len());
    // Looked up by position rather than searched per character: senders choose how many
    // ranges there are. The first range or name at a position counts.
    let mut mentions = std::collections::HashMap::new();
    for range in ranges {
        if matches!(range.value, RangeValue::Mention(_)) {
            mentions.entry(range.start).or_insert(range.length);
        }
    }
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
        if let Some(length) = mentions.get(&position) {
            out.push('@');
            out.push_str(named.get(&position).unwrap_or(&"…"));
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
    (out, styles)
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

/// The first http(s) address in `text`, as a byte range; trailing punctuation is left out.
pub fn find_url(text: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    loop {
        let start = from
            + ["https://", "http://"]
                .iter()
                .filter_map(|scheme| text[from..].find(scheme))
                .min()?;
        let tail = &text[start..];
        let length = tail
            .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
            .unwrap_or(tail.len());
        let url = tail[..length]
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"']);
        // Just a scheme is no address, but one may follow it.
        if url.len() > "https://".len() {
            return Some((start, start + url.len()));
        }
        from = start + "http".len();
    }
}

/// The web addresses in `text`, in order.
pub fn links(text: &str) -> Vec<&str> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some((start, end)) = find_url(rest) {
        links.push(&rest[start..end]);
        rest = &rest[end..];
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (text, _) = decode("\u{FFFC} hi", &[mention(0, u32::MAX)], &[]);
        assert_eq!(text, "@…");
        let (text, _) = decode(
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
        let (back, back_styles) = decode(&wire, &ranges, &[(7, "Ann".into())]);
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
