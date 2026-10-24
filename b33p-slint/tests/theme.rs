//! The window takes its look from `ui/theme.slint`: colours, text sizes, corners, shadows and
//! durations are the `Theme`'s and `Motion`'s, not numbers scattered through the UI. This keeps
//! it that way, so that the same kind of thing looks the same everywhere and a change of look is
//! made in one place.

use std::path::Path;

/// What a line of a `.slint` file may not say outside `theme.slint`, and what to use instead.
fn problems(line: &str) -> Vec<&'static str> {
    // Comments may say anything.
    let code = line.split("//").next().unwrap_or_default();
    let mut found = Vec::new();
    if code.contains("Palette.") && !code.contains("Palette.color-scheme") {
        found.push("a colour of the style's Palette: use Theme's");
    }
    if hex_colour(code) {
        found.push("a colour of its own: give it a name in Theme");
    }
    if literal_after(code, "with-alpha(", |rest| rest.starts_with(')')) {
        found.push("a transparency of its own: use Theme.secondary, .tertiary, .faint, .fill");
    }
    // `1rem * Theme.small` is how sizes are given, in rem, from the Theme.
    let own_size = |rest: &str| {
        rest.starts_with("px") || (rest.starts_with("rem") && !rest.starts_with("rem * Theme."))
    };
    if literal_after(code, "font-size: ", own_size) {
        found.push(
            "a text size of its own: use 1rem * Theme.caption, .small, … or a text component",
        );
    }
    if literal_after(code, "border-radius: ", |rest| rest.starts_with("px"))
        && !code.contains(": 0px")
    {
        found.push("a corner of its own: use Theme.radius-…, or self.height / 2 for a pill");
    }
    if literal_after(code, "duration: ", |rest| rest.starts_with("ms")) && !code.contains(": 0ms") {
        found.push("a duration of its own: use Motion.fast, .normal, .slow");
    }
    if literal_after(code, "drop-shadow-blur: ", |rest| rest.starts_with("px")) {
        found.push("a shadow of its own: use Theme.popover-… or Theme.dialog-…");
    }
    found
}

/// Whether `code` has a `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` colour.
fn hex_colour(code: &str) -> bool {
    code.match_indices('#').any(|(at, _)| {
        let digits = code[at + 1..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .count();
        let after = code[at + 1 + digits..].chars().next();
        matches!(digits, 3 | 4 | 6 | 8) && !after.is_some_and(|c| c.is_alphanumeric() || c == '-')
    })
}

/// Whether `code` has `key` followed by a plain number, and then what `unit` accepts.
fn literal_after(code: &str, key: &str, unit: impl Fn(&str) -> bool) -> bool {
    code.match_indices(key).any(|(at, _)| {
        let rest = &code[at + key.len()..];
        let number = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .count();
        number > 0 && unit(&rest[number..])
    })
}

#[test]
fn the_look_comes_from_the_theme() {
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
    let mut wrong = Vec::new();
    for entry in std::fs::read_dir(&ui).expect("the ui folder") {
        let path = entry.expect("a file").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // The theme says it; the tray icon has no look of its own.
        if !name.ends_with(".slint") || name == "theme.slint" || name == "tray.slint" {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("readable");
        for (number, line) in text.lines().enumerate() {
            for problem in problems(line) {
                wrong.push(format!(
                    "{name}:{}: {problem}\n    {}",
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn the_check_finds_what_it_should() {
    assert!(!problems("background: Palette.alternate-background;").is_empty());
    assert!(problems("Palette.color-scheme = ColorScheme.dark;").is_empty());
    assert!(!problems("color: #c4314b;").is_empty());
    assert!(!problems("background: #0008;").is_empty());
    assert!(problems("text: \"#general\";").is_empty());
    assert!(!problems("color: Theme.text.with-alpha(0.5);").is_empty());
    assert!(problems("color: root.ink.with-alpha(Theme.secondary);").is_empty());
    assert!(problems("background: x.with-alpha(0.3 + 0.7 * level);").is_empty());
    assert!(!problems("font-size: 0.85rem;").is_empty());
    assert!(problems("font-size: 1rem * Theme.small;").is_empty());
    assert!(problems("font-size: root.width * 0.4;").is_empty());
    assert!(!problems("border-radius: 6px;").is_empty());
    assert!(problems("border-radius: self.height / 2;").is_empty());
    assert!(!problems("animate x { duration: 120ms * Motion.scale; }").is_empty());
    assert!(problems("animate x { duration: seek.pressed ? 0ms : Motion.fast; }").is_empty());
    assert!(problems("// font-size: 2rem; in a comment").is_empty());
}
