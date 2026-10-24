//! `cargo testbox vnc`: the box's displays in one window, each from the VNC socket its
//! `with-display` serves, side by side as their toggles pick (with a window each, only the
//! window manager could put two side by side). Once connected, the viewer
//! restricts itself with Landlock (`testbox_vnc::sandbox`).

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use slint::{
    ComponentHandle, FilterModel, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel, Weak,
};
use testbox_tools::{Context, Describe, Result};
use testbox_vnc::{Frame, NamedKey, VncClient, X11Event};
use tokio::sync::mpsc;

slint::slint! {
    import { CheckBox } from "std-widgets.slint";

    export struct Screen {
        // Its place among all the displays, which the callbacks name it by.
        index: int,
        name: string,
        frame: image,
        // Why it shows nothing, or nothing new: empty while it shows its display.
        state: string,
        shown: bool,
    }

    component Pane inherits VerticalLayout {
        in property <Screen> screen;
        in property <bool> interactive;
        // The pointer at (x, y) of the frame's pixels, with these buttons down.
        callback pointer(x: float, y: float, left: bool, middle: bool, right: bool);
        // A wheel step.
        callback wheel(x: float, y: float, up: bool);
        // A key, by the text Slint gives it, pressed or released.
        callback key(text: string, down: bool);
        spacing: 2px;

        Text {
            text: root.screen.state == "" ? root.screen.name : root.screen.name + ": " + root.screen.state;
            color: #e0e0e0;
        }
        Rectangle {
            // As `image-fit: contain` draws the frame: scaled to fit, centred.
            property <float> scale: min(
                self.width / max(1px, root.screen.frame.width * 1px),
                self.height / max(1px, root.screen.frame.height * 1px));
            property <length> left-edge: (self.width - root.screen.frame.width * 1px * scale) / 2;
            property <length> top-edge: (self.height - root.screen.frame.height * 1px * scale) / 2;
            property <bool> left-down;
            property <bool> middle-down;
            property <bool> right-down;
            Image {
                source: root.screen.frame;
                image-fit: contain;
                width: 100%;
                height: 100%;
            }
            keys := FocusScope {
                enabled: root.interactive;
                key-pressed(event) => {
                    root.key(event.text, true);
                    accept
                }
                key-released(event) => {
                    root.key(event.text, false);
                    accept
                }
                TouchArea {
                    enabled: root.interactive;
                    function frame-x() -> float { (self.mouse-x - left-edge) / (1px * scale) }
                    function frame-y() -> float { (self.mouse-y - top-edge) / (1px * scale) }
                    function report() {
                        root.pointer(frame-x(), frame-y(), left-down, middle-down, right-down);
                    }
                    pointer-event(event) => {
                        if event.kind == PointerEventKind.down {
                            keys.focus();
                        }
                        if event.kind == PointerEventKind.down || event.kind == PointerEventKind.up {
                            if event.button == PointerEventButton.left {
                                left-down = event.kind == PointerEventKind.down;
                            }
                            if event.button == PointerEventButton.middle {
                                middle-down = event.kind == PointerEventKind.down;
                            }
                            if event.button == PointerEventButton.right {
                                right-down = event.kind == PointerEventKind.down;
                            }
                        }
                        report();
                    }
                    moved => {
                        report();
                    }
                    scroll-event(event) => {
                        root.wheel(frame-x(), frame-y(), event.delta-y > 0);
                        accept
                    }
                }
            }
        }
    }

    export component Viewer inherits Window {
        title: "testbox";
        preferred-width: 1600px;
        preferred-height: 1000px;
        background: #1e1e1e;
        // All the displays, for the toggles, and the ones they show.
        in property <[Screen]> screens;
        in property <[Screen]> panes;
        in property <bool> interactive;
        callback toggled(index: int, shown: bool);
        callback pointer(index: int, x: float, y: float, left: bool, middle: bool, right: bool);
        callback wheel(index: int, x: float, y: float, up: bool);
        callback key(index: int, text: string, down: bool);

        VerticalLayout {
            padding: 4px;
            spacing: 4px;
            HorizontalLayout {
                spacing: 12px;
                alignment: start;
                for screen in root.screens: CheckBox {
                    text: screen.name;
                    checked: screen.shown;
                    toggled => {
                        root.toggled(screen.index, self.checked);
                    }
                }
                Text {
                    text: root.interactive ? "input on" : "watching: input off";
                    color: #e0e0e0;
                    vertical-alignment: center;
                }
            }
            HorizontalLayout {
                spacing: 4px;
                for screen in root.panes: Pane {
                    screen: screen;
                    interactive: root.interactive;
                    pointer(x, y, left, middle, right) => {
                        root.pointer(screen.index, x, y, left, middle, right);
                    }
                    wheel(x, y, up) => {
                        root.wheel(screen.index, x, y, up);
                    }
                    key(text, down) => {
                        root.key(screen.index, text, down);
                    }
                }
            }
        }
    }
}

fn main() -> std::process::ExitCode {
    // No D-Bus: an address zbus rejects, so Slint's watcher of the desktop's settings (light or
    // dark), which no Cargo feature turns off, gives up quietly
    // (i-slint-backend-winit xdg_desktop_settings.rs `portal_unavailable`) instead of failing
    // on the sandbox.
    // SAFETY: the process's only thread yet; nothing reads the environment concurrently.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "disabled:") };
    testbox_tools::exit("testbox-viewer", run())
}

/// What a display's task and the window share: whether its toggle shows it, and whether a frame
/// of it is on its way to the window.
struct Shared {
    shown: AtomicBool,
    in_flight: AtomicBool,
}

fn run() -> Result<()> {
    let (interactive, targets) = testbox_vnc::arguments("testbox-viewer")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .context("a tokio runtime")?;
    // Every socket opens before the sandbox closes: a display that isn't there shows why.
    let clients: Vec<Result<VncClient>> = targets
        .iter()
        .map(|target| runtime.block_on(testbox_vnc::connect(&target.socket)))
        .collect();
    // The window's connection to the desktop's display opens here.
    let viewer = Viewer::new().describe("the window")?;
    let screens = Rc::new(VecModel::from(
        targets
            .iter()
            .zip(&clients)
            .enumerate()
            .map(|(index, (target, client))| Screen {
                index: i32::try_from(index).unwrap_or(i32::MAX),
                name: target.name.clone().into(),
                frame: slint::Image::default(),
                state: match client {
                    Ok(_) => "connecting".into(),
                    Err(error) => error.to_string().into(),
                },
                shown: true,
            })
            .collect::<Vec<_>>(),
    ));
    // Panes for the shown ones only: a hidden element would keep its place in the layout.
    let panes = Rc::new(FilterModel::new(screens.clone(), |screen: &Screen| {
        screen.shown
    }));
    viewer.set_screens(ModelRc::from(screens.clone()));
    viewer.set_panes(ModelRc::from(panes.clone()));
    viewer.set_interactive(interactive);
    viewer.show().describe("the window")?;
    let restricted = testbox_vnc::sandbox(readable())?;
    eprintln!(
        "testbox-viewer: Landlock {:?}, no_new_privs {}",
        restricted.ruleset, restricted.no_new_privs
    );

    let mut inputs = Vec::new();
    let mut shared = Vec::new();
    let mut followed = Vec::new();
    for (index, client) in clients.into_iter().enumerate() {
        let (input, events) = mpsc::unbounded_channel();
        let display = Arc::new(Shared {
            shown: AtomicBool::new(true),
            in_flight: AtomicBool::new(false),
        });
        inputs.push(input);
        shared.push(display.clone());
        if let Ok(client) = client {
            followed.push(runtime.spawn(follow(index, client, viewer.as_weak(), events, display)));
        }
    }
    viewer.on_toggled(move |index, shown| {
        let Ok(position) = usize::try_from(index) else {
            return;
        };
        if let Some(mut screen) = screens.row_data(position) {
            screen.shown = shown;
            screens.set_row_data(position, screen);
            panes.reset();
        }
        if let Some(display) = shared.get(position) {
            display.shown.store(shown, Ordering::Relaxed);
        }
    });
    let pointers = inputs.clone();
    viewer.on_pointer(move |index, x, y, left, middle, right| {
        let buttons = u8::from(left) | u8::from(middle) << 1 | u8::from(right) << 2;
        send(&pointers, index, testbox_vnc::pointer(x, y, buttons));
    });
    let wheels = inputs.clone();
    viewer.on_wheel(move |index, x, y, up| {
        for event in testbox_vnc::wheel(x, y, up) {
            send(&wheels, index, event);
        }
    });
    viewer.on_key(move |index, text: slint::SharedString, down| {
        if let Some(keysym) = keysym(&text) {
            send(&inputs, index, testbox_vnc::key(keysym, down));
        }
    });
    // The displays' tasks run on the runtime's thread until the window closes.
    viewer.run().describe("the window")?;
    // Its callbacks hold the input senders: dropped, each task sees its input end, and closes
    // its connection.
    drop(viewer);
    runtime.block_on(async {
        for display in followed {
            if let Err(error) = display.await {
                eprintln!("testbox-viewer: a display's task ended: {error}");
            }
        }
    });
    Ok(())
}

/// Where the window still reads once sandboxed: libraries, fonts, cursor themes.
fn readable() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    ["/usr", "/etc", "/proc", "/sys", "/dev"]
        .into_iter()
        .map(PathBuf::from)
        .chain(home.into_iter().flat_map(|home| {
            [
                ".cache/fontconfig",
                ".config/fontconfig",
                ".local/share/fonts",
                ".icons",
                ".local/share/icons",
            ]
            .map(|place| home.join(place))
        }))
        .filter(|place| place.exists())
        .collect()
}

/// Display `index` into the window until it closes: a frame only while its toggle shows it, and
/// one at a time (a slow window takes fewer, never a queue of them); a change left untaken shows
/// with the next tick.
async fn follow(
    index: usize,
    client: VncClient,
    viewer: Weak<Viewer>,
    input: mpsc::UnboundedReceiver<X11Event>,
    display: Arc<Shared>,
) {
    let frame = Mutex::new(Frame::default());
    let ended = testbox_vnc::follow(client, &frame, input, |frame| {
        let ready =
            display.shown.load(Ordering::Relaxed) && !display.in_flight.load(Ordering::Acquire);
        if ready && frame.take_changed().is_some() {
            display.in_flight.store(true, Ordering::Release);
            // The whole frame: Slint's image takes a buffer of its own.
            let pixels = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                frame.rgba(),
                frame.width().into(),
                frame.height().into(),
            );
            let landed = display.clone();
            shown(&viewer, index, Some(pixels), String::new(), move || {
                landed.in_flight.store(false, Ordering::Release);
            });
        }
    })
    .await;
    shown(&viewer, index, None, ended.to_string(), || {});
}

/// Puts display `index`'s frame, if any, and state into the window, then `landed`.
fn shown(
    viewer: &Weak<Viewer>,
    index: usize,
    pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
    state: String,
    landed: impl FnOnce() + Send + 'static,
) {
    let updated = viewer.upgrade_in_event_loop(move |viewer| {
        let screens = viewer.get_screens();
        if let Some(mut screen) = screens.row_data(index) {
            if let Some(pixels) = pixels {
                screen.frame = slint::Image::from_rgba8(pixels);
            }
            screen.state = state.into();
            screens.set_row_data(index, screen);
        }
        landed();
    });
    if let Err(error) = updated {
        eprintln!("testbox-viewer: the window is gone: {error}");
    }
}

fn send(inputs: &[mpsc::UnboundedSender<X11Event>], index: i32, event: X11Event) {
    let input = usize::try_from(index)
        .ok()
        .and_then(|index| inputs.get(index));
    if let Some(input) = input
        && let Err(mpsc::error::SendError(event)) = input.send(event)
    {
        eprintln!("testbox-viewer: display {index} closed, {event:?} not sent");
    }
}

/// The X keysym for a key as Slint gives it: a character, or one of Slint's private-use codes
/// for the other keys (internal/common/key_codes.rs).
fn keysym(text: &str) -> Option<u32> {
    use slint::platform::Key;
    let mut characters = text.chars();
    let (Some(character), None) = (characters.next(), characters.next()) else {
        return None;
    };
    let named = [
        (Key::Backspace, NamedKey::Backspace),
        (Key::Tab, NamedKey::Tab),
        (Key::Return, NamedKey::Return),
        (Key::Escape, NamedKey::Escape),
        (Key::Delete, NamedKey::Delete),
        (Key::Shift, NamedKey::ShiftLeft),
        (Key::ShiftR, NamedKey::ShiftRight),
        (Key::Control, NamedKey::ControlLeft),
        (Key::ControlR, NamedKey::ControlRight),
        (Key::Alt, NamedKey::Alt),
        (Key::AltGr, NamedKey::AltGr),
        (Key::Meta, NamedKey::Meta),
        (Key::LeftArrow, NamedKey::Left),
        (Key::UpArrow, NamedKey::Up),
        (Key::RightArrow, NamedKey::Right),
        (Key::DownArrow, NamedKey::Down),
        (Key::Home, NamedKey::Home),
        (Key::End, NamedKey::End),
        (Key::PageUp, NamedKey::PageUp),
        (Key::PageDown, NamedKey::PageDown),
        (Key::Insert, NamedKey::Insert),
    ];
    if let Some((_, named)) = named.iter().find(|(key, _)| char::from(*key) == character) {
        return Some(named.keysym());
    }
    // Slint's other named keys: none of them is sent.
    if character.is_control() || ('\u{f700}'..='\u{f8ff}').contains(&character) {
        return None;
    }
    Some(testbox_vnc::character_keysym(character))
}
