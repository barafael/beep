//! The window's life, which is not the app's: it opens, closes into the tray, opens again from
//! the tray, from a second start or from a notification, and may not open at all (`--hidden`).
//! [`Shared`] outlives every window, so each one is drawn anew from it.
//!
//! GPUI windows can't be hidden, only closed; so "in the tray" is "no window, and the process
//! running", and showing b33p again is opening a window.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use b33p_core::settings::Settings;
use b33p_core::state::{ChatState, Screen};
use b33p_core::{Backend, Failure};
use gpui_kit::{
    App, AppContext as _, AsyncApp, Bounds, TitlebarOptions, WindowBounds, WindowDecorations,
    WindowOptions, WindowingRequest, px, size,
};

use crate::shared::Shared;
use crate::tray::{self, Tray};

/// Result without a dependency on anyhow, which GPUI's errors come in.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// A window is being closed to be opened anew (see [`activate`]): its closing is not the last
/// window's. GPUI tells of a closed window at once, before the next one is opened.
static REOPENING: AtomicBool = AtomicBool::new(false);

/// The connection to the compositor is being made anew with an activation token, between a
/// window closed and the next one opened (see [`reconnect_with`]); nothing else opens one.
static SWITCHING: AtomicBool = AtomicBool::new(false);

/// Which switch of the connection is under way, so that a late answer to one that was given up
/// on (see [`SWITCH_PATIENCE`]) does not open a second window.
static SWITCH: AtomicU64 = AtomicU64::new(0);

/// How long the compositor gets to answer a switch of the connection before it is given up on
/// and the window opened on whatever connection there is. Without this, a compositor that never
/// answers leaves b33p with no window and every later "Open" ignored as "under way already".
const SWITCH_PATIENCE: Duration = Duration::from_secs(5);

/// How long a start into the tray (`--hidden`) waits for the desktop to show tray icons, as it
/// may not yet at login, before opening the window after all.
const TRAY_WAIT: Duration = Duration::from_secs(10);

/// Opens the window, with the views that draw the shared state.
pub fn open(shared: &Rc<Shared>, cx: &mut App) -> Result<()> {
    let title = if Backend::mock_requested() {
        "b33p · MOCK"
    } else {
        "b33p"
    };
    let options = WindowOptions {
        window_bounds: Some(shared.window_bounds.get().unwrap_or_else(|| {
            WindowBounds::Windowed(Bounds::centered(None, size(px(1000.), px(680.)), cx))
        })),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..TitlebarOptions::default()
        }),
        window_min_size: Some(size(px(360.), px(480.))),
        app_id: Some(crate::APP.id.into()),
        window_decorations: Some(WindowDecorations::Server),
        ..WindowOptions::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        // The native window exists by now; Windows is asked to keep it out of captures.
        crate::capture::exclude(window);
        cx.new(|cx| crate::app::AppView::new(shared.clone(), window, cx))
    })?;
    cx.activate(true);
    Ok(())
}

/// Opens the window, or ends b33p if it can't be: an app without a window nor a way to get
/// one is out of anyone's reach.
fn open_or_quit(shared: &Rc<Shared>, cx: &mut App) {
    if let Err(error) = open(shared, cx) {
        tracing::error!(error = %b33p_core::scrub::scrubbed(&error), "can't open the window");
        cx.quit();
    }
}

/// Puts b33p in the tray, if the desktop has one, and keeps the window's life in step with the
/// tray's and the system's: what to do when the window closes, a second start, the tray's own
/// menu. Call once, with the first window open (or not, for `--hidden`).
pub fn start(
    shared: &Rc<Shared>,
    activations: Option<async_channel::Receiver<Option<String>>>,
    hidden: bool,
    cx: &mut App,
) {
    // Keeps `host_present`'s answer up to date, on threads of its own.
    tray::watch_host();
    let (events, tray_events) = async_channel::unbounded();
    let shown = shared.settings.borrow().close_to_tray;
    shared.tray.replace(Tray::new(events, shown));
    // The icon's menu: "Open b33p" and "Quit".
    cx.spawn({
        let shared = shared.clone();
        async move |cx: &mut AsyncApp| {
            while let Ok(event) = tray_events.recv().await {
                cx.update(|cx| match event {
                    tray::Event::Open => activate(&shared, None, cx),
                    tray::Event::Quit => cx.quit(),
                });
            }
        }
    })
    .detach();

    // Starting b33p again shows this one.
    if let Some(activations) = activations {
        cx.spawn({
            let shared = shared.clone();
            async move |cx: &mut AsyncApp| {
                while let Ok(token) = activations.recv().await {
                    tracing::info!(token = token.is_some(), "started again; showing the window");
                    cx.update(|cx| activate(&shared, token, cx));
                }
            }
        })
        .detach();
    }

    // Closing the window leaves b33p in the tray, or ends it: GPUI's own ending of the app with
    // its last window is replaced by the choice below.
    cx.set_quit_mode(gpui_kit::QuitMode::Explicit);
    cx.on_window_closed({
        let shared = shared.clone();
        move |cx, _| {
            if REOPENING.load(Ordering::Relaxed) || !cx.windows().is_empty() {
                return;
            }
            if in_tray(&shared) {
                let mut ui = shared.ui.borrow_mut();
                ui.window_visible = false;
                ui.window_active = false;
            } else {
                cx.quit();
            }
        }
    })
    .detach();

    shared.start_hidden.set(hidden);
    if !hidden {
        open_or_quit(shared, cx);
    } else if !tray::host_present() {
        open_unless_host_appears(shared, cx);
    }
}

/// Whether closing the window leaves b33p running: the setting says so, and there is a tray
/// that shows it where the user can reach it.
pub fn in_tray(shared: &Shared) -> bool {
    shared.settings.borrow().close_to_tray && shared.tray.borrow().is_some() && tray::host_present()
}

/// For a start into the tray before the desktop shows tray icons, as can happen at login: gives
/// the tray [`TRAY_WAIT`] to come up, then opens the window after all, so that b33p isn't left
/// running out of reach. (The icon itself needs no help: it registers when the host appears.)
fn open_unless_host_appears(shared: &Rc<Shared>, cx: &mut App) {
    let (sender, appeared) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("b33p-tray-wait".into())
        .spawn(move || {
            // The only message, so there is room for it; nobody is left to read it only if the
            // wait below could not start.
            if let Err(error) = sender.try_send(tray::wait_for_host(TRAY_WAIT)) {
                tracing::debug!(%error, "nobody waits for the tray any more");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "can't wait for the tray");
    }
    let shared = shared.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        // Also when the thread didn't start.
        if appeared.recv().await != Ok(true) {
            tracing::info!("no tray showed up; opening the window");
            cx.update(|cx| stop_hiding(&shared, cx));
        }
    })
    .detach();
}

/// Opens the window of a start into the tray after all, because the user is needed (or there
/// is no tray to wait in).
pub fn stop_hiding(shared: &Rc<Shared>, cx: &mut App) {
    if !shared.start_hidden.replace(false) {
        return;
    }
    if cx.windows().is_empty() {
        open_or_quit(shared, cx);
    }
}

/// Shows the window and brings it to the front, from the tray, a second start, or a click on a
/// notification. `token` is a Wayland activation token from whatever asked, which lets the
/// compositor give b33p focus.
///
/// GPUI takes such a token only when its connection to the compositor is made, and uses it for
/// the first window then (`GraphicalEnvironment::activation_token`, meant for exactly this:
/// "the token of the process that asked for a window"). So with a token and a window to bring
/// up, the connection is made anew (see [`reconnect_with`]); without one, a window that isn't
/// the active one is closed and opened again, which KWin focuses by itself.
pub fn activate(shared: &Rc<Shared>, token: Option<String>, cx: &mut App) {
    let window = cx.windows().into_iter().next();
    let active = window
        .and_then(|window| {
            window
                .update(cx, |_, window, _| window.is_window_active())
                .ok()
        })
        .unwrap_or(false);
    tracing::debug!(active, token = token.is_some(), "bringing the window up");
    if SWITCHING.load(Ordering::Relaxed) {
        tracing::debug!("the window is being brought up already");
        return;
    }
    if active {
        if let Some(window) = window
            && let Err(error) = window.update(cx, |_, window, _| window.activate_window())
        {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "can't raise the window");
        }
        cx.activate(true);
        return;
    }
    let token = token.filter(|token| !token.is_empty());
    if let Some(token) = token
        && let Some(environment) = cx
            .graphical_environment()
            .filter(|environment| environment.wayland_display.is_some())
    {
        reconnect_with(shared, token, environment, cx);
        return;
    }
    let Some(window) = window else {
        open_or_quit(shared, cx);
        return;
    };
    if cfg!(all(unix, not(target_os = "macos"))) {
        // Without a token a compositor (KWin, like others) keeps an open window where it is,
        // but it does focus one that appears: so the window appears anew, where the compositor
        // places it. Everything shown is in `Shared`, so nothing is lost.
        REOPENING.store(true, Ordering::Relaxed);
        if let Err(error) = window.update(cx, |_, window, _| window.remove_window()) {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "can't close the window");
        }
        open_or_quit(shared, cx);
        REOPENING.store(false, Ordering::Relaxed);
        return;
    }
    if let Err(error) = window.update(cx, |_, window, _| window.activate_window()) {
        tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "can't raise the window");
    }
    cx.activate(true);
}

/// Opens the window with `token`, which GPUI hands the compositor along with the first window of
/// a connection: the window (if any) is closed, the connection to the compositor dropped
/// (GPUI's headless mode) and made again with the token, and a window opened on it. Nothing
/// shown is lost, as it is all in [`Shared`]; what is lost is a selection copied to the
/// clipboard from b33p, which the compositor drops with the connection. If it fails, a window
/// is opened on whatever connection there is, as without a token.
fn reconnect_with(
    shared: &Rc<Shared>,
    token: String,
    environment: gpui_kit::GraphicalEnvironment,
    cx: &mut App,
) {
    SWITCHING.store(true, Ordering::Relaxed);
    REOPENING.store(true, Ordering::Relaxed);
    let switch = SWITCH.fetch_add(1, Ordering::Relaxed) + 1;
    for window in cx.windows() {
        if let Err(error) = window.update(cx, |_, window, _| window.remove_window()) {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "can't close the window");
        }
    }
    // Ends the switch, once: by whichever of the answer and the patience's end comes first.
    let finish = {
        let shared = shared.clone();
        move |cx: &mut App| {
            if SWITCH.load(Ordering::Relaxed) != switch || !SWITCHING.load(Ordering::Relaxed) {
                return;
            }
            open_or_quit(&shared, cx);
            REOPENING.store(false, Ordering::Relaxed);
            SWITCHING.store(false, Ordering::Relaxed);
        }
    };
    cx.spawn({
        let finish = finish.clone();
        async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(SWITCH_PATIENCE).await;
            cx.update(|cx| {
                if SWITCH.load(Ordering::Relaxed) == switch && SWITCHING.load(Ordering::Relaxed) {
                    tracing::warn!("the compositor did not answer; opening the window as it is");
                    finish(cx);
                }
            });
        }
    })
    .detach();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let switched = async {
            let headless = cx.update(|cx| cx.request_windowing(WindowingRequest::Headless));
            headless.await?;
            let windowed = cx.update(|cx| {
                cx.request_windowing(WindowingRequest::Windowed(
                    gpui_kit::GraphicalEnvironment {
                        activation_token: Some(token),
                        ..environment
                    },
                ))
            });
            windowed.await
        }
        .await;
        if let Err(error) = switched {
            tracing::warn!(error = %b33p_core::scrub::chain(&*error), "can't hand the compositor the activation token");
        }
        cx.update(|cx| finish(cx));
    })
    .detach();
}

/// What follows from a flush of the shared state: the icon's unread count, and whether a start
/// into the tray needs the user after all.
pub fn after_flush(shared: &Shared, chat: &ChatState, cx: &mut App) {
    if let Some(tray) = &*shared.tray.borrow() {
        tray.set_unread(chat.unread_total());
    }
    if !shared.start_hidden.get() {
        return;
    }
    let screen = chat.screen();
    // Not for a keyring that isn't unlocked yet: at login it usually is a moment later, and it
    // is tried again by itself. Opened from the tray, the window says why.
    let needs_user = match screen {
        Screen::Password { .. } | Screen::Link(_) => true,
        Screen::Failed { failure, .. } => *failure != Failure::KeyringUnavailable,
        Screen::Syncing(_) | Screen::Chats => false,
    };
    if needs_user || chat.error().is_some() {
        tracing::info!(
            needs_user,
            error = chat.error().is_some(),
            "the start into the tray needs the user"
        );
        // After this flush: opening the window reads the state it holds.
        let me = shared.me.clone();
        cx.defer(move |cx| {
            if let Some(shared) = me.upgrade() {
                stop_hiding(&shared, cx);
            }
        });
    } else if matches!(screen, Screen::Chats) {
        shared.start_hidden.set(false);
    }
}

/// What a change of the settings does outside the window: the tray icon shows or goes, and
/// animations follow the "Animations" choice.
pub fn settings_changed(shared: &Shared, settings: &Settings, cx: &mut App) {
    if let Some(tray) = &*shared.tray.borrow() {
        tray.set_shown(settings.close_to_tray);
    }
    apply_motion(shared, cx);
}

/// Animates, or not, as the settings and the desktop say: the component library and GPUI ask
/// [`App::reduce_motion`] before every transition. Once set here, the flag is ours, so the
/// desktop's own preference reaches it only through [`Shared::system_reduces_motion`].
pub fn apply_motion(shared: &Shared, cx: &mut App) {
    cx.set_reduce_motion(!shared.animates());
}
