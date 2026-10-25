//! A box display's VNC client for testbox/viewer and testbox/web: their arguments,
//! the connection to a `with-display` socket, the frame it keeps up to date, the input it sends,
//! and the Landlock both restrict themselves with.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use landlock::{
    ABI, Access, AccessFs, AccessNet, RestrictionStatus, Ruleset, RulesetAttr, RulesetCreatedAttr,
    Scope, path_beneath_rules,
};
use testbox_tools::{Context, Describe, Error, Result};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use vnc::{
    ClientKeyEvent, ClientMouseEvent, PixelFormat, Rect, VncConnector, VncEncoding, VncError,
    VncEvent,
};

pub use vnc::{VncClient, X11Event};

/// How often a display's changes are asked for and taken: vnc-rs's client is polled (its
/// `recv_event` holds the lock `input` needs, and it marks no update's end), at 60 Hz.
pub const FRAME: Duration = Duration::from_millis(16);

/// A display to show: its name and its socket.
pub struct Target {
    pub name: String,
    pub socket: PathBuf,
}

/// The arguments both viewers take, `[--interactive] <name>=<socket>...`: whether input is on,
/// and the displays.
pub fn arguments(tool: &str) -> Result<(bool, Vec<Target>)> {
    let usage = || Error::Failed(format!("usage: {tool} [--interactive] <name>=<socket>..."));
    let mut interactive = false;
    let mut targets = Vec::new();
    for argument in std::env::args().skip(1) {
        if argument == "--interactive" {
            interactive = true;
        } else {
            let (name, socket) = argument.split_once('=').ok_or_else(usage)?;
            targets.push(Target {
                name: name.to_owned(),
                socket: PathBuf::from(socket),
            });
        }
    }
    if targets.is_empty() {
        return Err(usage());
    }
    Ok((interactive, targets))
}

/// Landlock for the rest of the process's life: files only read, and only beneath
/// `readable`; no new Unix socket (ABI 9's `ResolveUnix`); no TCP bind or connect beyond what is
/// already open; no abstract socket or signal beyond itself. Best effort on an older kernel: the
/// status says what holds.
pub fn sandbox(readable: impl IntoIterator<Item = PathBuf>) -> Result<RestrictionStatus> {
    restrict(readable, [])
}

/// [`sandbox`], and files beneath `writable` also written and made: for testbox/shot's
/// screenshots.
pub fn sandbox_writing(writable: PathBuf) -> Result<RestrictionStatus> {
    restrict([], [writable])
}

fn restrict(
    readable: impl IntoIterator<Item = PathBuf>,
    writable: impl IntoIterator<Item = PathBuf>,
) -> Result<RestrictionStatus> {
    let abi = ABI::V9;
    Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .and_then(|ruleset| ruleset.handle_access(AccessNet::from_all(abi)))
        .and_then(|ruleset| ruleset.scope(Scope::from_all(abi)))
        .and_then(|ruleset| ruleset.create())
        .and_then(|ruleset| {
            ruleset.add_rules(path_beneath_rules(readable, AccessFs::from_read(abi)))
        })
        .and_then(|ruleset| {
            ruleset.add_rules(path_beneath_rules(writable, AccessFs::from_all(abi)))
        })
        .and_then(|ruleset| ruleset.restrict_self())
        .describe("Landlock")
}

/// Connects to a display's socket, which must be the box's (`testbox_tools::box_socket`).
pub async fn connect(socket: &Path) -> Result<VncClient> {
    testbox_tools::box_socket(socket)?;
    let stream = UnixStream::connect(socket)
        .await
        .context(socket.display().to_string())?;
    // The server has no authentication (`with-display`), so no credentials are asked for.
    let client =
        VncConnector::<UnixStream, std::future::Ready<Result<String, VncError>>>::new(stream)
            .add_encoding(VncEncoding::CopyRect)
            .add_encoding(VncEncoding::Raw)
            // The cursor's shape apart from the frame: without it Xvnc draws the cursor into the
            // frame, and where it was stays drawn (vnc-rs `X11Event::FullRefresh`'s "cursor
            // ghosts").
            .add_encoding(VncEncoding::CursorPseudo)
            .allow_shared(true)
            .set_pixel_format(PixelFormat::rgba())
            .build()
            .describe("a VNC connection")?
            .try_start()
            .await
            .describe("a VNC handshake")?
            .finish()
            .describe("a VNC connection")?;
    // The whole screen once: incremental updates bring only what changed since the last update,
    // so what never changes would never show.
    client
        .input(X11Event::FullRefresh)
        .await
        .describe("asking for the whole screen")?;
    Ok(client)
}

/// Takes the display's changes into `frame`, a frame's time at a time, and `input` to the
/// display, until either ends; then closes the connection and says why it ended. `changed` sees
/// the frame after each tick, under its lock: it takes the change (`Frame::take_changed`) when
/// it shows it, and leaves it for a later tick otherwise.
pub async fn follow(
    client: VncClient,
    frame: &Mutex<Frame>,
    mut input: mpsc::UnboundedReceiver<X11Event>,
    mut changed: impl FnMut(&mut Frame),
) -> Error {
    let mut tick = tokio::time::interval(FRAME);
    // A tick that came late asks once, not once for each one missed.
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let ended = loop {
        tokio::select! {
            _ = tick.tick() => {
                if let Err(error) = take_changes(&client, frame, &mut changed).await {
                    break error;
                }
            }
            event = input.recv() => {
                let Some(event) = event else {
                    break Error::Failed("its viewer closed".to_owned());
                };
                if let Err(error) = client.input(event).await {
                    break Error::Failed(format!("input: {error}"));
                }
            }
        }
    };
    if let Err(error) = client.close().await {
        eprintln!("testbox-vnc: closing a display: {error}");
    }
    ended
}

/// How long a display stays quiet before [`snapshot`] takes its frame as whole: vnc-rs marks no
/// update's end, so the frame is whole once all of it came and nothing more has for this long.
const SETTLED: Duration = Duration::from_millis(200);

/// The whole display once, from a client just connected ([`connect`]), for a screenshot; fails
/// if it isn't whole within `patience`. Closes the connection.
pub async fn snapshot(client: VncClient, patience: Duration) -> Result<Frame> {
    let mut frame = Frame::default();
    let taken = tokio::time::timeout(patience, async {
        let mut quiet = Duration::ZERO;
        loop {
            let mut came = false;
            while let Some(event) = client.poll_event().await.describe("the display")? {
                frame.apply(event)?;
                came = true;
            }
            quiet = if came { Duration::ZERO } else { quiet + FRAME };
            if frame.width > 0 && frame.changed == Some(frame.whole()) && quiet >= SETTLED {
                return Ok(());
            }
            tokio::time::sleep(FRAME).await;
        }
    })
    .await;
    if let Err(error) = client.close().await {
        eprintln!("testbox-vnc: closing a display: {error}");
    }
    match taken {
        Ok(Ok(())) => Ok(frame),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(Error::Failed(format!(
            "not all of the display came within {patience:?}"
        ))),
    }
}

/// What the display sent since the last time, into `frame`; then the next incremental update
/// asked for.
async fn take_changes(
    client: &VncClient,
    frame: &Mutex<Frame>,
    changed: &mut impl FnMut(&mut Frame),
) -> Result<()> {
    // Taken before the frame is locked: polling awaits.
    let mut events = Vec::new();
    while let Some(event) = client.poll_event().await.describe("the display")? {
        events.push(event);
    }
    {
        let mut frame = frame
            .lock()
            .map_err(|_| Error::Failed("the frame's lock".to_owned()))?;
        for event in events {
            frame.apply(event)?;
        }
        changed(&mut frame);
    }
    client
        .input(X11Event::Refresh)
        .await
        .describe("asking for an update")
}

/// The pointer at (x, y) of the frame's pixels with `buttons` down (RFC 6143 §7.5.5's mask);
/// out of the frame is at its edge.
pub fn pointer(x: f32, y: f32, buttons: u8) -> X11Event {
    let at = |position: f32| position.clamp(0.0, f32::from(u16::MAX)) as u16;
    X11Event::PointerEvent(ClientMouseEvent {
        position_x: at(x),
        position_y: at(y),
        bottons: buttons,
    })
}

/// A wheel step: a press and release of button 4 (up) or 5 (down) (RFC 6143 §7.5.5).
pub fn wheel(x: f32, y: f32, up: bool) -> [X11Event; 2] {
    let step = if up { 1 << 3 } else { 1 << 4 };
    [pointer(x, y, step), pointer(x, y, 0)]
}

/// A key `keysym` pressed or released.
pub fn key(keysym: u32, down: bool) -> X11Event {
    X11Event::KeyEvent(ClientKeyEvent {
        keycode: keysym,
        down,
    })
}

/// The keysym of a character: Latin-1 is its own, the rest of Unicode 0x01000000 above it
/// (X11 keysymdef.h).
pub fn character_keysym(character: char) -> u32 {
    let code = u32::from(character);
    if code < 0x100 {
        code
    } else {
        0x0100_0000 + code
    }
}

/// The keys besides characters the box's apps need, each viewer naming them its own way.
#[derive(Clone, Copy, Debug)]
pub enum NamedKey {
    Backspace,
    Tab,
    Return,
    Escape,
    Delete,
    Home,
    Left,
    Up,
    Right,
    Down,
    PageUp,
    PageDown,
    End,
    Insert,
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    Alt,
    AltGr,
    Meta,
}

impl NamedKey {
    /// X11 keysymdef.h's.
    pub fn keysym(self) -> u32 {
        match self {
            Self::Backspace => 0xff08,
            Self::Tab => 0xff09,
            Self::Return => 0xff0d,
            Self::Escape => 0xff1b,
            Self::Delete => 0xffff,
            Self::Home => 0xff50,
            Self::Left => 0xff51,
            Self::Up => 0xff52,
            Self::Right => 0xff53,
            Self::Down => 0xff54,
            Self::PageUp => 0xff55,
            Self::PageDown => 0xff56,
            Self::End => 0xff57,
            Self::Insert => 0xff63,
            Self::ShiftLeft => 0xffe1,
            Self::ShiftRight => 0xffe2,
            Self::ControlLeft => 0xffe3,
            Self::ControlRight => 0xffe4,
            Self::Alt => 0xffe9,
            Self::AltGr => 0xfe03,
            Self::Meta => 0xffeb,
        }
    }
}

/// A part of a frame, in its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Region {
    /// Both, as one: the regions are within the frame (`Frame::apply` checks them), so the sums
    /// stay within `u16`.
    fn union(self, other: Region) -> Region {
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Region {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }
}

impl From<Rect> for Region {
    fn from(rect: Rect) -> Self {
        Region {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

/// A display's pixels as RGBA, opaque, and the part that changed since it was last taken.
#[derive(Default)]
pub struct Frame {
    width: u16,
    height: u16,
    rgba: Vec<u8>,
    changed: Option<Region>,
}

impl Frame {
    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// All of it, row by row.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// All of it, as a region.
    pub fn whole(&self) -> Region {
        Region {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }
    }

    /// The part that changed since the last call, if any.
    pub fn take_changed(&mut self) -> Option<Region> {
        self.changed.take()
    }

    /// Appends `region`'s pixels, row by row, to `out`.
    pub fn write_region(&self, region: Region, out: &mut Vec<u8>) -> Result<()> {
        out.reserve(usize::from(region.width) * usize::from(region.height) * 4);
        for line in 0..region.height {
            let row = self.row(region.x, below(region.y, line), region.width)?;
            out.extend_from_slice(&self.rgba[row]);
        }
        Ok(())
    }

    fn apply(&mut self, event: VncEvent) -> Result<()> {
        let region = match event {
            VncEvent::SetResolution(screen) => {
                self.width = screen.width;
                self.height = screen.height;
                self.rgba = vec![0; usize::from(screen.width) * usize::from(screen.height) * 4];
                // All of it, in place of what changed at the old size.
                self.changed = Some(self.whole());
                return Ok(());
            }
            VncEvent::RawImage(rect, data) => {
                self.draw(rect, &data)?;
                rect.into()
            }
            VncEvent::Copy(destination, source) => {
                self.copy(destination, source)?;
                destination.into()
            }
            VncEvent::Error(error) => return Err(Error::Failed(error)),
            // The viewer's own pointer stands in for the display's.
            VncEvent::SetCursor(..) => return Ok(()),
            // Not asked for (`connect`'s encodings), or nothing to show.
            other => {
                eprintln!("testbox-vnc: not shown: {other:?}");
                return Ok(());
            }
        };
        self.changed = Some(match self.changed {
            Some(changed) => changed.union(region),
            None => region,
        });
        Ok(())
    }

    /// Row `y`'s pixels from `x` on for `width`, in `rgba`; an error past the frame. `y` is a
    /// `usize`: the coordinates are the server's, and their sums may not fit a `u16`.
    fn row(&self, x: u16, y: usize, width: u16) -> Result<std::ops::Range<usize>> {
        let start = (y * usize::from(self.width) + usize::from(x)) * 4;
        let end = start + usize::from(width) * 4;
        if usize::from(x) + usize::from(width) > usize::from(self.width) || end > self.rgba.len() {
            return Err(Error::Failed(format!(
                "a rectangle at ({x}, {y}) past the {}x{} frame",
                self.width, self.height
            )));
        }
        Ok(start..end)
    }

    fn draw(&mut self, rect: Rect, data: &[u8]) -> Result<()> {
        let stride = usize::from(rect.width) * 4;
        for line in 0..rect.height {
            let pixels = self.row(rect.x, below(rect.y, line), rect.width)?;
            let offset = usize::from(line) * stride;
            let source = data
                .get(offset..offset + stride)
                .ok_or("a rectangle shorter than its size")?;
            let target = &mut self.rgba[pixels];
            target.copy_from_slice(source);
            // The server's alpha byte is padding (RFC 6143 §7.4: depth 24 in 32 bits).
            for pixel in target.as_chunks_mut::<4>().0 {
                pixel[3] = u8::MAX;
            }
        }
        Ok(())
    }

    /// Within the frame, row by row; from the bottom when the rows move down, so no row is
    /// overwritten before it is copied. Both rectangles are checked before anything moves.
    fn copy(&mut self, destination: Rect, source: Rect) -> Result<()> {
        if source.height == 0 {
            return Ok(());
        }
        let last = source.height - 1;
        for (rect, line) in [
            (source, 0),
            (source, last),
            (destination, 0),
            (destination, last),
        ] {
            self.row(rect.x, below(rect.y, line), source.width)?;
        }
        let mut move_row = |line: u16| -> Result<()> {
            let from = self.row(source.x, below(source.y, line), source.width)?;
            let to = self.row(destination.x, below(destination.y, line), source.width)?;
            self.rgba.copy_within(from, to.start);
            Ok(())
        };
        if destination.y > source.y {
            (0..source.height).rev().try_for_each(&mut move_row)
        } else {
            (0..source.height).try_for_each(&mut move_row)
        }
    }
}

/// The row `line` rows below `y`, which may be past `u16`.
fn below(y: u16, line: u16) -> usize {
    usize::from(y) + usize::from(line)
}
