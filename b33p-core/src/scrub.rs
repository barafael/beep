//! Keeps user data out of the log.
//!
//! An error shown to the person using b33p names what went wrong: the contact, the number they
//! typed, the file. The same string is logged, and the log (the journal, usually) outlives the
//! moment, so [`scrub`] blanks that data first. By convention, user data in an error string goes
//! in “curly quotes”; what library errors carry on their own (URLs, service IDs, phone numbers,
//! paths) is recognised by shape.

/// `text` with user data blanked: “quoted” spans, URLs, UUIDs, phone numbers and paths.
pub fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut at_word_start = true;
    while let Some(c) = rest.chars().next() {
        if c == '“' {
            // Quoted user data: up to the closing quote, or the end if it never closes.
            let end = rest.find('”').map_or(rest.len(), |i| i + '”'.len_utf8());
            out.push_str("“…”");
            rest = &rest[end..];
            at_word_start = false;
            continue;
        }
        let taken = if at_word_start {
            url(rest)
                .map(|n| (n, "<url>"))
                .or_else(|| path(rest).map(|n| (n, "<path>")))
        } else {
            None
        }
        .or_else(|| uuid(rest).map(|n| (n, "<id>")))
        .or_else(|| phone(rest).map(|n| (n, "<number>")));
        match taken {
            Some((n, placeholder)) => {
                out.push_str(placeholder);
                rest = &rest[n..];
                at_word_start = false;
            }
            None => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
                at_word_start = c.is_whitespace() || matches!(c, '(' | '"' | '\'' | '[' | '{');
            }
        }
    }
    out
}

/// `value` as text with user data blanked, for a log field: `warn!(error = %scrubbed(&error), …)`.
/// Library errors carry URLs, service IDs, numbers and paths of their own.
pub fn scrubbed(value: &impl std::fmt::Display) -> String {
    scrub(&value.to_string())
}

/// The length of a token at the start of `s` that runs to whitespace, without the trailing
/// punctuation a sentence puts after it.
fn token(s: &str) -> usize {
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    s[..end]
        .trim_end_matches(['.', ',', ':', ';', ')', ']', '}', '"', '\''])
        .len()
}

/// A URL (`scheme://…`), such as reqwest puts in its errors.
fn url(s: &str) -> Option<usize> {
    let scheme = s.find("://")?;
    if scheme == 0 || !s[..scheme].bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    Some(token(s))
}

/// An absolute path: `/…` with another `/` in it, or `C:\…`.
fn path(s: &str) -> Option<usize> {
    let n = token(s);
    let t = &s[..n];
    let unix = t.starts_with('/') && t[1..].contains('/');
    let windows = t.len() > 3
        && t.as_bytes()[0].is_ascii_alphabetic()
        && t.as_bytes()[1] == b':'
        && matches!(t.as_bytes()[2], b'\\' | b'/');
    (unix || windows).then_some(n)
}

/// A UUID in its hyphenated form, which is how service IDs and protocol addresses print.
fn uuid(s: &str) -> Option<usize> {
    const SHAPE: [usize; 5] = [8, 4, 4, 4, 12];
    let bytes = s.as_bytes();
    let mut at = 0;
    for (i, run) in SHAPE.into_iter().enumerate() {
        if i > 0 {
            if bytes.get(at) != Some(&b'-') {
                return None;
            }
            at += 1;
        }
        if bytes.len() < at + run || !bytes[at..at + run].iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        at += run;
    }
    // Not the start of something longer.
    if bytes.get(at).is_some_and(u8::is_ascii_hexdigit) {
        return None;
    }
    Some(at)
}

/// A phone number: `+` and at least seven digits, with the spaces, dashes and brackets people
/// put between them.
fn phone(s: &str) -> Option<usize> {
    if !s.starts_with('+') {
        return None;
    }
    let mut digits = 0;
    let mut end = 1;
    for (i, c) in s.char_indices().skip(1) {
        if c.is_ascii_digit() {
            digits += 1;
            end = i + 1;
        } else if !matches!(c, ' ' | '-' | '(' | ')') {
            break;
        }
    }
    (digits >= 7).then_some(end)
}

#[cfg(test)]
mod tests {
    use super::{scrub, scrubbed};

    #[test]
    fn scrubbed_takes_anything_displayable() {
        let error = std::io::Error::other(r"can't open C:\Users\me\file.txt.");
        assert_eq!(scrubbed(&error), "can't open <path>.");
        assert_eq!(scrubbed(&"0d4c8a3e-7b1f-4e2a-9c5d-1a2b3c4d5e6f"), "<id>");
    }

    #[test]
    fn quoted_user_data_is_blanked() {
        assert_eq!(
            scrub("“Alice, Bob” aren't on Signal."),
            "“…” aren't on Signal."
        );
        assert_eq!(
            scrub("“+49 151 23456789” isn't a phone number."),
            "“…” isn't a phone number."
        );
        assert_eq!(
            scrub("can't read “tax return.pdf”: denied"),
            "can't read “…”: denied"
        );
        assert_eq!(scrub("left open “forever"), "left open “…”");
    }

    #[test]
    fn service_ids_are_blanked() {
        assert_eq!(
            scrub("untrusted identity for address 0d4c8a3e-7b1f-4e2a-9c5d-1a2b3c4d5e6f.1"),
            "untrusted identity for address <id>.1"
        );
        assert_eq!(
            scrub("Thread(contact=PNI:0D4C8A3E-7B1F-4E2A-9C5D-1A2B3C4D5E6F)"),
            "Thread(contact=PNI:<id>)"
        );
        // A hex blob that only starts like one stays.
        assert_eq!(
            scrub("key 0d4c8a3e-7b1f-4e2a-9c5d-1a2b3c4d5e6f00"),
            "key 0d4c8a3e-7b1f-4e2a-9c5d-1a2b3c4d5e6f00"
        );
    }

    #[test]
    fn urls_are_blanked() {
        assert_eq!(
            scrub(
                "error sending request for url (https://chat.signal.org/v1/profile/0d4c8a3e-7b1f-4e2a-9c5d-1a2b3c4d5e6f): timed out"
            ),
            "error sending request for url (<url>): timed out"
        );
        assert_eq!(
            scrub("link sgnl://linkdevice?uuid=x&pub_key=y."),
            "link <url>."
        );
        assert_eq!(scrub("a://b"), "<url>");
        // Not a URL: a scheme is letters.
        assert_eq!(scrub("ratio 1://2"), "ratio 1://2");
    }

    #[test]
    fn phone_numbers_are_blanked() {
        assert_eq!(
            scrub("found +49 151 23456789 twice"),
            "found <number> twice"
        );
        assert_eq!(scrub("found +1 (555) 010-9999."), "found <number>.");
        // Too short to be one.
        assert_eq!(scrub("retry in +5 s"), "retry in +5 s");
    }

    #[test]
    fn paths_are_blanked() {
        assert_eq!(
            scrub("failed to write /home/me/.local/share/b33p/key: denied"),
            "failed to write <path>: denied"
        );
        assert_eq!(
            scrub(r"can't open C:\Users\me\file.txt."),
            "can't open <path>."
        );
        // Slashes inside words are not paths.
        assert_eq!(scrub("input/output error"), "input/output error");
        assert_eq!(scrub("a / b"), "a / b");
        assert_eq!(scrub("at /tmp"), "at /tmp");
    }

    #[test]
    fn plain_text_is_unchanged() {
        let text = "This device was removed from your Signal account, so it can't send any more.";
        assert_eq!(scrub(text), text);
        assert_eq!(scrub(""), "");
    }
}
