//! Message text: Signal's formatting (bold, italic, strikethrough, monospace, spoilers), web
//! addresses and mentions as links, and text that can be selected with the pointer and copied.
//!
//! [`build`] turns a message's text and the formatting that came with it into the text to show
//! and the pieces of it that differ; it is pure, and tested. [`RichText`] draws that, and takes
//! the pointer: a click on a link or a spoiler is the caller's to answer, a drag selects.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use b33p_core::format::{find_url, link_target};
use b33p_core::{Mention, Style, StyleRange};
use gpui_kit::component::Theme;
use gpui_kit::{
    App, BorderStyle, Bounds, ClipboardItem, Corners, CursorStyle, DispatchPhase, Edges, Element,
    ElementId, FocusHandle, FontStyle, FontWeight, Global, GlobalElementId, HighlightStyle, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, SharedString,
    StrikethroughStyle, StyledText, TextLayout, UnderlineStyle, Window, px, transparent_black,
};

/// The web addresses in `body`, as they would be opened, at most `most`: what a message's menu
/// offers for those who can't click them in the text.
pub fn links(body: &str, most: usize) -> Vec<String> {
    let mut found = Vec::new();
    let mut at = 0;
    while found.len() < most
        && let Some((start, end)) = find_url(&body[at..])
    {
        found.push(link_target(&body[at + start..at + end]));
        at += end;
    }
    found
}

/// What a piece of text leads to when it is clicked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A web address, with its scheme (see [`link_target`]).
    Url(String),
    /// The service ID of the person a mention is of: their chat.
    Mention(String),
    /// A hidden spoiler, which a click reveals.
    Spoiler,
}

/// A stretch of the text shown with the same formatting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Piece {
    /// Bytes of [`Rich::text`].
    pub range: Range<usize>,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub monospace: bool,
    pub target: Option<Target>,
}

impl Piece {
    fn plain(&self) -> bool {
        self.target.is_none() && !(self.bold || self.italic || self.strikethrough || self.monospace)
    }

    fn same_look(&self, other: &Piece) -> bool {
        Piece {
            range: 0..0,
            ..self.clone()
        } == Piece {
            range: 0..0,
            ..other.clone()
        }
    }
}

/// The text to show, and how its pieces look.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rich {
    /// Hidden spoilers are blocks (`▒`) in place of their characters.
    pub text: String,
    /// In order and without gaps or overlaps, but only where something differs from plain.
    pub pieces: Vec<Piece>,
    /// `text` as the shared string drawing takes, made once.
    pub shown: SharedString,
}

const STYLES: [Style; 5] = [
    Style::Bold,
    Style::Italic,
    Style::Strikethrough,
    Style::Monospace,
    Style::Spoiler,
];

/// `body` with the formatting Signal sent along, web addresses as links and mentions as links to
/// the person, as pieces; spoilers hidden unless `revealed`. Ranges that don't fit the text are
/// ignored: they are the sender's.
pub fn build(body: &str, styles: &[StyleRange], mentions: &[Mention], revealed: bool) -> Rich {
    let fits = |start: usize, end: usize| {
        start < end
            && end <= body.len()
            && body.is_char_boundary(start)
            && body.is_char_boundary(end)
    };
    let valid = |s: &&StyleRange| fits(s.start, s.end);
    let mut mentions: Vec<&Mention> = mentions.iter().filter(|m| fits(m.start, m.end)).collect();
    mentions.sort_unstable_by_key(|m| m.start);
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
    cuts.extend(mentions.iter().flat_map(|m| [m.start, m.end]));
    cuts.sort_unstable();
    cuts.dedup();
    // How many ranges of each style cover each piece, counted in one pass rather than by
    // asking every range for every piece: senders choose how many ranges there are.
    let piece_at = |at: usize| cuts.binary_search(&at).unwrap_or_default();
    let mut covering = vec![[0i32; STYLES.len()]; cuts.len()];
    for range in styles.iter().filter(valid) {
        let style = STYLES
            .iter()
            .position(|s| *s == range.style)
            .unwrap_or_default();
        covering[piece_at(range.start)][style] += 1;
        covering[piece_at(range.end)][style] -= 1;
    }
    let mut active = [0i32; STYLES.len()];
    let mut urls = urls.into_iter().peekable();
    // The mention a piece is in: the last one starting at or before it, if it reaches that far.
    let mut mentions = mentions.into_iter().peekable();
    let mut mention: Option<&Mention> = None;
    let mut out = Rich::default();
    for (index, bounds) in cuts.windows(2).enumerate() {
        for (count, change) in active.iter_mut().zip(covering[index]) {
            *count += change;
        }
        let (start, end) = (bounds[0], bounds[1]);
        let text = &body[start..end];
        if text == "\n" {
            // A line break as the sender typed it; none at the end, where it would show as
            // an empty line.
            if !body[end..].chars().all(char::is_whitespace) {
                push(&mut out, "\n", Piece::default());
            }
            continue;
        }
        let has = |style| active[STYLES.iter().position(|s| *s == style).unwrap_or_default()] > 0;
        while urls.next_if(|url| url.end <= start).is_some() {}
        let url = urls
            .peek()
            .filter(|url| url.start <= start)
            .map(|url| &body[url.clone()]);
        while let Some(m) = mentions.next_if(|m| m.start <= start) {
            mention = Some(m);
        }
        let mention = mention.filter(|m| end <= m.end);
        // Carriage returns would show as a break of their own.
        let text: String = text.chars().filter(|c| *c != '\r').collect();
        if text.is_empty() {
            continue;
        }
        if has(Style::Spoiler) && !revealed {
            let hidden: String = text
                .chars()
                .map(|c| if c.is_whitespace() { c } else { '▒' })
                .collect();
            push(
                &mut out,
                &hidden,
                Piece {
                    target: Some(Target::Spoiler),
                    ..Piece::default()
                },
            );
            continue;
        }
        let monospace = has(Style::Monospace);
        let target = if monospace {
            None
        } else if let Some(mention) = mention {
            Some(Target::Mention(mention.service_id.clone()))
        } else {
            url.map(|url| Target::Url(link_target(url)))
        };
        push(
            &mut out,
            &text,
            Piece {
                range: 0..0,
                // A mention stands out as bold.
                bold: has(Style::Bold) || (mention.is_some() && !monospace),
                italic: has(Style::Italic),
                strikethrough: has(Style::Strikethrough),
                monospace,
                target,
            },
        );
    }
    out.shown = SharedString::from(out.text.clone());
    out
}

/// Adds `text` as a piece looking like `look` (its range is set here), joined with the one
/// before it when they look alike and are not links of their own.
fn push(out: &mut Rich, text: &str, look: Piece) {
    let range = out.text.len()..out.text.len() + text.len();
    out.text.push_str(text);
    if look.plain() {
        return;
    }
    if let Some(last) = out.pieces.last_mut()
        && last.range.end == range.start
        && last.same_look(&look)
        && matches!(look.target, None | Some(Target::Spoiler))
    {
        last.range.end = range.end;
        return;
    }
    out.pieces.push(Piece { range, ..look });
}

/// The word around byte `at` in `text` (letters, digits and underscores; or the one character
/// there if it is none): what a double click selects.
pub fn word_at(text: &str, at: usize) -> Range<usize> {
    let at = at.min(text.len());
    let at = (0..=at)
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let Some(here) = text[at..].chars().next() else {
        return at..at;
    };
    if !word(here) {
        return at..at + here.len_utf8();
    }
    let mut start = at;
    for (i, c) in text[..at].char_indices().rev() {
        if !word(c) {
            break;
        }
        start = i;
    }
    let mut end = at;
    for (i, c) in text[at..].char_indices() {
        if !word(c) {
            break;
        }
        end = at + i + c.len_utf8();
    }
    start..end
}

/// The text selected in a message, if any: one message's at a time, as a window has one
/// clipboard and Ctrl+C asks for it.
#[derive(Default)]
pub struct TextSelection {
    /// Whose it is: the message's identity.
    owner: Option<SharedString>,
    anchor: usize,
    range: Range<usize>,
    /// The pointer is down on the text and moves select.
    dragging: bool,
    /// What the pointer went down on, to be clicked if it comes up there without having moved.
    pressed: Option<Target>,
    /// What is selected, as shown.
    text: String,
    /// What gets the keys while text is selected, so Ctrl+C reaches the messages and not the
    /// composer.
    pub focus: Option<FocusHandle>,
}

impl Global for TextSelection {}

impl TextSelection {
    /// The text selected, if there is a selection.
    pub fn selected(cx: &App) -> Option<String> {
        let state = cx.try_global::<TextSelection>()?;
        (!state.range.is_empty()).then(|| state.text.clone())
    }

    /// Whether the message `key` holds the selection.
    pub fn is_in(key: &str, cx: &App) -> bool {
        cx.try_global::<TextSelection>()
            .is_some_and(|state| !state.range.is_empty() && state.owner.as_deref() == Some(key))
    }

    /// Drops the selection; whether there was one.
    pub fn clear(cx: &mut App) -> bool {
        let state = cx.default_global::<TextSelection>();
        let had = !state.range.is_empty();
        state.owner = None;
        state.range = 0..0;
        state.dragging = false;
        state.pressed = None;
        state.text.clear();
        had
    }

    /// Puts the selected text on the clipboard; whether there was some.
    pub fn copy(cx: &mut App) -> bool {
        match Self::selected(cx) {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            None => false,
        }
    }

    fn set(&mut self, key: &SharedString, range: Range<usize>, text: &str) {
        self.owner = Some(key.clone());
        self.text = text.get(range.clone()).unwrap_or_default().to_owned();
        self.range = range;
    }
}

/// How a [`RichText`] looks.
#[derive(Clone, Copy)]
pub struct Look {
    /// Links and mentions.
    pub link: Hsla,
    /// The text, for what is drawn over it (hidden spoilers, code).
    pub ink: Hsla,
    pub selection: Hsla,
}

type OnTarget = Rc<dyn Fn(&Target, &mut Window, &mut App)>;

/// Message text drawn from [`Rich`]. With `interactive` the pointer selects text and clicks
/// links and spoilers; without it (while messages are being picked) it only draws.
pub struct RichText {
    key: SharedString,
    rich: Rc<Rich>,
    look: Look,
    interactive: bool,
    selectable: bool,
    on_target: OnTarget,
    styled: Option<StyledText>,
}

impl RichText {
    /// `key` is the message's identity, for who holds the selection.
    pub fn new(
        key: impl Into<SharedString>,
        rich: Rc<Rich>,
        look: Look,
        on_target: impl Fn(&Target, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            key: key.into(),
            rich,
            look,
            interactive: true,
            selectable: true,
            on_target: Rc::new(on_target),
            styled: None,
        }
    }

    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    fn highlights(&self) -> Vec<(Range<usize>, HighlightStyle)> {
        let look = self.look;
        self.rich
            .pieces
            .iter()
            .map(|piece| {
                let mut style = HighlightStyle::default();
                if piece.bold {
                    style.font_weight = Some(FontWeight::BOLD);
                }
                if piece.italic {
                    style.font_style = Some(FontStyle::Italic);
                }
                if piece.strikethrough {
                    style.strikethrough = Some(StrikethroughStyle {
                        thickness: px(1.),
                        color: None,
                    });
                }
                if piece.monospace {
                    style.background_color = Some(look.ink.opacity(0.12));
                }
                match &piece.target {
                    Some(Target::Spoiler) => {
                        style.color = Some(look.ink.opacity(0.45));
                        style.background_color = Some(look.ink.opacity(0.18));
                    }
                    Some(_) => {
                        style.color = Some(look.link);
                        style.underline = Some(UnderlineStyle {
                            thickness: px(1.),
                            color: None,
                            wavy: false,
                        });
                    }
                    None => {}
                }
                (piece.range.clone(), style)
            })
            .collect()
    }

    fn target_at(pieces: &[Piece], at: usize) -> Option<Target> {
        pieces
            .iter()
            .find(|piece| piece.range.contains(&at))
            .and_then(|piece| piece.target.clone())
    }
}

impl IntoElement for RichText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for RichText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mono = Theme::global(cx).mono_font_family.clone();
        let overrides: Vec<_> = self
            .rich
            .pieces
            .iter()
            .filter(|piece| piece.monospace)
            .map(|piece| (piece.range.clone(), mono.clone()))
            .collect();
        let mut styled = StyledText::new(self.rich.shown.clone())
            .with_highlights(self.highlights())
            .with_font_family_overrides(overrides);
        let (layout, ()) = styled.request_layout(id, inspector_id, window, cx);
        self.styled = Some(styled);
        (layout, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if let Some(styled) = self.styled.as_mut() {
            styled.prepaint(id, inspector_id, bounds, &mut (), window, cx);
        }
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(mut styled) = self.styled.take() else {
            return;
        };
        let layout = styled.layout().clone();
        if let Some(state) = cx.try_global::<TextSelection>()
            && state.owner.as_ref() == Some(&self.key)
            && !state.range.is_empty()
        {
            paint_selection(&layout, state.range.clone(), self.look.selection, window);
        }
        styled.paint(id, inspector_id, bounds, &mut (), &mut (), window, cx);
        if !self.interactive {
            return;
        }
        let rich = self.rich.clone();
        if hitbox.is_hovered(window) {
            let over = layout
                .index_for_position(window.mouse_position())
                .ok()
                .and_then(|at| Self::target_at(&rich.pieces, at));
            let style = match (over.is_some(), self.selectable) {
                (true, _) => CursorStyle::PointingHand,
                (false, true) => CursorStyle::IBeam,
                (false, false) => CursorStyle::Arrow,
            };
            window.set_cursor_style(style, hitbox);
        }
        let selectable = self.selectable;
        let key = self.key.clone();
        // The pointer goes down on the text: on a link it may become a click, anywhere else a
        // selection starts (a double click takes the word, a triple click all of it).
        window.on_mouse_event({
            let (rich, key, hitbox, layout) =
                (rich.clone(), key.clone(), hitbox.clone(), layout.clone());
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                    || picks(event.modifiers, cx)
                {
                    return;
                }
                let at = layout.index_for_position(event.position);
                let pressed = at.ok().and_then(|at| Self::target_at(&rich.pieces, at));
                let at = at.unwrap_or_else(|nearest| nearest);
                let focus = {
                    let state = cx.default_global::<TextSelection>();
                    state.dragging = true;
                    state.pressed = pressed.clone();
                    state.anchor = at;
                    let range = match event.click_count {
                        _ if !selectable => 0..0,
                        1 => 0..0,
                        2 => word_at(&rich.text, at),
                        _ => 0..rich.text.len(),
                    };
                    if range.is_empty() {
                        state.owner = Some(key.clone());
                        state.range = 0..0;
                        state.text.clear();
                    } else {
                        state.pressed = None;
                        state.set(&key, range, &rich.text);
                    }
                    state.focus.clone()
                };
                if let Some(focus) = focus {
                    window.focus(&focus, cx);
                }
                window.refresh();
                cx.stop_propagation();
            }
        });
        window.on_mouse_event({
            let (rich, key, layout) = (rich.clone(), key.clone(), layout.clone());
            move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture
                    || event.pressed_button != Some(MouseButton::Left)
                {
                    return;
                }
                let Some(state) = cx.try_global::<TextSelection>() else {
                    return;
                };
                if !state.dragging || state.owner.as_ref() != Some(&key) || !selectable {
                    return;
                }
                let at = layout
                    .index_for_position(event.position)
                    .unwrap_or_else(|nearest| nearest);
                let anchor = state.anchor;
                let range = anchor.min(at)..anchor.max(at);
                let state = cx.default_global::<TextSelection>();
                if range != state.range {
                    if !range.is_empty() {
                        // It moved: not a click any more.
                        state.pressed = None;
                    }
                    state.set(&key, range, &rich.text);
                    window.refresh();
                }
            }
        });
        window.on_mouse_event({
            let (key, layout, rich) = (key, layout, rich);
            let on_target = self.on_target.clone();
            let clicked = Rc::new(Cell::new(false));
            move |event: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                    return;
                }
                let Some(state) = cx.try_global::<TextSelection>() else {
                    return;
                };
                if !state.dragging || state.owner.as_ref() != Some(&key) {
                    return;
                }
                let pressed = state.pressed.clone();
                let selected = !state.range.is_empty();
                let state = cx.default_global::<TextSelection>();
                state.dragging = false;
                state.pressed = None;
                if selected {
                    // Selected text can be pasted with a middle click where there is such a
                    // clipboard.
                    let text = state.text.clone();
                    cx.write_to_primary(ClipboardItem::new_string(text));
                    return;
                }
                // Up where it went down, on the same link: a click.
                let over = layout
                    .index_for_position(event.position)
                    .ok()
                    .and_then(|at| Self::target_at(&rich.pieces, at));
                if let (Some(pressed), Some(over)) = (pressed, over)
                    && pressed == over
                    && !clicked.replace(true)
                {
                    on_target(&pressed, window, cx);
                }
                window.refresh();
            }
        });
    }
}

/// Whether a click with `modifiers` picks the message instead of acting on what is under it.
pub fn picks(modifiers: Modifiers, cx: &App) -> bool {
    modifiers.secondary(cx)
}

fn paint_selection(layout: &TextLayout, range: Range<usize>, color: Hsla, window: &mut Window) {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return;
    };
    for bounds in selection_rows(start, end, layout.bounds(), layout.line_height()) {
        window.paint_quad(PaintQuad {
            bounds,
            background: color.into(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
            border_color: transparent_black(),
            border_style: BorderStyle::default(),
        });
    }
}

/// The rectangles a selection from `start` to `end` covers: the rest of the first line, whole
/// lines, and the start of the last.
fn selection_rows(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        )];
    }
    let mut rows = vec![Bounds::from_corners(
        start,
        Point::new(bounds.right(), start.y + line_height),
    )];
    if end.y > start.y + line_height {
        rows.push(Bounds::from_corners(
            Point::new(bounds.left(), start.y + line_height),
            Point::new(bounds.right(), end.y),
        ));
    }
    rows.push(Bounds::from_corners(
        Point::new(bounds.left(), end.y),
        Point::new(end.x, end.y + line_height),
    ));
    rows
}

#[cfg(test)]
mod tests {
    use b33p_core::Style;

    use super::*;

    #[test]
    fn the_links_of_a_text_are_found_as_they_would_open() {
        assert_eq!(
            links("see https://a.org/x and www.b.org, or c.io!", 5),
            ["https://a.org/x", "https://www.b.org", "https://c.io"]
        );
        assert_eq!(links("https://a.org https://b.org", 1), ["https://a.org"]);
        assert!(links("nothing here", 3).is_empty());
    }

    fn range(start: usize, end: usize, style: Style) -> StyleRange {
        StyleRange { start, end, style }
    }

    fn shown(rich: &Rich, piece: usize) -> &str {
        &rich.text[rich.pieces[piece].range.clone()]
    }

    /// What people type is shown as typed: no markup of any kind is read into it.
    #[test]
    fn plain_text_is_shown_as_typed() {
        let body = "*not bold* <b>nor this</b> [x](y) `no`\n\n  indented";
        let rich = build(body, &[], &[], false);
        assert_eq!(rich.text, body);
        assert!(rich.pieces.is_empty());
    }

    #[test]
    fn styles_become_pieces() {
        let rich = build(
            "a bold and italic b",
            &[range(2, 6, Style::Bold), range(11, 17, Style::Italic)],
            &[],
            false,
        );
        assert_eq!(rich.text, "a bold and italic b");
        assert_eq!(rich.pieces.len(), 2);
        assert_eq!(shown(&rich, 0), "bold");
        assert!(rich.pieces[0].bold && !rich.pieces[0].italic);
        assert_eq!(shown(&rich, 1), "italic");
        assert!(rich.pieces[1].italic);
    }

    #[test]
    fn overlapping_styles_combine() {
        let rich = build(
            "abcdef",
            &[range(0, 4, Style::Bold), range(2, 6, Style::Strikethrough)],
            &[],
            false,
        );
        assert_eq!(rich.pieces.len(), 3);
        assert!(rich.pieces[0].bold && !rich.pieces[0].strikethrough);
        assert!(rich.pieces[1].bold && rich.pieces[1].strikethrough);
        assert!(!rich.pieces[2].bold && rich.pieces[2].strikethrough);
    }

    #[test]
    fn spoilers_are_hidden_until_revealed() {
        let styles = [range(6, 12, Style::Spoiler)];
        let hidden = build("a big secret", &styles, &[], false);
        assert_eq!(hidden.text, "a big ▒▒▒▒▒▒");
        assert_eq!(hidden.pieces[0].target, Some(Target::Spoiler));
        let shown = build("a big secret", &styles, &[], true);
        assert_eq!(shown.text, "a big secret");
        assert!(shown.pieces.is_empty());
        // Spaces stay, so the words keep their shape and the text its wrapping.
        let words = build("ab cd", &[range(0, 5, Style::Spoiler)], &[], false);
        assert_eq!(words.text, "▒▒ ▒▒");
    }

    #[test]
    fn web_addresses_are_links_without_their_trailing_punctuation() {
        let body = "see https://example.org/a?b=1. and www.example.com";
        let rich = build(body, &[], &[], false);
        assert_eq!(rich.text, body);
        assert_eq!(rich.pieces.len(), 2);
        assert_eq!(shown(&rich, 0), "https://example.org/a?b=1");
        assert_eq!(
            rich.pieces[0].target,
            Some(Target::Url("https://example.org/a?b=1".into()))
        );
        // A bare host name leads to the page with a scheme.
        assert_eq!(
            rich.pieces[1].target,
            Some(Target::Url("https://www.example.com".into()))
        );
    }

    /// A style cutting an address in two doesn't change where either half leads.
    #[test]
    fn a_styled_part_of_an_address_still_leads_to_all_of_it() {
        let body = "https://example.org/path";
        let rich = build(body, &[range(8, 15, Style::Bold)], &[], false);
        let targets: Vec<_> = rich.pieces.iter().map(|p| p.target.clone()).collect();
        assert_eq!(rich.pieces.len(), 3);
        assert!(
            targets
                .iter()
                .all(|t| *t == Some(Target::Url(body.to_owned())))
        );
        assert!(rich.pieces[1].bold);
    }

    #[test]
    fn monospace_is_never_a_link() {
        let rich = build(
            "https://example.org",
            &[range(0, 19, Style::Monospace)],
            &[],
            false,
        );
        assert_eq!(rich.pieces.len(), 1);
        assert!(rich.pieces[0].monospace);
        assert_eq!(rich.pieces[0].target, None);
    }

    #[test]
    fn mentions_are_bold_links_to_the_person() {
        let body = "hi @Alice!";
        let rich = build(
            body,
            &[],
            &[Mention {
                start: 3,
                end: 9,
                service_id: "alice-id".into(),
            }],
            false,
        );
        assert_eq!(rich.pieces.len(), 1);
        assert_eq!(shown(&rich, 0), "@Alice");
        assert!(rich.pieces[0].bold);
        assert_eq!(
            rich.pieces[0].target,
            Some(Target::Mention("alice-id".into()))
        );
    }

    #[test]
    fn ranges_that_do_not_fit_are_ignored() {
        let body = "héllo";
        // Past the end, backwards, empty, and inside the two bytes of the "é".
        let styles = [
            range(0, 99, Style::Bold),
            range(3, 1, Style::Bold),
            range(2, 2, Style::Bold),
            range(2, 3, Style::Bold),
        ];
        let mentions = [Mention {
            start: 2,
            end: 3,
            service_id: "x".into(),
        }];
        let rich = build(body, &styles, &mentions, false);
        assert_eq!(rich.text, body);
        assert!(rich.pieces.is_empty());
    }

    #[test]
    fn line_breaks_are_kept_except_at_the_end() {
        let rich = build("one\ntwo\r\n\n", &[], &[], false);
        assert_eq!(rich.text, "one\ntwo");
        assert_eq!(build("a\n\nb", &[], &[], false).text, "a\n\nb");
    }

    #[test]
    fn pieces_cover_what_they_say() {
        let body = "x *y* https://a.org z";
        let rich = build(body, &[range(2, 5, Style::Italic)], &[], false);
        let mut at = 0;
        for piece in &rich.pieces {
            assert!(piece.range.start >= at);
            assert!(rich.text.is_char_boundary(piece.range.start));
            assert!(rich.text.is_char_boundary(piece.range.end));
            at = piece.range.end;
        }
        assert!(at <= rich.text.len());
    }

    #[test]
    fn a_double_click_takes_the_word() {
        let text = "hello wörld, again";
        assert_eq!(&text[word_at(text, 1)], "hello");
        assert_eq!(&text[word_at(text, 4)], "hello");
        assert_eq!(&text[word_at(text, 8)], "wörld");
        // On punctuation or a space: that character.
        assert_eq!(&text[word_at(text, 12)], ",");
        assert_eq!(&text[word_at(text, 5)], " ");
        assert_eq!(word_at("", 0), 0..0);
        // In the middle of a character: its word.
        assert_eq!(&text[word_at(text, 8 + 1)], "wörld");
    }

    #[test]
    fn a_selection_over_lines_covers_whole_lines_between() {
        use gpui_kit::{point, size};
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(100.), px(100.)));
        let rows = selection_rows(
            point(px(40.), px(20.)),
            point(px(30.), px(80.)),
            bounds,
            px(20.),
        );
        assert_eq!(
            rows,
            vec![
                Bounds::from_corners(point(px(40.), px(20.)), point(px(110.), px(40.))),
                Bounds::from_corners(point(px(10.), px(40.)), point(px(110.), px(80.))),
                Bounds::from_corners(point(px(10.), px(80.)), point(px(30.), px(100.))),
            ]
        );
    }
}
