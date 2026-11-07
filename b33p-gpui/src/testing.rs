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
    /// Keeps the mock backend running until the test is over.
    pub backend: Backend,
}

/// Brings the app up on `chat`, with a window of `width` by `height` logical pixels.
pub fn boot(cx: &mut TestAppContext, chat: ChatState, width: f32, height: f32) -> Booted {
    init();
    let backend = Backend::mock();
    let (notifier, _actions) = Notifier::new(&crate::APP);
    let (desktop_requests, _desktop_queue) = async_channel::unbounded();
    let commands = backend.commands();
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
    let window = cx.update(|cx| open(&shared, width, height, cx));
    cx.run_until_parked();
    Booted {
        shared,
        window,
        backend,
    }
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

/// Draws what the state says, as the window does after every turn of the event loop.
pub fn show(booted: &Booted, cx: &mut TestAppContext, update: Update) {
    cx.update(|cx| booted.shared.refresh(update, cx));
    cx.run_until_parked();
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
