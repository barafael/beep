//! What the window's tests share: the app brought up the way `main` does, but on GPUI's test
//! platform (no compositor, no GPU, a clock that stands still unless it is moved) and on the
//! mock backend, with every file b33p writes going to a scratch home.
//!
//! `e2e_tests.rs` drives it through the agent harness's socket, as `b33p-slint/tests/harness.rs`
//! drives the Slint window; `ui_tests.rs` gives the views a chat state and input directly, as
//! `b33p-slint/src/ui_tests.rs` does.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Once;

use b33p_core::Backend;
use b33p_core::settings::Settings;
use b33p_core::state::{ChatState, Update};
use b33p_notify::Notifier;
use gpui::TestAppContext;
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::{App, AppContext as _, WindowHandle};

use crate::app::AppView;
use crate::shared::Shared;

/// Points everything b33p writes (settings, the mock's saved files, the log) and every bus it
/// might talk to somewhere harmless. The environment is the process's, shared by all tests, so
/// this happens once, before any of them gets as far as reading it.
pub fn init() -> PathBuf {
    static ONCE: Once = Once::new();
    let home = std::env::temp_dir().join(format!("b33p-gpui-tests-{}", std::process::id()));
    ONCE.call_once(|| {
        for dir in ["config", "data", "cache", "runtime", "home"] {
            std::fs::create_dir_all(home.join(dir)).expect("a scratch home");
        }
        // SAFETY: every test calls `init` first and waits here for the one that sets these;
        // nothing reads the environment before that, and nothing writes it afterwards.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
            std::env::set_var("XDG_DATA_HOME", home.join("data"));
            std::env::set_var("XDG_CACHE_HOME", home.join("cache"));
            std::env::set_var("XDG_RUNTIME_DIR", home.join("runtime"));
            std::env::set_var("HOME", home.join("home"));
            // A bus that isn't there: b33p can't tell whether another copy runs, and starts
            // anyway, and the notifier and the tray find no desktop to talk to.
            std::env::set_var(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", home.join("bus").display()),
            );
            std::env::set_var("B33P_MOCK", "1");
        }
    });
    home
}

/// A window on the app, with the state it shows.
pub struct Booted {
    pub shared: Rc<Shared>,
    pub window: WindowHandle<AppView>,
    /// What the window asked the backend for, when there is none.
    pub recorded: Option<b33p_core::Recorded>,
    /// Keeps the mock backend running until the test is over.
    _backend: Option<Backend>,
    /// What the app holds the other end of, which must stay open.
    _kept: Box<dyn std::any::Any>,
}

/// Brings the app up on `chat`, with a window of `width` by `height` logical pixels. Nothing
/// arrives from the backend unless the test makes it: the chat state is what it was given.
pub fn boot(cx: &mut TestAppContext, chat: ChatState, width: f32, height: f32) -> Booted {
    let (booted, _) = boot_with(cx, chat, width, height, None);
    booted
}

/// The app as `main` runs it: the backend's events, the notifications' actions and the rest of
/// what arrives from outside are pumped into the window, and the agent listens at `socket`.
#[cfg(all(feature = "harness", target_family = "unix"))]
pub fn boot_live(cx: &mut TestAppContext, socket: &std::path::Path) -> Booted {
    let (booted, _) = boot_with(cx, ChatState::default(), 1000., 680., Some(socket));
    booted
}

fn boot_with(
    cx: &mut TestAppContext,
    chat: ChatState,
    width: f32,
    height: f32,
    live: Option<&std::path::Path>,
) -> (Booted, ()) {
    init();
    let (notifier, notifier_actions) = Notifier::new(&crate::APP);
    let (desktop_requests, desktop_queue) = async_channel::unbounded();
    let (quit, quit_asked) = async_channel::unbounded();
    // The mock for the app as it runs; for a window alone, a recorder of what it asks for.
    let (backend, commands, events, recorded) = match live {
        Some(_) => {
            let backend = Backend::mock();
            let (commands, events) = (backend.commands(), backend.events());
            (Some(backend), commands, events, None)
        }
        None => {
            let (commands, recorded) = b33p_core::Commands::recorded();
            (None, commands, async_channel::unbounded().1, Some(recorded))
        }
    };
    let shared = cx.update(|cx| {
        gpui_kit::init(cx);
        crate::actions::bind(cx);
        crate::theme::apply(b33p_core::settings::Theme::Light, None, cx);
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let shared = Shared::new(
            chat,
            notifier,
            commands,
            Settings::default(),
            desktop_requests,
            scroller,
        );
        crate::window::apply_motion(&shared, cx);
        shared
    });
    // What the state holds is told to the message list before the first frame, so that the
    // list isn't first seen empty (and at its top, where it asks for older messages).
    cx.update(|cx| shared.refresh(crate::shared::everything(), cx));
    cx.run_until_parked();
    let window = cx.update(|cx| open(&shared, width, height, cx));
    if let Some(_socket) = live {
        cx.update(|cx| {
            #[cfg(all(feature = "harness", target_family = "unix"))]
            crate::agent::start_at(_socket.to_owned(), &shared, cx);
            crate::spawn_pumps(&shared, events, notifier_actions, quit_asked, cx);
        });
    }
    cx.run_until_parked();
    let booted = Booted {
        shared,
        window,
        recorded,
        _backend: backend,
        _kept: Box::new((desktop_queue, quit)),
    };
    (booted, ())
}

fn open(shared: &Rc<Shared>, width: f32, height: f32, cx: &mut App) -> WindowHandle<AppView> {
    let options = gpui_kit::WindowOptions {
        window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds::new(
            gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)),
            gpui_kit::size(gpui_kit::px(width), gpui_kit::px(height)),
        ))),
        ..gpui_kit::WindowOptions::default()
    };
    cx.open_window(options, |window, cx| {
        cx.new(|cx| AppView::new(shared.clone(), window, cx))
    })
    .expect("a test window opens")
}

/// A chat state on the chats screen, with `chats` listed (all accepted contacts), the first of
/// them open.
pub fn chats(titles: &[&str]) -> ChatState {
    use b33p_core::{ChatSettings, Conversation, ConversationId, Event};
    let mut chat = ChatState::default();
    chat.apply(Event::Linked {
        account: "me".into(),
        service_id: "me".into(),
    });
    chat.apply(Event::Synced);
    chat.apply(Event::Conversations(
        titles
            .iter()
            .map(|title| Conversation {
                id: ConversationId::contact(&title.to_lowercase()),
                title: (*title).into(),
                settings: ChatSettings {
                    accepted: true,
                    ..ChatSettings::default()
                },
                ..Conversation::default()
            })
            .collect(),
    ));
    chat
}

/// The window of a booted app, to look into and give input to.
pub struct Ui {
    pub booted: Booted,
    pub cx: gpui::VisualTestContext,
}

impl Ui {
    pub fn new(tc: &mut TestAppContext, chat: ChatState) -> Self {
        Self::sized(tc, chat, 1000., 680.)
    }

    pub fn sized(tc: &mut TestAppContext, chat: ChatState, width: f32, height: f32) -> Self {
        let booted = boot(tc, chat, width, height);
        let cx = gpui::VisualTestContext::from_window(booted.window.into(), tc);
        let mut ui = Self { booted, cx };
        ui.settle();
        ui
    }

    /// Lets everything that was asked for happen, and draws.
    pub fn settle(&mut self) {
        self.cx.run_until_parked();
        self.cx.update(|window, _| window.refresh());
        self.cx.run_until_parked();
    }

    /// What the state says now, drawn.
    pub fn show(&mut self, update: Update) {
        self.cx
            .update(|_, cx| self.booted.shared.refresh(update, cx));
        self.settle();
    }

    /// Turns the wheel over the messages by `pixels`: positive goes towards older ones.
    pub fn scroll(&mut self, pixels: f32) {
        let size = self.window_size();
        let position = gpui::point(size.width * 0.7, size.height * 0.4);
        self.cx.simulate_event(gpui::ScrollEvent {
            position,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(pixels))),
            modifiers: gpui::Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
            source: gpui::ScrollSource::Pointer,
        });
        self.settle();
    }

    /// Where the message list is scrolled to: the first row shown, and how far into it.
    pub fn top_row(&mut self) -> (usize, f32) {
        let scroller = self.booted.shared.scroller.clone();
        self.cx.update(|_, cx| {
            let top = scroller.read(cx).logical_scroll_top();
            (top.item_ix, top.offset_in_item.into())
        })
    }

    /// Presses `keys` (as GPUI spells them, "ctrl-k"), which go where the focus is.
    pub fn press(&mut self, keys: &str) {
        self.cx.simulate_keystrokes(keys);
        self.settle();
    }

    /// Ctrl+K, which puts the caret in the chat list's search.
    pub fn focus_search(&mut self) {
        self.press("ctrl-k");
    }

    /// Types `text` where the focus is.
    pub fn type_keys(&mut self, text: &str) {
        self.cx.simulate_input(text);
        self.settle();
    }

    /// Moves the pointer to the middle of the element named `selector`.
    pub fn hover(&mut self, selector: &str) {
        let bounds = self
            .bounds(selector)
            .unwrap_or_else(|| panic!("{selector:?} is not shown"));
        self.cx
            .simulate_mouse_move(bounds.center(), None, gpui::Modifiers::none());
        self.settle();
    }

    /// Lets `time` pass on the app's clock, which stands still unless it is moved.
    pub fn wait(&mut self, time: std::time::Duration) {
        self.cx.executor().advance_clock(time);
        self.settle();
    }

    /// How many boxes the last frame painted: more of them when something (a tooltip) is shown.
    pub fn painted(&mut self) -> usize {
        self.cx.update(|window, _| window.painted_quads().len())
    }

    /// The backgrounds of the boxes the last frame painted, in order.
    pub fn backgrounds(&mut self) -> String {
        self.cx.update(|window, _| {
            window
                .painted_quads()
                .iter()
                .map(|quad| format!("{:?}", quad.background))
                .collect::<Vec<_>>()
                .join(" ")
        })
    }

    /// Presses the left button at `from`, moves to `to` and lets go, as a drag does.
    pub fn drag(&mut self, from: gpui::Point<gpui::Pixels>, to: gpui::Point<gpui::Pixels>) {
        self.cx
            .simulate_mouse_move(from, None, gpui::Modifiers::none());
        self.cx
            .simulate_mouse_down(from, gpui::MouseButton::Left, gpui::Modifiers::none());
        self.cx
            .simulate_mouse_move(to, gpui::MouseButton::Left, gpui::Modifiers::none());
        self.cx
            .simulate_mouse_up(to, gpui::MouseButton::Left, gpui::Modifiers::none());
        self.settle();
    }

    /// The text on the clipboard.
    pub fn clipboard(&mut self) -> Option<String> {
        self.cx.read_from_clipboard().and_then(|item| item.text())
    }

    /// The commands the window sent to the backend since the last call.
    pub fn sent(&self) -> Vec<b33p_core::Command> {
        self.booted
            .recorded
            .as_ref()
            .map(b33p_core::Recorded::take)
            .unwrap_or_default()
    }

    /// Changes the chat state as `f` says, and draws what it asks for.
    pub fn act(&mut self, f: impl FnOnce(&mut ChatState) -> Update) {
        let update = f(&mut self.booted.shared.chat.borrow_mut());
        self.show(update);
    }

    /// Where the element named `selector` is, if it is drawn.
    pub fn bounds(&mut self, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
        // The window keeps selectors as `&'static str`; the few a test builds are kept for good.
        self.cx
            .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
    }

    pub fn shown(&mut self, selector: &str) -> bool {
        self.bounds(selector).is_some()
    }

    /// The window's size, in logical pixels.
    pub fn window_size(&mut self) -> gpui::Size<gpui::Pixels> {
        self.cx.update(|window, _| window.viewport_size())
    }

    /// Puts `text` in the composer, as if typed.
    pub fn type_in_composer(&mut self, text: &str) {
        let text = text.to_owned();
        let shared = self.booted.shared.clone();
        self.cx.update(|window, cx| {
            let composer = shared
                .composer
                .borrow()
                .as_ref()
                .and_then(|composer| composer.upgrade())
                .expect("a chat is open");
            composer.update(cx, |composer, cx| composer.set_text(&text, window, cx));
        });
        self.settle();
    }

    /// Selects `range` of the composer's text and has an emoji from the picker put in.
    pub fn insert_in_composer(&mut self, text: &str, range: std::ops::Range<usize>) {
        let text = text.to_owned();
        let shared = self.booted.shared.clone();
        self.cx.update(|window, cx| {
            let composer = shared
                .composer
                .borrow()
                .as_ref()
                .and_then(|composer| composer.upgrade())
                .expect("a chat is open");
            composer.update(cx, |composer, cx| {
                composer.select(range, cx);
                composer.insert(&text, window, cx);
            });
        });
        self.settle();
    }

    /// The text in the composer.
    pub fn composer_text(&mut self) -> String {
        let shared = self.booted.shared.clone();
        self.cx.update(|_, cx| {
            let composer = shared
                .composer
                .borrow()
                .as_ref()
                .and_then(|composer| composer.upgrade())
                .expect("a chat is open");
            composer.read(cx).text(cx)
        })
    }

    /// A click in the middle of the element named `selector`.
    pub fn click(&mut self, selector: &str) {
        let bounds = self
            .bounds(selector)
            .unwrap_or_else(|| panic!("{selector:?} is not shown"));
        self.cx
            .simulate_click(bounds.center(), gpui::Modifiers::none());
        self.settle();
    }
}

/// An error of `kind` that says `message`, as the backend's failures are shown.
pub fn error(kind: b33p_core::ErrorKind, message: &str) -> b33p_core::Error {
    #[derive(Debug, thiserror::Error)]
    #[error("{message}")]
    struct Said {
        kind: b33p_core::ErrorKind,
        message: String,
    }
    impl b33p_core::Classified for Said {
        fn kind(&self) -> b33p_core::ErrorKind {
            self.kind
        }
    }
    Said {
        kind,
        message: message.to_owned(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    async fn the_app_comes_up(cx: &mut TestAppContext) {
        let booted = boot(cx, ChatState::default(), 900., 600.);
        assert!(booted.shared.chat.borrow().error().is_none());
    }
}
