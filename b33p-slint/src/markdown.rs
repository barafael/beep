//! Message text as Slint styled text: Slint parses Markdown, so whatever a sender typed is
//! escaped, and only the formatting Signal sends along (and web addresses) becomes Markdown.

use std::ops::Range;

use b33p_core::format::find_url;
use b33p_core::{Style, StyleRange};

/// Message text as Slint styled text: everything escaped, so nothing a sender types is taken as
/// formatting, except web addresses, which become links.
pub fn rich_text(body: &str, styles: &[StyleRange], revealed: bool) -> slint::StyledText {
    slint::StyledText::from_markdown(&markdown(body, styles, revealed)).unwrap_or_else(|error| {
        tracing::warn!(%error, "message text that doesn't parse as Markdown");
        slint::StyledText::from_plain_text(&masked(body, styles, revealed))
    })
}

/// `body` with its spoilers hidden, unless `revealed`.
fn masked(body: &str, styles: &[StyleRange], revealed: bool) -> String {
    if revealed {
        body.to_owned()
    } else {
        b33p_core::format::hide_spoilers(body, styles)
    }
}

/// What stands for `c` in Markdown: a carriage return would end the line in the middle of what
/// `markdown` takes as one (a non-breaking space, as Markdown drops spaces starting a line), and
/// Slint reserves U+E541 (its placeholder for arguments).
fn markdown_char(c: char) -> char {
    match c {
        '\r' => '\u{a0}',
        '\u{e541}' => '\u{fffd}',
        c => c,
    }
}

/// `text` with `found` (a byte range in it) bold and in `color`, e.g. a search result's match.
pub fn highlighted(
    text: &str,
    found: Option<Range<usize>>,
    color: slint::Color,
) -> slint::StyledText {
    found
        .and_then(|found| highlight_markdown(text, found, color))
        .and_then(|markdown| slint::StyledText::from_markdown(&markdown).ok())
        .unwrap_or_else(|| slint::StyledText::from_plain_text(text))
}

/// See [`highlighted`]; `None` if `found` isn't a part of `text`.
fn highlight_markdown(text: &str, found: Range<usize>, color: slint::Color) -> Option<String> {
    if text.get(found.clone()).is_none_or(str::is_empty) {
        return None;
    }
    let mut markdown = String::with_capacity(text.len() + 48);
    escape(&text[..found.start], &mut markdown);
    // The joiners keep the markers working next to spaces, as in `markdown`.
    markdown.push_str(&format!(
        "<font color=\"#{:02x}{:02x}{:02x}\">**\u{2060}",
        color.red(),
        color.green(),
        color.blue()
    ));
    escape(&text[found.clone()], &mut markdown);
    markdown.push_str("\u{2060}**</font>");
    escape(&text[found.end..], &mut markdown);
    Some(markdown)
}

/// Escapes `text` for Markdown, so that it all shows as typed.
fn escape(text: &str, out: &mut String) {
    let mut run = 0;
    for c in text.chars() {
        breakable(c, &mut run, out);
        push_escaped(c, out);
    }
}

fn push_escaped(c: char, out: &mut String) {
    if c.is_ascii_punctuation() {
        out.push('\\');
    }
    out.push(markdown_char(c));
}

/// Lets a line break inside a word longer than a bubble is wide (a hash, a key, an address)
/// by putting an invisible break opportunity before every [`WORD_PART`]th character of it:
/// otherwise the bubble would be as wide as that word, past the chat's edge. `run` counts the
/// word's characters so far.
fn breakable(c: char, run: &mut usize, out: &mut String) {
    /// No word that wraps would be this long.
    const WORD_PART: usize = 24;
    if c.is_whitespace() {
        *run = 0;
        return;
    }
    if *run > 0 && run.is_multiple_of(WORD_PART) {
        out.push('\u{200b}');
    }
    *run += 1;
}

/// See [`rich_text`].
fn markdown(body: &str, styles: &[StyleRange], revealed: bool) -> String {
    /// Between markers and text, so emphasis opens and closes whatever the neighbouring
    /// characters are (CommonMark's flanking rules); invisible.
    const JOIN: char = '\u{2060}';
    const STYLES: [Style; 5] = [
        Style::Bold,
        Style::Italic,
        Style::Strikethrough,
        Style::Monospace,
        Style::Spoiler,
    ];
    let valid = |s: &&StyleRange| {
        s.start < s.end
            && s.end <= body.len()
            && body.is_char_boundary(s.start)
            && body.is_char_boundary(s.end)
    };
    // Found in the whole text: styles cutting an address into pieces mustn't change where its
    // pieces lead.
    let mut urls = Vec::new();
    let mut at = 0;
    while let Some((start, end)) = find_url(&body[at..]) {
        urls.push(at + start..at + end);
        at += end;
    }
    // Pieces with the same formatting throughout; lines apart, for the breaks between them.
    let mut cuts: Vec<usize> = vec![0, body.len()];
    cuts.extend(styles.iter().filter(valid).flat_map(|s| [s.start, s.end]));
    cuts.extend(body.match_indices('\n').flat_map(|(i, _)| [i, i + 1]));
    cuts.extend(urls.iter().flat_map(|url| [url.start, url.end]));
    cuts.sort_unstable();
    cuts.dedup();
    // How many ranges of each style cover each piece, counted in one pass rather than by
    // asking every range for every piece: senders choose how many ranges there are.
    let piece = |at: usize| cuts.binary_search(&at).expect("every range bound is a cut");
    let mut covering = vec![[0i32; STYLES.len()]; cuts.len()];
    for range in styles.iter().filter(valid) {
        let style = STYLES
            .iter()
            .position(|s| *s == range.style)
            .expect("every style is listed");
        covering[piece(range.start)][style] += 1;
        covering[piece(range.end)][style] -= 1;
    }
    let mut active = [0i32; STYLES.len()];
    let mut urls = urls.into_iter().peekable();
    let mut markdown = String::with_capacity(body.len() + 16);
    for (index, bounds) in cuts.windows(2).enumerate() {
        for (count, change) in active.iter_mut().zip(covering[index]) {
            *count += change;
        }
        let (start, end) = (bounds[0], bounds[1]);
        let text = &body[start..end];
        if text == "\n" {
            // A hard line break, as the sender typed it; none at the end, where it would show
            // as a backslash.
            if !body[end..].chars().all(char::is_whitespace) {
                markdown.push_str("\\\n");
            }
            continue;
        }
        let has = |style| active[STYLES.iter().position(|s| *s == style).unwrap_or_default()] > 0;
        while urls.next_if(|url| url.end <= start).is_some() {}
        let url = urls
            .peek()
            .filter(|url| url.start <= start)
            .map(|url| &body[url.clone()]);
        // Leading spaces would start a code block; keep them as non-breaking spaces.
        let line_start = start == 0 || body.as_bytes()[start - 1] == b'\n';
        let text = if line_start {
            let trimmed = text.trim_start_matches(' ');
            markdown.extend(std::iter::repeat_n('\u{a0}', text.len() - trimmed.len()));
            // So is a tab.
            if trimmed.len() == text.len() && trimmed.starts_with('\t') {
                markdown.push(JOIN);
            }
            trimmed
        } else {
            text
        };
        if text.is_empty() {
            continue;
        }
        if has(Style::Spoiler) && !revealed {
            // Hidden behind a link that reveals it; see `Bubble` in app.slint.
            let hidden: String = text
                .chars()
                .map(|c| if c.is_whitespace() { c } else { '▒' })
                .collect();
            markdown.push('[');
            markdown.push_str(&hidden);
            markdown.push_str("](spoiler:)");
            continue;
        }
        let markers: Vec<&str> = [
            (Style::Bold, "**"),
            (Style::Italic, "*"),
            (Style::Strikethrough, "~~"),
        ]
        .into_iter()
        .filter(|(style, _)| has(*style))
        .map(|(_, marker)| marker)
        .collect();
        // Apart from the previous piece's closing markers, which would otherwise run into these.
        if !markers.is_empty() {
            markdown.push(JOIN);
        }
        for marker in &markers {
            markdown.push_str(marker);
            markdown.push(JOIN);
        }
        if has(Style::Monospace) {
            // Code spans take no escapes; a longer fence than any backtick run inside.
            let longest = text
                .split(|c| c != '`')
                .map(str::len)
                .max()
                .unwrap_or_default();
            let fence = "`".repeat(longest + 1);
            // Kept apart from a code span just before, whose fence it would lengthen.
            if markdown.ends_with('`') {
                markdown.push(JOIN);
            }
            // Spaces around it keep backticks inside from joining the fence; they are taken off
            // again, except from a span of nothing but spaces.
            let pad = if text.chars().all(|c| c == ' ') {
                ""
            } else {
                " "
            };
            markdown.push_str(&fence);
            markdown.push_str(pad);
            let mut run = 0;
            for c in text.chars() {
                breakable(c, &mut run, &mut markdown);
                markdown.push(markdown_char(c));
            }
            markdown.push_str(pad);
            markdown.push_str(&fence);
        } else if let Some(url) = url {
            // The whole address, escaped too: destinations decode escapes and entities, which
            // would make it lead somewhere other than what it shows.
            markdown.push('[');
            escape(text, &mut markdown);
            markdown.push_str("](<");
            for c in url.chars() {
                push_escaped(c, &mut markdown);
            }
            markdown.push_str(">)");
        } else {
            escape(text, &mut markdown);
        }
        for marker in markers.iter().rev() {
            markdown.push(JOIN);
            markdown.push_str(marker);
        }
    }
    markdown
}

#[cfg(test)]
mod tests {
    use i_slint_common::styled_text::{StyledTextParagraph, parse_interpolated};
    use proptest::prelude::*;
    use proptest::sample::Index;

    use super::*;

    /// The text shown, without the invisible break opportunities.
    fn raw(text: &slint::StyledText) -> String {
        i_slint_core::styled_text::get_raw_text(text).replace('\u{200b}', "")
    }

    #[test]
    fn finds_web_addresses_without_trailing_punctuation() {
        let text = "see https://example.org/a?b=1. and (http://x.y/z)";
        let (start, end) = find_url(text).unwrap();
        assert_eq!(&text[start..end], "https://example.org/a?b=1");
        let rest = &text[end..];
        let (start, end) = find_url(rest).unwrap();
        assert_eq!(&rest[start..end], "http://x.y/z");
        assert_eq!(find_url("no links here, nor in https:// alone"), None);
    }

    /// What people type is shown as typed: no markdown, no HTML, line breaks kept.
    #[test]
    fn message_text_is_never_formatting() {
        for text in [
            "*not bold* _nor this_ `code` <b>tag</b> # heading",
            "1. not a list",
            "    indented",
            "two\nlines",
        ] {
            let shown = raw(&rich_text(text, &[], false));
            assert_eq!(shown.replace('\u{a0}', " "), text, "{text:?}");
        }
    }

    #[test]
    fn links_keep_their_text() {
        let shown = raw(&rich_text("look: https://example.org/x_y*z ok", &[], false));
        assert_eq!(shown, "look: https://example.org/x_y*z ok");
    }

    /// Formatting applies whatever surrounds it, and the text stays as it was.
    #[test]
    fn formatting_keeps_the_text() {
        let range = |start, end, style| StyleRange { start, end, style };
        let text = "a\"b\" (c) `d` e";
        let styles = [
            range(1, 4, Style::Bold),
            range(5, 8, Style::Italic),
            range(9, 12, Style::Monospace),
            range(0, 14, Style::Strikethrough),
        ];
        // Parsed, not the plain-text fallback: the markers are gone.
        let parse = |styles: &[StyleRange], revealed| {
            let markdown = markdown(text, styles, revealed);
            raw(&slint::StyledText::from_markdown(&markdown).expect("valid markdown"))
                .replace('\u{2060}', "")
        };
        assert_eq!(parse(&styles, false), text);
        let spoiler = [range(5, 8, Style::Spoiler)];
        assert_eq!(parse(&spoiler, false), "a\"b\" ▒▒▒ `d` e");
        assert_eq!(parse(&spoiler, true), text);
    }

    /// A search result's match stands out, whatever surrounds it, and the text stays as it was.
    #[test]
    fn highlighting_keeps_the_text() {
        let accent = slint::Color::from_rgb_u8(0x33, 0x99, 0xff);
        for (text, found) in [
            ("Message 249", 0..10),
            ("…say *hi* to (Ann)!", 13..18),
            ("Grüße aus München", 12..20),
            ("a b c", 1..4),
            ("<b>", 0..3),
        ] {
            let markdown = highlight_markdown(text, found, accent).expect("a part of it");
            let parsed = slint::StyledText::from_markdown(&markdown).expect("valid markdown");
            assert_eq!(raw(&parsed).replace('\u{2060}', ""), text, "{markdown:?}");
        }
        assert_eq!(highlight_markdown("abc", 2..9, accent), None);
        assert_eq!(raw(&highlighted("abc", None, accent)), "abc");
    }

    /// What Slint makes of `markdown`: the text of each line and its links, or why it failed.
    fn parse(markdown: &str) -> Result<Vec<StyledTextParagraph>, String> {
        let (paragraphs, errors) = parse_interpolated::<&[StyledTextParagraph]>(markdown, &[]);
        match errors.as_slice() {
            [] => Ok(paragraphs),
            errors => Err(errors.iter().map(ToString::to_string).collect()),
        }
    }

    /// The shown text, without what doesn't show (joiners) or can't be told apart from spaces.
    fn shown(paragraphs: &[StyledTextParagraph]) -> String {
        let text: Vec<&str> = paragraphs.iter().map(|p| p.text.as_str()).collect();
        tidy(&text.join("\n"))
    }

    fn tidy(text: &str) -> String {
        let text = text
            .replace(['\u{2060}', '\u{200b}'], "")
            .replace(['\u{a0}', '\r'], " ");
        // Trailing spaces of a line don't show, and CommonMark drops them.
        let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches(' ')).collect();
        lines.join("\n").trim_end().to_owned()
    }

    /// Text as people type it, links included, with what Markdown would take for formatting.
    fn typed() -> impl Strategy<Value = String> {
        let piece = prop_oneof![
            10 => prop::sample::select(vec![
                "a", "word", " ", "  ", "\n", "\t", ".", ",", ")", "(", "!", "*", "_", "~", "`",
                "``", "|", "#", "<", ">", "&", "&amp;", "&#64;", "\\", "[", "]", "-", "1.", ":",
                "/", "@", "'", "\"", "é", "🦀", "e\u{301}", "\u{a0}", "\u{3000}", "\r",
                "\u{e541}", "\u{2028}", "\0",
            ]),
            3 => prop::sample::select(vec![
                "https://example.org/a",
                "http://x.y/z_w",
                "https://ex.am/p?q=1&r=2",
                "https://bank.example\\@evil.example/",
                "https://bank.example&#64;evil.example/",
                "https://",
                "www.example.org",
            ]),
        ];
        prop::collection::vec(piece, 0..16).prop_map(|pieces| pieces.concat())
    }

    /// Ranges of `body` on its character boundaries, in any of the styles.
    fn styled(body: &str, picks: &[(Index, Index, usize)]) -> Vec<StyleRange> {
        const STYLES: [Style; 5] = [
            Style::Bold,
            Style::Italic,
            Style::Strikethrough,
            Style::Monospace,
            Style::Spoiler,
        ];
        let bounds: Vec<usize> = body
            .char_indices()
            .map(|(i, _)| i)
            .chain([body.len()])
            .collect();
        picks
            .iter()
            .map(|(a, b, style)| {
                let (a, b) = (*a.get(&bounds), *b.get(&bounds));
                StyleRange {
                    start: a.min(b),
                    end: a.max(b),
                    style: STYLES[style % STYLES.len()],
                }
            })
            .filter(|s| s.start < s.end)
            .collect()
    }

    fn picks() -> impl Strategy<Value = Vec<(Index, Index, usize)>> {
        prop::collection::vec(any::<(Index, Index, usize)>(), 0..4)
    }

    proptest! {
        /// Whatever arrives parses: otherwise it shows as plain text, without its formatting
        /// and links.
        #[test]
        fn any_text_parses(body in typed(), picks in picks(), revealed: bool) {
            let styles = styled(&body, &picks);
            let markdown = markdown(&body, &styles, revealed);
            prop_assert!(parse(&markdown).is_ok(), "{body:?} {styles:?}\n{markdown:?}");
        }

        /// It shows as typed, whatever looks like formatting; only spoilers are hidden.
        #[test]
        fn text_shows_as_typed(body in typed(), picks in picks()) {
            let styles = styled(&body, &picks);
            let markdown = markdown(&body, &styles, true);
            let paragraphs = parse(&markdown).unwrap();
            let typed = tidy(&body.chars().map(markdown_char).collect::<String>());
            prop_assert_eq!(shown(&paragraphs), typed, "{:?}", markdown);
        }

        /// A link leads to a whole web address of the message, the one it shows.
        #[test]
        fn links_lead_where_they_show(body in typed(), picks in picks()) {
            let styles = styled(&body, &picks);
            let markdown = markdown(&body, &styles, true);
            let shown: String = body.chars().map(markdown_char).collect();
            let urls = b33p_core::format::links(&shown);
            for paragraph in parse(&markdown).unwrap() {
                for (range, target) in &paragraph.links {
                    prop_assert!(urls.contains(&target.as_str()), "{target:?} in {markdown:?}");
                    let label = tidy(&paragraph.text[range.clone()]);
                    prop_assert!(target.contains(&label), "{label:?} leads to {target:?}");
                }
            }
        }

        /// Spoilers stay hidden until revealed, also when the text doesn't parse.
        #[test]
        fn spoilers_stay_hidden(before in typed(), secret in "[a-z]{6}", after in typed()) {
            prop_assume!(!before.contains(&secret) && !after.contains(&secret));
            let body = format!("{before}{secret}{after}");
            let start = before.len();
            let styles = [StyleRange { start, end: start + secret.len(), style: Style::Spoiler }];
            let markdown = markdown(&body, &styles, false);
            prop_assert!(!shown(&parse(&markdown).unwrap()).contains(&secret));
            prop_assert!(!masked(&body, &styles, false).contains(&secret));
        }

        /// A search result shows its snippet as it is, with the match standing out.
        #[test]
        fn highlighted_snippets_parse(body in typed(), query in typed()) {
            let (text, found) = b33p_core::state::snippet(&body, &query);
            if let Some(found) = found {
                let accent = slint::Color::from_rgb_u8(0x33, 0x99, 0xff);
                let markdown = highlight_markdown(&text, found, accent).expect("a part of it");
                let paragraphs = parse(&markdown);
                prop_assert!(paragraphs.is_ok(), "{text:?}\n{markdown:?}");
                let typed = tidy(&text.chars().map(markdown_char).collect::<String>());
                prop_assert_eq!(shown(&paragraphs.unwrap()), typed, "{:?}", markdown);
            }
        }
    }

    /// A word longer than a bubble is wide can break, so the bubble stays within the chat; an
    /// address in it still leads where it says.
    #[test]
    fn long_words_can_break() {
        let hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let url = format!("https://example.org/{hash}");
        let body = format!("sha256: {hash} {url}");
        let mono = [StyleRange {
            start: 0,
            end: 6,
            style: Style::Monospace,
        }];
        let markdown = markdown(&body, &mono, true);
        let paragraphs = parse(&markdown).unwrap();
        let text = &paragraphs[0].text;
        assert!(text.contains('\u{200b}'));
        assert!(
            text.split(['\u{200b}', ' '])
                .all(|part| part.chars().count() <= 24)
        );
        assert_eq!(paragraphs[0].links[0].1, url);
        assert_eq!(shown(&paragraphs), body);
    }

    /// The reviewed ways a link could lead somewhere other than it shows.
    #[test]
    fn links_lead_only_where_they_show() {
        let range = |start, end, style| StyleRange { start, end, style };
        for (body, styles) in [
            ("https://bank.example\\@evil.example/login", vec![]),
            ("https://bank.example&#64;evil.example/", vec![]),
            ("https://bank.example&amp;x/", vec![]),
            ("https://bank.com/login", vec![range(15, 22, Style::Italic)]),
            ("use https:// or see https://example.org/x", vec![]),
        ] {
            let markdown = markdown(body, &styles, true);
            let urls = b33p_core::format::links(body);
            assert!(!urls.is_empty(), "{body:?}");
            for paragraph in parse(&markdown).unwrap() {
                for (_, target) in &paragraph.links {
                    assert!(urls.contains(&target.as_str()), "{target:?} from {body:?}");
                }
            }
        }
    }
}
