//! What shows before the chats: starting up, the password, the code to link, and what went
//! wrong at the start.

use std::rc::Rc;
use std::time::{Duration, Instant};

use b33p_core::state::{ChatState, Screen, Update};
use b33p_core::{Failure, HistoryTransfer, QrMatrix, SyncStage};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Div, Entity, FontWeight, IntoElement, Render, SharedString, Subscription,
    Window, div, px,
};

use crate::shared::{Confirm, Shared};
use crate::theme::{self, Tokens};

/// The start screen's line about what it waits for.
pub fn sync_status(stage: SyncStage) -> String {
    match stage {
        SyncStage::Starting => "Opening your messages…".into(),
        SyncStage::Connecting => "Connecting to Signal…".into(),
        SyncStage::Transfer(HistoryTransfer::Waiting) => {
            "Waiting for your phone to send your message history…".into()
        }
        SyncStage::Transfer(HistoryTransfer::Downloading) => {
            "Downloading your message history…".into()
        }
        SyncStage::Transfer(HistoryTransfer::Importing { messages }) => {
            format!("Importing your message history… {messages} messages")
        }
        SyncStage::Transfer(HistoryTransfer::Finished) => "Connecting to Signal…".into(),
        SyncStage::CatchingUp { messages: 0 } => "Catching up on your messages…".into(),
        SyncStage::CatchingUp { messages } => {
            format!("Catching up on your messages… {messages} new")
        }
    }
}

pub struct StartView {
    shared: Rc<Shared>,
    password: Entity<InputState>,
    confirmation: Entity<InputState>,
    device_name: Entity<InputState>,
    /// When "Start" first could be offered: it shows once the wait has been long enough that
    /// the chats are worth opening before everything has come in.
    could_start_since: Option<Instant>,
    _subscriptions: Vec<Subscription>,
}

impl StartView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let password = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Password"));
        let confirmation =
            cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Repeat password"));
        let name = shared.settings.borrow().device_name();
        let device_name = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let subscriptions = vec![
            cx.subscribe_in(&password, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event
                    && !this.is_new_password()
                {
                    this.submit(window, cx);
                }
            }),
            cx.subscribe_in(&confirmation, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit(window, cx);
                }
            }),
            cx.subscribe_in(&device_name, window, |this, input, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let name = input.read(cx).value().to_string();
                    this.shared.rename_device(&name, cx);
                }
            }),
        ];
        Self {
            shared,
            password,
            confirmation,
            device_name,
            could_start_since: None,
            _subscriptions: subscriptions,
        }
    }

    fn is_new_password(&self) -> bool {
        matches!(
            self.shared.chat.borrow().screen(),
            Screen::Password { new: true, .. }
        )
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let password = self.password.read(cx).value().to_string();
        let confirmation = self.confirmation.read(cx).value().to_string();
        self.shared.act(cx, |chat| {
            (
                chat.unlock(&password, &confirmation),
                Update {
                    screen: true,
                    ..Update::default()
                },
            )
        });
        for input in [&self.password, &self.confirmation] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.password.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Ready to type into.
    pub fn focus_password(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.password.update(cx, |input, cx| input.focus(window, cx));
    }
}

/// The page every start screen is: centered, a column 360 wide, an optional logo and title,
/// then `children`.
fn page_with(t: &Tokens, title: Option<&str>, mark: bool, children: Vec<AnyElement>) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(t.surface)
        .child(
            div()
                .w(px(360.))
                .max_w_full()
                .mx(px(theme::SPACE_XL))
                .flex()
                .flex_col()
                .gap(px(theme::SPACE_M))
                .when(mark, |column| {
                    column.child(
                        div()
                            .flex()
                            .justify_center()
                            .pb(px(theme::SPACE_S))
                            .child(gpui_kit::img("b33p.svg").size(px(64.))),
                    )
                })
                .children(title.map(|title| {
                    div()
                        .text_center()
                        .text_size(px(theme::BODY * theme::DISPLAY))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(title.to_owned()))
                }))
                .children(children),
        )
}

fn centered_text(t: &Tokens, text: impl Into<SharedString>) -> Div {
    div()
        .text_center()
        .text_color(t.text_secondary)
        .child(text.into())
}

impl Render for StartView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let chat = shared.chat.borrow();
        match chat.screen().clone() {
            Screen::Syncing(stage) => {
                self.could_start_since = None;
                self.syncing(&chat, stage, &t, cx).into_any_element()
            }
            Screen::Password { new, error } => self.password_screen(new, error, &t, cx),
            Screen::Failed { failure, message } => {
                self.failed(&chat, failure, message, &t, cx)
            }
            Screen::Link(qr) => self.link(&qr, &t, cx),
            Screen::Chats => div().into_any_element(),
        }
    }
}

impl StartView {
    fn syncing(
        &mut self,
        chat: &ChatState,
        stage: SyncStage,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let skippable = stage == SyncStage::Transfer(HistoryTransfer::Waiting);
        let can_start = chat.can_start();
        let since = *self.could_start_since.get_or_insert_with(Instant::now);
        // After a short wait, once the chats could be shown.
        let waited = can_start && since.elapsed() >= Duration::from_millis(1500);
        if can_start && !waited {
            let wake = Duration::from_millis(1500).saturating_sub(since.elapsed());
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wake + Duration::from_millis(20)).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
        let shared = self.shared.clone();
        let mut children: Vec<AnyElement> = vec![
            div()
                .flex()
                .justify_center()
                .child(Spinner::new().large())
                .into_any_element(),
            centered_text(t, sync_status(stage)).into_any_element(),
        ];
        if skippable {
            children.push(
                div()
                    .flex()
                    .justify_center()
                    .child(Button::new("skip").label("Skip").on_click({
                        let shared = shared.clone();
                        move |_, _, cx| shared.command(cx, |chat| chat.skip_history())
                    }))
                    .into_any_element(),
            );
        }
        if waited {
            children.push(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(theme::SPACE_S))
                    .child(Button::new("start").primary().label("Start").on_click({
                        let shared = shared.clone();
                        move |_, _, cx| shared.redraw(cx, |chat| chat.start())
                    }))
                    .child(
                        div()
                            .text_center()
                            .text_size(px(theme::BODY * theme::SMALL))
                            .text_color(t.text_tertiary)
                            .child("The chats show now; what is still coming in follows."),
                    )
                    .into_any_element(),
            );
        }
        page_with(t, None, true, children)
    }

    fn password_screen(
        &mut self,
        new: bool,
        error: Option<String>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shared = self.shared.clone();
        let mut children: Vec<AnyElement> = vec![
            centered_text(
                t,
                if new {
                    "No system keyring was found, so b33p will encrypt your messages with this \
                     password. It can't be recovered if you forget it."
                } else {
                    "Enter the password that protects your messages on this computer."
                },
            )
            .into_any_element(),
            Input::new(&self.password).into_any_element(),
        ];
        if new {
            children.push(Input::new(&self.confirmation).into_any_element());
        }
        if let Some(error) = error.filter(|error| !error.is_empty()) {
            children.push(
                div()
                    .text_center()
                    .text_color(t.danger_text)
                    .child(error)
                    .into_any_element(),
            );
        }
        children.push(
            Button::new("unlock")
                .primary()
                .label(if new { "Continue" } else { "Unlock" })
                .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
                .into_any_element(),
        );
        if !new {
            children.push(
                Button::new("forgot")
                    .label("Forgot your password?")
                    .on_click(move |_, _, cx| {
                        shared.ui.borrow_mut().confirm = Confirm::StartOver;
                        cx.refresh_windows();
                    })
                    .into_any_element(),
            );
        }
        page_with(
            t,
            Some(if new { "Choose a password" } else { "Unlock b33p" }),
            true,
            children,
        )
        .into_any_element()
    }

    fn failed(
        &mut self,
        chat: &ChatState,
        failure: Failure,
        message: String,
        t: &Tokens,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let shared = self.shared.clone();
        let mut children: Vec<AnyElement> = vec![centered_text(t, message).into_any_element()];
        if failure == Failure::KeyringUnavailable {
            children.push(
                div()
                    .text_center()
                    .text_size(px(theme::BODY * theme::SMALL))
                    .text_color(t.text_tertiary)
                    .child("Trying again by itself…")
                    .into_any_element(),
            );
        }
        if failure.can_retry() {
            children.push(
                Button::new("retry")
                    .primary()
                    .label("Try again")
                    .on_click({
                        let shared = shared.clone();
                        move |_, _, cx| {
                            shared.act(cx, |chat| {
                                (
                                    chat.retry(),
                                    Update {
                                        screen: true,
                                        ..Update::default()
                                    },
                                )
                            });
                        }
                    })
                    .into_any_element(),
            );
        }
        if failure.can_use_password() {
            children.push(
                Button::new("use-password")
                    .label("Use a password instead")
                    .on_click({
                        let shared = shared.clone();
                        move |_, _, cx| {
                            shared.act(cx, |chat| {
                                (
                                    chat.use_password(),
                                    Update {
                                        screen: true,
                                        ..Update::default()
                                    },
                                )
                            });
                        }
                    })
                    .into_any_element(),
            );
        }
        if chat.can_start_over() {
            children.push(
                Button::new("start-over")
                    .label("Start over…")
                    .on_click({
                        let shared = shared.clone();
                        move |_, _, cx| {
                            shared.ui.borrow_mut().confirm = Confirm::StartOver;
                            cx.refresh_windows();
                        }
                    })
                    .into_any_element(),
            );
        }
        children.push(
            Button::new("data-folder")
                .label("Open data folder")
                .on_click(|_, _, _| open_folder(b33p_core::settings::data_dir()))
                .into_any_element(),
        );
        page_with(t, Some("b33p can't go on"), true, children).into_any_element()
    }

    fn link(&mut self, qr: &QrMatrix, t: &Tokens, _cx: &mut Context<Self>) -> AnyElement {
        let modules = qr.to_rgba(1);
        let (side, pixels) = modules;
        let drawn = crate::images::draw(&b33p_core::Picture {
            width: side as u32,
            height: side as u32,
            rgba: pixels,
        });
        let code = div()
            .size(px(280.))
            .rounded(px(theme::RADIUS_L))
            .bg(gpui_kit::white())
            .p(px(12.))
            .shadow_lg()
            .children(drawn.map(|drawn| {
                gpui_kit::img(drawn)
                    .size_full()
                    .with_fallback(|| div().into_any_element())
            }));
        let children: Vec<AnyElement> = vec![
            div()
                .flex()
                .justify_center()
                .py(px(theme::SPACE_S))
                .child(code)
                .into_any_element(),
            centered_text(
                t,
                "On your phone, open Signal → Settings → Linked devices → Link new device, then \
                 scan this code.",
            )
            .into_any_element(),
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_S))
                .child(div().text_color(t.text_secondary).child("Name on your phone"))
                .child(div().flex_1().child(Input::new(&self.device_name)))
                .into_any_element(),
        ];
        page_with(t, Some("Link b33p to your phone"), false, children).into_any_element()
    }
}

/// Opens a folder in the desktop's file manager.
pub fn open_folder(dir: Option<std::path::PathBuf>) {
    let Some(dir) = dir else { return };
    if let Err(error) = open::that_detached(&dir) {
        tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "couldn't open a folder");
    }
}
