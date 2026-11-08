//! Opening a web page in the browser.

#[cfg(test)]
thread_local! {
    /// What the tests asked to open, which they must not really open.
    pub static OPENED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Opens `url` in the default browser; says in the log if it can't.
pub fn open(url: &str) {
    #[cfg(test)]
    OPENED.with(|opened| opened.borrow_mut().push(url.to_owned()));
    #[cfg(not(test))]
    if let Err(error) = open::that_detached(url) {
        tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "could not open a web page");
    }
}

/// Opens a link from a message, the one way every message and panel does: a group's invite link
/// is for joining here; a web address goes to the browser, after asking if it looks like it
/// means to deceive; anything else (files, other apps' schemes) is not opened, as links come
/// from message text and nothing but a web page should be opened by one.
pub fn open_link(shared: &crate::shared::Shared, link: &str, cx: &mut gpui_kit::App) {
    if is_join_link(link) {
        shared.command(cx, |chat| chat.open_join(Some(link)));
    } else if is_web(link) {
        if is_deceptive(link) {
            shared.confirm_link(link, cx);
        } else {
            open(link);
        }
    }
}

/// Opens `link` after the user was asked about it: only web addresses, still.
pub fn open_confirmed(link: &str) {
    if is_web(link) {
        open(link);
    }
}

pub fn is_join_link(link: &str) -> bool {
    link.starts_with("https://signal.group/#") || link.starts_with("sgnl://signal.group/#")
}

fn is_web(link: &str) -> bool {
    link.starts_with("https://") || link.starts_with("http://")
}

/// Whether the address reads as one site and goes to another: the part that names the host has a
/// user name (`google.com@evil.example`), letters of other alphabets that look like ours
/// (`аpple.com` with a Cyrillic а), or an encoded form of those (`xn--`); or the address has
/// control or direction characters, which hide what it says.
pub fn is_deceptive(link: &str) -> bool {
    let rest = link.split_once("://").map_or(link, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    host.contains('@')
        || !host.is_ascii()
        || host
            .split(['.', ':'])
            .any(|label| label.len() >= 4 && label[..4].eq_ignore_ascii_case("xn--"))
        || link.chars().any(hides_text)
}

/// Control characters, direction marks and joiners: what makes text read other than it is.
fn hides_text(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{061c}' | '\u{feff}'
        )
}

/// `link` with every character outside ASCII spelled as its code point, for a dialog to show
/// what an address really is: `https://[U+0430]pple.com`.
pub fn spelled_out(link: &str) -> String {
    link.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_control() {
                c.to_string()
            } else {
                format!("[U+{:04X}]", c as u32)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_addresses_open_without_a_question() {
        for link in [
            "https://example.org/",
            "http://example.org:8080/a?b=c#d",
            "https://de.wikipedia.org/wiki/Zürich",
            "https://example.org/path@with-at",
            "https://sub.example.co.uk",
        ] {
            assert!(!is_deceptive(link), "{link}");
        }
    }

    #[test]
    fn lookalikes_are_asked_about() {
        for link in [
            "https://\u{430}pple.com/",
            "https://google.com@evil.example/",
            "https://xn--pple-43d.com/",
            "https://example.org/\u{202e}gpj.exe",
            "https://exam\u{200b}ple.org",
            "https://example.org/\u{7}",
        ] {
            assert!(is_deceptive(link), "{link:?}");
        }
    }

    #[test]
    fn a_dialog_shows_what_an_address_is() {
        assert_eq!(
            spelled_out("https://\u{430}pple.com"),
            "https://[U+0430]pple.com"
        );
        assert_eq!(
            spelled_out("https://a.org/\u{202e}x"),
            "https://a.org/[U+202E]x"
        );
    }
}
