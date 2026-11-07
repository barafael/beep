//! The bottom of the open chat: where the message is written, and everything around it. Above
//! the field come what offers to complete a mention, the edit and reply banners and the files
//! waiting to go out; where a chat can't be written to, bars about it (a request, an
//! invitation, a blocked chat, ...) stand in for the field.

use std::path::PathBuf;
use std::rc::Rc;

use b33p_core::state::Update;
use b33p_core::{ConversationId, GroupEdit, RequestResponse, Trust};
use b33p_platform::desktop::{Pick, ask_paste, ask_pick};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Div, Entity, ExternalPaths, Focusable as _, FontWeight, IntoElement,
    KeystrokeEvent, MouseButton, Render, Stateful, Subscription, Window, deferred, div, px,
    relative,
};

use crate::emoji::{self, EmojiPicker};
use crate::shared::{Confirm, Shared};
use crate::stickers::{self, StickerPicker};
use crate::theme::{self, Tokens};
use crate::widgets::{icon, icon_button};

/// What floats up from the composer's buttons, if anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Popup {
    #[default]
    None,
    Emoji,
    Stickers,
}

pub struct ComposerView {
    shared: Rc<Shared>,
    pub input: Entity<TextareaState>,
    emoji: Entity<EmojiPicker>,
    stickers: Entity<StickerPicker>,
    /// The chat whose draft the field shows, and whether it was an edit that it shows.
    showing: Option<(ConversationId, bool)>,
    /// Whether the field was drawn the last time: it is not where a bar says why not.
    field_shown: bool,
    /// The mention Tab and Enter pick, among the candidates.
    mention: usize,
    /// The popup as last drawn, to tell when one closed.
    popup_drawn: Popup,
    _subscriptions: Vec<Subscription>,
}

impl ComposerView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .placeholder("Message")
                .submit_on_enter(true)
        });
        let emoji = cx.new(|cx| EmojiPicker::new(shared.clone(), window, cx));
        let stickers = cx.new(|_| StickerPicker::new(shared.clone()));
        let mut subscriptions = vec![
            cx.subscribe_in(&input, window, Self::input_event),
            cx.subscribe_in(
                &emoji,
                window,
                |this, _, picked: &emoji::Picked, window, cx| {
                    this.insert(&picked.0, window, cx);
                },
            ),
            cx.subscribe_in(
                &stickers,
                window,
                |this, _, picked: &stickers::Picked, window, cx| {
                    this.send_sticker(&picked.pack, picked.sticker, window, cx);
                },
            ),
        ];
        // The field's keys are bound to its own actions, which run before any key listener of
        // ours; the mention list and Up in an empty field need the keys first.
        let this = cx.weak_entity();
        subscriptions.push(cx.intercept_keystrokes(move |event, window, cx| {
            this.update(cx, |this, cx| this.intercept(event, window, cx))
                .ok();
        }));
        *shared.composer.borrow_mut() = Some(cx.weak_entity());
        Self {
            shared,
            input,
            emoji,
            stickers,
            showing: None,
            field_shown: true,
            mention: 0,
            popup_drawn: Popup::None,
            _subscriptions: subscriptions,
        }
    }

    fn input_event(
        &mut self,
        _: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => self.draft_edited(cx),
            InputEvent::PressEnter { shift: false, .. } => {
                self.send(window, cx);
            }
            // The field's outline follows the focus. Writing drops the selection of text in the
            // messages, so that Ctrl+C there copies the composer's own.
            InputEvent::Focus => {
                crate::text::TextSelection::clear(cx);
                cx.notify();
            }
            InputEvent::Blur => cx.notify(),
            _ => {}
        }
    }

    /// The field's text changed: the state keeps it as the chat's draft (and tells the others
    /// we are typing).
    fn draft_edited(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        let command = self.shared.chat.borrow_mut().draft_changed(&text);
        if let Some(command) = command {
            self.shared.commands.send(command);
        }
        self.mention = 0;
        cx.notify();
    }

    /// Puts `text` in the field, with the caret after it, as when it is typed.
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        fill(&self.input, text, window, cx);
    }

    /// Puts the caret in the field.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Puts `text` where the caret is, in place of what is selected, and the caret after it,
    /// as an emoji from the picker is.
    fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_owned();
        self.input
            .update(cx, |input, cx| input.replace(text, window, cx));
        self.draft_edited(cx);
        self.focus(window, cx);
    }

    /// Sends what is written: during an edit that is the edit, which the state keeps out of
    /// the chat's draft. The field is then empty, or after an edit holds what was being written
    /// before it.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.input.read(cx).value().to_string();
        let sent = self.shared.act(cx, |chat| chat.compose(&text));
        if sent {
            let draft = self.shared.chat.borrow().draft().to_owned();
            self.set_text(&draft, window, cx);
            self.shared.refresh(Update::default(), cx);
        }
        sent
    }

    fn send_sticker(
        &mut self,
        pack: &str,
        sticker: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shared
            .command(cx, |chat| chat.send_sticker(pack, sticker));
        self.shared.ui.borrow_mut().composer_popup = Popup::None;
        self.focus(window, cx);
        cx.notify();
    }

    /// Opens `popup`, or closes it if it is the one open.
    fn toggle(&mut self, popup: Popup, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.shared.ui.borrow().composer_popup;
        let next = if open == popup { Popup::None } else { popup };
        self.shared.ui.borrow_mut().composer_popup = next;
        match next {
            Popup::Emoji => self
                .emoji
                .update(cx, |picker, cx| picker.opened(window, cx)),
            Popup::Stickers => self.stickers.update(cx, |picker, cx| picker.opened(cx)),
            Popup::None => {}
        }
        cx.notify();
    }

    /// Asks the desktop for files and stages them for the chat that is open now.
    fn attach(&mut self, cx: &mut Context<Self>) {
        let chat = self.shared.chat.borrow().selected().map(|c| c.id.clone());
        let Some(picked) = ask_pick(&self.shared.desktop, "Attach files", Pick::Files) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Ok(files) = picked.await else {
                return;
            };
            if files.is_empty() {
                return;
            }
            this.update(cx, |this, cx| this.stage_for(chat, files, cx))
                .ok();
        })
        .detach();
    }

    /// Stages `files`, if the chat they were meant for is still the one open: not another one
    /// opened while the dialog was up.
    fn stage_for(
        &mut self,
        chat: Option<ConversationId>,
        files: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let now = self.shared.chat.borrow().selected().map(|c| c.id.clone());
        if now.is_some() && now == chat {
            self.shared.edit(cx, |chat| chat.stage_files(files));
        }
    }

    /// Ctrl+V or Paste: files or a picture on the clipboard are staged to go out, anything else
    /// is text, which goes in the field as the desktop would have it (`text`).
    fn paste(&mut self, text: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let chat = self.shared.chat.borrow().selected().map(|c| c.id.clone());
        let Some(pasted) = ask_paste(&self.shared.desktop) else {
            if let Some(text) = text {
                self.insert(&text, window, cx);
            }
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let Ok(files) = pasted.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| match files {
                Some(files) => this.stage_for(chat, files, cx),
                None => {
                    if let Some(text) = text {
                        this.insert(&text, window, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// The names of the people the draft's "@..." could mean.
    fn mention_candidates(&self, cx: &App) -> Vec<String> {
        let text = self.input.read(cx).value();
        let chat = self.shared.chat.borrow();
        chat.mention_candidates(&text)
            .iter()
            .map(|person| person.name.clone())
            .collect()
    }

    fn pick_mention(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        let draft = self.shared.chat.borrow_mut().pick_mention(&text, index);
        if let Some(draft) = draft {
            self.set_text(&draft, window, cx);
            self.mention = 0;
            self.shared.refresh(Update::default(), cx);
        }
    }

    /// Keys the field would take, but that mean something else here: Tab, Enter and the arrows
    /// while mentions are offered, and Up in an empty field, which edits the last message.
    fn intercept(&mut self, event: &KeystrokeEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;
        if m.control || m.alt || m.shift || m.platform || m.function {
            return;
        }
        if !self.input.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        let key = keystroke.key.as_str();
        let candidates = self.mention_candidates(cx).len();
        let handled = if candidates > 0 {
            let at = self.mention.min(candidates - 1);
            match key {
                "tab" | "enter" => {
                    self.pick_mention(at, window, cx);
                    true
                }
                "down" => {
                    self.mention = (at + 1) % candidates;
                    cx.notify();
                    true
                }
                "up" => {
                    self.mention = (at + candidates - 1) % candidates;
                    cx.notify();
                    true
                }
                _ => false,
            }
        } else if key == "up" && self.input.read(cx).value().is_empty() {
            let text = self.shared.chat.borrow_mut().edit_last();
            match text {
                Some(text) => {
                    self.set_text(&text, window, cx);
                    self.shared.refresh(Update::default(), cx);
                    true
                }
                None => false,
            }
        } else {
            false
        };
        if handled {
            cx.stop_propagation();
        }
    }
}

/// Puts `text` in `input`, with the caret after it.
fn fill(input: &Entity<TextareaState>, text: &str, window: &mut Window, cx: &mut App) {
    let end = text.len();
    let text = text.to_owned();
    input.update(cx, |input, cx| {
        input.set_value(text, window, cx);
        input.set_selected_range(end..end, cx);
    });
}

/// What the bottom of the chat shows of the open chat, copied out of the state.
struct Situation {
    id: ConversationId,
    title: String,
    subtitle: String,
    request: bool,
    blocked: bool,
    left: bool,
    invited: bool,
    requested: bool,
    safety_changed: bool,
    editing: bool,
    reply: Option<(String, String)>,
    staged: Vec<String>,
}

impl Situation {
    fn of(chat: &b33p_core::state::ChatState) -> Option<Self> {
        let c = chat.selected()?;
        Some(Self {
            id: c.id.clone(),
            title: c.title.clone(),
            subtitle: c.subtitle.clone(),
            request: chat.is_request(c),
            blocked: c.settings.blocked,
            left: c.left,
            invited: c.invited,
            requested: c.requested,
            safety_changed: c.trust == Trust::Changed,
            editing: chat.editing().is_some(),
            reply: chat
                .replying()
                .map(|quote| (quote.author.clone(), quote.text.clone())),
            staged: chat
                .staged()
                .iter()
                .map(|path| {
                    path.file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
        })
    }

    /// The field is where a message is written, unless a bar says why not.
    fn shows_field(&self) -> bool {
        !(self.request || self.blocked || self.left || self.safety_changed)
    }
}

impl Render for ComposerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let Some(situation) = Situation::of(&self.shared.chat.borrow()) else {
            return div().into_any_element();
        };
        let draft = self.shared.chat.borrow().draft().to_owned();
        let now = Some((situation.id.clone(), situation.editing));
        // Another chat's draft, or an edit that ended without being sent (its message was
        // deleted, here or elsewhere): the field follows the state.
        if now != self.showing {
            let editing_ended = matches!(
                (&self.showing, &now),
                (Some((a, true)), Some((b, false))) if a == b
            );
            let other_chat = self.showing.as_ref().map(|s| &s.0) != now.as_ref().map(|n| &n.0);
            if other_chat || editing_ended {
                fill(&self.input, &draft, window, cx);
            }
            let editing = situation.editing;
            self.input.update(cx, |input, cx| {
                input.set_placeholder(if editing { "Edit message" } else { "Message" }, window, cx)
            });
            // A chat opens ready to write in, wherever it was opened from.
            if other_chat && situation.shows_field() {
                cx.defer_in(window, |this, window, cx| this.focus(window, cx));
            }
            self.showing = now;
        }
        // A field that comes back (a request accepted, a block lifted) has the caret again.
        let shows_field = situation.shows_field();
        if shows_field && !self.field_shown {
            cx.defer_in(window, |this, window, cx| this.focus(window, cx));
        }
        self.field_shown = shows_field;
        let popup = self.shared.ui.borrow().composer_popup;
        // Esc or a click outside closed one: back to writing.
        if popup == Popup::None && self.popup_drawn != Popup::None && situation.shows_field() {
            cx.defer_in(window, |this, window, cx| this.focus(window, cx));
        }
        self.popup_drawn = popup;

        let candidates = self.mention_candidates(cx);
        let can_send = self
            .shared
            .chat
            .borrow()
            .can_compose(&self.input.read(cx).value());
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);

        let mut bottom = div().flex_none().flex().flex_col();
        if !candidates.is_empty() {
            bottom = bottom.child(self.mentions(&candidates, &t, cx));
        }
        if situation.editing {
            bottom = bottom.child(self.editing_bar(&t));
        }
        if let Some((author, text)) = &situation.reply {
            bottom = bottom.child(self.reply_bar(author, text, &t));
        }
        if !situation.staged.is_empty() {
            bottom = bottom.child(self.staged_row(&situation.staged, &t));
        }
        for bar in self.banners(&situation, &t) {
            bottom = bottom.child(bar);
        }
        if shows_field {
            bottom = bottom.child(self.row(popup, can_send, focused, situation.editing, &t, cx));
        } else if focused {
            // The caret was in the field when a bar took its place. A field that is not in the
            // frame can't be told it lost the focus, so its caret would go on blinking, and the
            // window on being drawn again for it, with nobody to see: it stays one more frame,
            // without a size, while the focus is taken from it.
            bottom = bottom.child(div().h(px(0.)).overflow_hidden().child(self.row(
                popup,
                can_send,
                focused,
                situation.editing,
                &t,
                cx,
            )));
            cx.defer_in(window, |_, window, cx| window.blur(cx));
        }
        bottom.into_any_element()
    }
}

/// The bars that say why a chat can't be written to, or what to do first.
impl ComposerView {
    fn banners(&self, s: &Situation, t: &Tokens) -> Vec<AnyElement> {
        let shared = &self.shared;
        let id = &s.id;
        let mut bars = Vec::new();
        if s.request {
            let answer = |response: RequestResponse| {
                let shared = shared.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| match response {
                    RequestResponse::Accept => {
                        shared.act(cx, |chat| {
                            let id = chat.selected().map(|c| c.id.clone());
                            (
                                id.and_then(|id| chat.respond(&id, RequestResponse::Accept)),
                                Update::reset(),
                            )
                        });
                    }
                    RequestResponse::Block => {
                        shared.ui.borrow_mut().confirm = Confirm::BlockRequest;
                        cx.refresh_windows();
                    }
                    RequestResponse::Delete => {
                        shared.ui.borrow_mut().confirm = Confirm::DeleteRequest;
                        cx.refresh_windows();
                    }
                }
            };
            bars.push(banner(
                "request",
                "message-circle-question-mark",
                format!(
                    "{} wants to message you. They won't know you've seen their messages until \
                     you accept.",
                    s.title
                ),
                vec![
                    Button::new("request-block")
                        .label("Block…")
                        .on_click(answer(RequestResponse::Block))
                        .into_any_element(),
                    Button::new("request-delete")
                        .label("Delete…")
                        .on_click(answer(RequestResponse::Delete))
                        .into_any_element(),
                    Button::new("request-accept")
                        .primary()
                        .label("Accept")
                        .on_click(answer(RequestResponse::Accept))
                        .into_any_element(),
                ],
                t,
            ));
        }
        if s.invited {
            let respond = |edit: fn() -> GroupEdit| {
                let (shared, id) = (shared.clone(), id.clone());
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    shared.command(cx, |chat| chat.edit_group(&id, edit()));
                }
            };
            bars.push(banner(
                "invited",
                "users",
                "You're invited to join this group. Its members see you once you join.".into(),
                vec![
                    Button::new("invitation-decline")
                        .label("Decline")
                        .on_click(respond(|| GroupEdit::DeclineInvitation))
                        .into_any_element(),
                    Button::new("invitation-accept")
                        .primary()
                        .label("Accept")
                        .on_click(respond(|| GroupEdit::AcceptInvitation))
                        .into_any_element(),
                ],
                t,
            ));
        }
        if s.requested {
            let (shared, id) = (shared.clone(), id.clone());
            bars.push(banner(
                "requested",
                "hourglass",
                "You asked to join this group. An admin has to let you in.".into(),
                vec![
                    Button::new("take-request-back")
                        .label("Take request back")
                        .on_click(move |_, _, cx| {
                            shared.command(cx, |chat| {
                                chat.edit_group(&id, GroupEdit::CancelJoinRequest)
                            });
                        })
                        .into_any_element(),
                ],
                t,
            ));
        }
        if s.left && !s.invited && !s.requested {
            bars.push(banner("left", "log-out", s.subtitle.clone(), Vec::new(), t));
        }
        if s.safety_changed {
            let (details, keep) = (shared.clone(), shared.clone());
            let id = id.clone();
            bars.push(banner(
                "safety-changed",
                "shield-alert",
                format!(
                    "Your safety number with {} changed since you verified it. Compare it \
                     again, or continue without.",
                    s.title
                ),
                vec![
                    Button::new("show-safety-number")
                        .label("Show safety number")
                        .on_click(move |_, _, cx| details.command(cx, |chat| chat.open_details()))
                        .into_any_element(),
                    Button::new("continue-unverified")
                        .label("Continue")
                        .on_click(move |_, _, cx| {
                            keep.command(cx, |chat| chat.confirm_safety_number(&id))
                        })
                        .into_any_element(),
                ],
                t,
            ));
        }
        if s.blocked {
            let (shared, id) = (shared.clone(), id.clone());
            bars.push(banner(
                "blocked",
                "ban",
                "You blocked this chat. You won't see new messages from it.".into(),
                vec![
                    Button::new("unblock")
                        .label("Unblock")
                        .on_click(move |_, _, cx| {
                            shared.chat_action(cx, &id, |chat, id| chat.block(id, false))
                        })
                        .into_any_element(),
                ],
                t,
            ));
        }
        bars
    }

    /// People to mention, as the draft's "@..." names them; the highlighted one is what Tab or
    /// Enter picks.
    fn mentions(&self, names: &[String], t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let at = self.mention.min(names.len().saturating_sub(1));
        div()
            .flex_none()
            .flex()
            .flex_col()
            .py(px(6.))
            .px(px(theme::SPACE_M))
            .bg(t.surface_raised)
            .children(names.iter().enumerate().map(|(index, name)| {
                div()
                    .id(("mention", index))
                    .h(px(30.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(theme::RADIUS_S))
                    .cursor_pointer()
                    .aria_label(format!("Mention {name}"))
                    .when(index == at, |row| row.bg(t.selected))
                    .hover(|row| row.bg(t.pressed))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_mention(index, window, cx);
                        this.focus(window, cx);
                    }))
                    .child(format!("@{name}"))
            }))
            .into_any_element()
    }

    fn editing_bar(&self, t: &Tokens) -> AnyElement {
        let shared = self.shared.clone();
        quote_bar(
            "editing-bar",
            "square-pen",
            div()
                .flex_1()
                .flex()
                .items_center()
                .text_size(px(theme::BODY * theme::BODY_SMALL))
                .font_weight(FontWeight::SEMIBOLD)
                .child("Editing message")
                .into_any_element(),
            icon_button("cancel-edit", "x", "Stop editing", move |_, _, cx| {
                shared.edit(cx, |chat| chat.cancel_edit())
            }),
            t,
        )
    }

    fn reply_bar(&self, author: &str, text: &str, t: &Tokens) -> AnyElement {
        let shared = self.shared.clone();
        quote_bar(
            "reply-bar",
            "reply",
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::BODY * theme::SMALL))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("Replying to {author}")),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::BODY * theme::BODY_SMALL))
                        .text_color(t.text_secondary)
                        .child(text.to_owned()),
                )
                .into_any_element(),
            icon_button("cancel-reply", "x", "Stop replying", move |_, _, cx| {
                shared.edit(cx, |chat| chat.cancel_reply())
            }),
            t,
        )
    }

    /// Files waiting to go out with the next message, each with a way to take it back.
    fn staged_row(&self, names: &[String], t: &Tokens) -> AnyElement {
        div()
            .flex_none()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(6.))
            .px(px(theme::SPACE_M))
            .pt(px(theme::SPACE_S))
            .children(names.iter().enumerate().map(|(index, name)| {
                let shared = self.shared.clone();
                div()
                    .id(("staged", index))
                    .h(px(28.))
                    .max_w(px(260.))
                    .pl(px(10.))
                    .pr(px(2.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .rounded_full()
                    .border_1()
                    .border_color(t.divider)
                    .bg(t.surface_raised)
                    .child(icon("paperclip").size(px(theme::ICON_SMALL)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::BODY * theme::BODY_SMALL))
                            .child(name.clone()),
                    )
                    .child(
                        Button::new(("unstage", index))
                            .ghost()
                            .xsmall()
                            .icon(icon("x"))
                            .tooltip(format!("Remove {name}"))
                            .accessibility_label(format!("Remove {name}"))
                            .on_click(move |_, _, cx| shared.edit(cx, |chat| chat.unstage(index))),
                    )
            }))
            .into_any_element()
    }

    /// The buttons, the field and the send button.
    fn row(
        &self,
        popup: Popup,
        can_send: bool,
        focused: bool,
        editing: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tool = |id: &'static str, name: &'static str, label: &'static str| {
            Button::new(id)
                .ghost()
                .small()
                .icon(icon(name))
                .tooltip(label)
                .accessibility_label(label)
        };
        let popover =
            |id: &'static str, button: Button, card: Option<(AnyElement, f32, Stateful<Div>)>| {
                div().relative().flex_none().child(button).when_some(
                    card,
                    |wrapper, (content, width, backdrop)| {
                        wrapper
                            // Everything else answers the pointer first otherwise: a click outside
                            // closes the popup and is not otherwise acted on.
                            .child(deferred(backdrop).with_priority(1))
                            .child(
                                deferred(
                                    div()
                                        .id(id)
                                        .absolute()
                                        .bottom(relative(1.))
                                        .left_0()
                                        .mb(px(theme::SPACE_S))
                                        .w(px(width))
                                        .occlude()
                                        .rounded(px(theme::RADIUS_L))
                                        .border_1()
                                        .border_color(t.divider)
                                        .bg(t.surface_raised)
                                        .shadow_lg()
                                        .overflow_hidden()
                                        .child(content),
                                )
                                .with_priority(2),
                            )
                    },
                )
            };
        let stickers = popover(
            "stickers-popup",
            tool("stickers", "sticker", "Stickers").on_click(
                cx.listener(|this, _, window, cx| this.toggle(Popup::Stickers, window, cx)),
            ),
            (popup == Popup::Stickers).then(|| {
                (
                    self.stickers.clone().into_any_element(),
                    stickers::WIDTH,
                    self.backdrop("stickers-popup", cx),
                )
            }),
        );
        let emoji = popover(
            "emoji-popup",
            tool("emoji", "face-slightly-smiling", "Emoji")
                .on_click(cx.listener(|this, _, window, cx| this.toggle(Popup::Emoji, window, cx))),
            (popup == Popup::Emoji).then(|| {
                (
                    self.emoji.clone().into_any_element(),
                    emoji::WIDTH,
                    self.backdrop("emoji-popup", cx),
                )
            }),
        );
        let paste = cx.weak_entity();
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_end()
            .gap(px(6.))
            .p(px(theme::SPACE_M))
            .child(
                tool("attach", "paperclip", "Attach files")
                    .on_click(cx.listener(|this, _, _, cx| this.attach(cx))),
            )
            .child(stickers)
            .child(emoji)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(theme::SPACE_M))
                    .py(px(theme::SPACE_S))
                    .rounded(px(theme::RADIUS_M))
                    .border_1()
                    .border_color(if focused { t.accent } else { t.divider })
                    .bg(t.field)
                    .child(
                        Textarea::new(&self.input)
                            .appearance(false)
                            .bordered(false)
                            .on_paste(move |item, window, cx| {
                                // Files and pictures are the desktop's to find; the text is
                                // what this clipboard item holds.
                                let text = item.text();
                                paste
                                    .update(cx, |this, cx| this.paste(text, window, cx))
                                    .is_ok()
                            }),
                    ),
            )
            .child(
                Button::new("send")
                    .primary()
                    .icon(icon(if editing { "check" } else { "send" }))
                    .tooltip(if editing { "Save" } else { "Send" })
                    .accessibility_label(if editing { "Save" } else { "Send" })
                    .disabled(!can_send)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.send(window, cx);
                        this.focus(window, cx);
                    })),
            )
            .into_any_element()
    }

    /// Covers the window, to catch the click that closes a popup.
    fn backdrop(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id((id, 1usize))
            .absolute()
            .top(px(-4000.))
            .left(px(-4000.))
            .w(px(8000.))
            .h(px(8000.))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.toggle(Popup::None, window, cx)),
            )
    }
}

/// A bar over the composer about the chat: what is up, and what can be done about it.
fn banner(
    id: &'static str,
    name: &'static str,
    text: String,
    buttons: Vec<AnyElement>,
    t: &Tokens,
) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_M))
        .p(px(theme::SPACE_M))
        .pl(px(theme::SPACE_L))
        .bg(t.surface_raised)
        .border_t_1()
        .border_color(t.divider)
        .aria_label(text.clone())
        .child(icon(name).size(px(theme::ICON)))
        .child(div().flex_1().min_w_0().child(text))
        .when(!buttons.is_empty(), |bar| {
            bar.child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .gap(px(theme::SPACE_S))
                    .children(buttons),
            )
        })
        .into_any_element()
}

/// The bar of a reply or an edit: an accent stripe, an icon, what it says and a way to stop.
fn quote_bar(
    id: &'static str,
    name: &'static str,
    content: AnyElement,
    stop: Button,
    t: &Tokens,
) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_S))
        .pl(px(theme::SPACE_L))
        .pr(px(theme::SPACE_S))
        .py(px(6.))
        .bg(t.surface_raised)
        .child(
            div()
                .flex_none()
                .w(px(3.))
                .h(px(28.))
                .rounded_full()
                .bg(t.accent),
        )
        .child(icon(name).size(px(theme::ICON_SMALL)).text_color(t.accent))
        .child(content)
        .child(stop)
        .into_any_element()
}

/// Makes `root` take files dragged onto it from elsewhere as files to attach, and show that
/// it will (an outline and "Drop to attach") while they are over it. `root` needs the group
/// name "conversation".
pub fn accepting_files(root: Stateful<Div>, shared: &Rc<Shared>, t: &Tokens) -> Stateful<Div> {
    let shared = shared.clone();
    root.on_drop::<ExternalPaths>(move |paths, _, cx| {
        let files = paths.paths().to_vec();
        shared.edit(cx, |chat| chat.stage_files(files));
    })
    .child(
        div()
            .absolute()
            .inset_0()
            .invisible()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(theme::RADIUS_L))
            .border_2()
            .border_color(t.accent)
            .bg(t.scrim)
            .text_color(t.accent)
            .text_size(px(theme::BODY * theme::HEADING))
            .font_weight(FontWeight::SEMIBOLD)
            .group_drag_over::<ExternalPaths>("conversation", |style| style.visible())
            .child("Drop to attach"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn situation() -> Situation {
        Situation {
            id: ConversationId("c:alice".into()),
            title: "Alice".into(),
            subtitle: String::new(),
            request: false,
            blocked: false,
            left: false,
            invited: false,
            requested: false,
            safety_changed: false,
            editing: false,
            reply: None,
            staged: Vec::new(),
        }
    }

    #[test]
    fn the_field_shows_unless_a_bar_says_why_not() {
        assert!(situation().shows_field());
        for hide in [
            |s: &mut Situation| s.request = true,
            |s: &mut Situation| s.blocked = true,
            |s: &mut Situation| s.left = true,
            |s: &mut Situation| s.safety_changed = true,
        ] {
            let mut s = situation();
            hide(&mut s);
            assert!(!s.shows_field());
        }
        // An invitation or a requested join is a group we are not in yet: `left` is set too,
        // and the bar about it stands in for the field.
        let mut s = situation();
        s.invited = true;
        assert!(s.shows_field());
    }
}
