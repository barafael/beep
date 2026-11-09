//! The box's displays as a web page, for a browser:
//! `testbox-web [--interactive] <name>=<socket>...` connects to each display's socket, serves the
//! page on loopback only, and prints its URL, a token in its fragment. Once listening and
//! connected, it restricts itself with Landlock (`testbox_vnc::sandbox`): no file, no new socket.
//!
//! What a page on another site could try, and what stops it (OWASP):
//! - a WebSocket to this port (WebSockets aren't held to the same-origin policy): its `Origin`
//!   must be this page's, and its first message the token;
//! - a name of its own that resolves here (DNS rebinding): every request's `Host` must be this
//!   page's;
//! - the page in a frame of its own (clickjacking): `frame-ancestors 'none'`;
//! - scripts injected into the page: the CSP allows this server's script only, never inline.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use testbox_tools::Report;
use testbox_vnc::{Frame, FrameError, NamedKey, Region, VncClient, X11Event};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;

/// The page's name: every `*.localhost` resolves to loopback (RFC 6761 §6.3; Firefox and Chrome
/// without asking a resolver), so no file of the host's changes. Revise if a browser resolves
/// `*.localhost` elsewhere.
const HOST_NAME: &str = "view.testbox.localhost";
/// How long a new WebSocket has for the token.
const TOKEN_PATIENCE: Duration = Duration::from_secs(5);
/// Messages a slow page may fall behind by, each up to a whole frame (a second of small
/// updates), before it is sent the whole frame again instead.
const BACKLOG: usize = 16;

/// The page's files, built in.
#[derive(Clone, Copy)]
enum Asset {
    Index,
    Script,
    Style,
}

impl Asset {
    const ALL: [Self; 3] = [Self::Index, Self::Script, Self::Style];

    fn route(self) -> &'static str {
        match self {
            Self::Index => "/",
            Self::Script => "/viewer.js",
            Self::Style => "/viewer.css",
        }
    }

    fn response(self) -> Response {
        let (content_type, body) = match self {
            Self::Index => (
                "text/html; charset=utf-8",
                include_str!("../page/index.html"),
            ),
            Self::Script => (
                "text/javascript; charset=utf-8",
                include_str!("../page/viewer.js"),
            ),
            Self::Style => (
                "text/css; charset=utf-8",
                include_str!("../page/viewer.css"),
            ),
        };
        ([(header::CONTENT_TYPE, content_type)], body).into_response()
    }
}

fn main() -> std::process::ExitCode {
    testbox_tools::exit("testbox-web", run())
}

/// What the server itself fails at; testbox-vnc's failures come as its own.
#[derive(Debug, thiserror::Error)]
enum WebError {
    #[error("starting a tokio runtime")]
    Runtime {
        #[source]
        source: std::io::Error,
    },
    #[error("listening on {address}")]
    Bind {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("finding the port 127.0.0.1 listens on")]
    Port {
        #[source]
        source: std::io::Error,
    },
    /// The page's messages name a display in a byte.
    #[error("{count} displays, more than a byte counts")]
    TooManyDisplays { count: usize },
    #[error("drawing random bytes for the token")]
    Token {
        #[source]
        source: getrandom::Error,
    },
    #[error("serving on {address}")]
    Serve {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// Why a page's session ended early: it is refused, or its WebSocket failed.
#[derive(Debug, thiserror::Error)]
enum PageError {
    #[error("no token within {patience:?}")]
    TokenTimeout { patience: Duration },
    #[error("the page closed before its token")]
    ClosedBeforeToken,
    /// Its first message, of another kind than text.
    #[error("a {kind} message in place of the token")]
    TokenNotText { kind: &'static str },
    /// Its first message, text but not a token message. Its text isn't kept: it may hold the
    /// token, mistyped.
    #[error("reading the token message")]
    ParseToken {
        #[source]
        source: serde_json::Error,
    },
    #[error("input before the token")]
    InputBeforeToken,
    #[error("a wrong token")]
    WrongToken,
    #[error("receiving from the page")]
    Receive {
        #[source]
        source: axum::Error,
    },
    #[error("sending to the page")]
    Send {
        #[source]
        source: axum::Error,
    },
    #[error("reading input {text:?}")]
    ParseInput {
        text: String,
        #[source]
        source: serde_json::Error,
    },
}

/// What a display's task shares with the pages: its frame as it is now, its changes as they
/// come, ready to send, and the way to its input.
struct Display {
    index: u8,
    name: String,
    frame: Mutex<Frame>,
    /// Why it ended, once it has: for a page that comes after, or falls behind past it.
    ended: Mutex<Option<String>>,
    updates: broadcast::Sender<Message>,
    input: mpsc::UnboundedSender<X11Event>,
}

struct Served {
    host: String,
    origin: String,
    token: String,
    interactive: bool,
    displays: Vec<Arc<Display>>,
}

/// The server, until it fails; any failure, to be reported, whoever's type it is.
fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (interactive, targets) = testbox_vnc::arguments("testbox-web")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| WebError::Runtime { source })?;
    runtime.block_on(serve(interactive, targets))
}

/// [`run`]'s async part: its failures, too, only to be reported.
async fn serve(
    interactive: bool,
    targets: Vec<testbox_vnc::Target>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Loopback only; the port the kernel picks, on both families (this host resolves
    // `*.localhost` to ::1 first).
    let bind = async |address: SocketAddr| {
        TcpListener::bind(address)
            .await
            .map_err(|source| WebError::Bind { address, source })
    };
    let ipv4 = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
    let port = ipv4
        .local_addr()
        .map_err(|source| WebError::Port { source })?
        .port();
    let ipv6_address = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    let ipv6 = bind(ipv6_address).await?;
    let ipv4_address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut displays = Vec::new();
    let mut connected = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let (input, events) = mpsc::unbounded_channel();
        let display = Arc::new(Display {
            index: u8::try_from(index).map_err(|_| WebError::TooManyDisplays {
                count: targets.len(),
            })?,
            name: target.name.clone(),
            frame: Mutex::new(Frame::default()),
            ended: Mutex::new(None),
            updates: broadcast::channel(BACKLOG).0,
            input,
        });
        match testbox_vnc::connect(&target.socket).await {
            Ok(client) => connected.push((display.clone(), client, events)),
            Err(error) => eprintln!("testbox-web: {}: {}", target.name, Report(&error)),
        }
        displays.push(display);
    }
    let token = token()?;
    // No file at all: the page is in the binary.
    let restricted = testbox_vnc::sandbox([])?;
    eprintln!(
        "testbox-web: Landlock {:?}, no_new_privs {}",
        restricted.ruleset, restricted.no_new_privs
    );
    for (display, client, events) in connected {
        tokio::spawn(follow(display, client, events));
    }
    let host = format!("{HOST_NAME}:{port}");
    let served = Arc::new(Served {
        origin: format!("http://{host}"),
        host,
        token,
        interactive,
        displays,
    });
    eprintln!("testbox-web: the displays, until this ends (the token is for this run only):");
    println!("http://{}/#{}", served.host, served.token);
    let app = Asset::ALL
        .into_iter()
        .fold(Router::new(), |router, asset| {
            router.route(asset.route(), get(move || async move { asset.response() }))
        })
        .route("/ws", get(websocket))
        .layer(middleware::from_fn_with_state(served.clone(), guard))
        .with_state(served);
    let on_ipv6 = app.clone();
    tokio::try_join!(
        async {
            axum::serve(ipv4, app)
                .await
                .map_err(|source| WebError::Serve {
                    address: ipv4_address,
                    source,
                })
        },
        async {
            axum::serve(ipv6, on_ipv6)
                .await
                .map_err(|source| WebError::Serve {
                    address: ipv6_address,
                    source,
                })
        },
    )?;
    Ok(())
}

/// 32 random bytes, in hex.
fn token() -> Result<String, WebError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|source| WebError::Token { source })?;
    Ok(hex::encode(bytes))
}

/// Refuses a request for any other name (DNS rebinding), and gives every answer the headers that
/// keep the page to itself.
async fn guard(State(served): State<Arc<Served>>, request: Request, next: Next) -> Response {
    let host = request.headers().get(header::HOST);
    if host.is_none_or(|host| host.as_bytes() != served.host.as_bytes()) {
        return (StatusCode::MISDIRECTED_REQUEST, "not this server's name").into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let fixed = [
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
             img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CACHE_CONTROL, "no-store"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ),
        (
            HeaderName::from_static("cross-origin-resource-policy"),
            "same-origin",
        ),
    ];
    for (name, value) in fixed {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

async fn websocket(
    State(served): State<Arc<Served>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    // Cross-site WebSocket hijacking: only this page may open one.
    let origin = headers.get(header::ORIGIN);
    if origin.is_none_or(|origin| origin.as_bytes() != served.origin.as_bytes()) {
        return (StatusCode::FORBIDDEN, "not this page's origin").into_response();
    }
    upgrade.on_upgrade(move |socket| async move {
        if let Err(error) = page_session(served, socket).await {
            eprintln!("testbox-web: a page: {}", Report(&error));
        }
    })
}

/// What the page sends: the token first, then input.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FromPage {
    Token {
        token: String,
    },
    Pointer {
        display: usize,
        x: f32,
        y: f32,
        buttons: u8,
    },
    Wheel {
        display: usize,
        x: f32,
        y: f32,
        up: bool,
    },
    Key {
        display: usize,
        key: String,
        down: bool,
    },
}

/// What the page is sent besides frames.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToPage<'served> {
    Hello {
        displays: Vec<&'served str>,
        interactive: bool,
    },
    Resize {
        display: u8,
        width: u16,
        height: u16,
    },
    State {
        display: u8,
        state: &'served str,
    },
}

/// `message` as a WebSocket text message; none if it doesn't serialize, which is said.
fn json(message: &ToPage<'_>) -> Option<Message> {
    match serde_json::to_string(message) {
        Ok(text) => Some(Message::Text(text.into())),
        Err(error) => {
            eprintln!("testbox-web: a message to the page: {error}");
            None
        }
    }
}

/// A region's pixels as the page draws them: [u8 display][u16 x][u16 y][u16 width][u16 height],
/// little-endian, then RGBA row by row (page/viewer.js `draw`).
fn pixels(index: u8, region: Region, frame: &Frame) -> Result<Message, FrameError> {
    let mut message = vec![index];
    for value in [region.x, region.y, region.width, region.height] {
        message.extend_from_slice(&value.to_le_bytes());
    }
    frame.write_region(region, &mut message)?;
    Ok(Message::Binary(message.into()))
}

async fn page_session(served: Arc<Served>, mut socket: WebSocket) -> Result<(), PageError> {
    let first = tokio::time::timeout(TOKEN_PATIENCE, socket.recv())
        .await
        .map_err(|_elapsed| PageError::TokenTimeout {
            patience: TOKEN_PATIENCE,
        })?
        .ok_or(PageError::ClosedBeforeToken)?
        .map_err(|source| PageError::Receive { source })?;
    let first = match first {
        Message::Text(first) => first,
        Message::Binary(_) => return Err(PageError::TokenNotText { kind: "binary" }),
        Message::Ping(_) => return Err(PageError::TokenNotText { kind: "ping" }),
        Message::Pong(_) => return Err(PageError::TokenNotText { kind: "pong" }),
        Message::Close(_) => return Err(PageError::TokenNotText { kind: "close" }),
    };
    let FromPage::Token { token } = serde_json::from_str::<FromPage>(first.as_str())
        .map_err(|source| PageError::ParseToken { source })?
    else {
        return Err(PageError::InputBeforeToken);
    };
    if !same(token.as_bytes(), served.token.as_bytes()) {
        return Err(PageError::WrongToken);
    }
    let hello = ToPage::Hello {
        displays: served
            .displays
            .iter()
            .map(|display| display.name.as_str())
            .collect(),
        interactive: served.interactive,
    };
    if let Some(hello) = json(&hello) {
        socket
            .send(hello)
            .await
            .map_err(|source| PageError::Send { source })?;
    }
    let (frames, mut updates) = mpsc::channel(BACKLOG);
    // Ended with the page.
    let mut relays = JoinSet::new();
    for display in &served.displays {
        relays.spawn(relay(display.clone(), frames.clone()));
    }
    drop(frames);
    loop {
        tokio::select! {
            outgoing = updates.recv() => {
                let Some(message) = outgoing else {
                    return Ok(());
                };
                socket
                    .send(message)
                    .await
                    .map_err(|source| PageError::Send { source })?;
            }
            incoming = socket.recv() => {
                let Some(message) = incoming else {
                    return Ok(());
                };
                match message.map_err(|source| PageError::Receive { source })? {
                    Message::Text(text) if served.interactive => {
                        let input = serde_json::from_str::<FromPage>(text.as_str())
                            .map_err(|source| PageError::ParseInput {
                                text: text.as_str().to_owned(),
                                source,
                            })?;
                        take_input(&served, input);
                    }
                    Message::Close(_) => return Ok(()),
                    // Input while it is off, pings, pongs: nothing to do.
                    _ => {}
                }
            }
        }
    }
}

/// Constant-time equality, so the comparison's time tells nothing about the token.
fn same(given: &[u8], token: &[u8]) -> bool {
    given.len() == token.len()
        && given
            .iter()
            .zip(token)
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0
}

/// A display's frame for one page: the whole of it now, then its changes; the whole again if
/// the page falls `BACKLOG` behind.
async fn relay(display: Arc<Display>, page: mpsc::Sender<Message>) {
    loop {
        // Subscribed while the frame is held: nothing comes between the copy and the changes.
        let (mut changes, whole) = {
            let Ok(frame) = display.frame.lock() else {
                return;
            };
            (display.updates.subscribe(), whole_frame(&display, &frame))
        };
        let ended = display.ended.lock().ok().and_then(|ended| ended.clone());
        let state = ended.and_then(|state| {
            json(&ToPage::State {
                display: display.index,
                state: &state,
            })
        });
        for message in whole.into_iter().chain(state) {
            if page.send(message).await.is_err() {
                return;
            }
        }
        loop {
            match changes.recv().await {
                // A page slower than the display holds this up, and its changes fall behind
                // into `Lagged`.
                Ok(message) => {
                    if page.send(message).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => break,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

fn whole_frame(display: &Display, frame: &Frame) -> Vec<Message> {
    let resized = json(&ToPage::Resize {
        display: display.index,
        width: frame.width(),
        height: frame.height(),
    });
    let whole = pixels(display.index, frame.whole(), frame)
        .inspect_err(|error| {
            eprintln!("testbox-web: {}'s frame: {}", display.name, Report(error));
        })
        .ok();
    resized.into_iter().chain(whole).collect()
}

/// Takes a display's changes into its frame and to the pages, a message each, and the pages'
/// input to it, until it closes.
async fn follow(
    display: Arc<Display>,
    client: VncClient,
    input: mpsc::UnboundedReceiver<X11Event>,
) {
    let (mut width, mut height) = (0, 0);
    let ended = testbox_vnc::follow(client, &display.frame, input, |frame| {
        let Some(region) = frame.take_changed() else {
            return;
        };
        if (frame.width(), frame.height()) != (width, height) {
            (width, height) = (frame.width(), frame.height());
            if let Some(resized) = json(&ToPage::Resize {
                display: display.index,
                width,
                height,
            }) {
                publish(&display, resized);
            }
        }
        match pixels(display.index, region, frame) {
            Ok(message) => publish(&display, message),
            Err(error) => {
                let state = Report(&error).to_string();
                eprintln!("testbox-web: {}: {state}", display.name);
                publish_state(&display, &state);
            }
        }
    })
    .await;
    let state = Report(&ended).to_string();
    if let Ok(mut kept) = display.ended.lock() {
        *kept = Some(state.clone());
    }
    publish_state(&display, &state);
}

/// Tells the pages why a display shows nothing new.
fn publish_state(display: &Display, state: &str) {
    if let Some(message) = json(&ToPage::State {
        display: display.index,
        state,
    }) {
        publish(display, message);
    }
}

fn publish(display: &Display, message: Message) {
    match display.updates.send(message) {
        Ok(_pages) => {}
        // No page subscribed: the next one starts from the frame.
        Err(broadcast::error::SendError(_message)) => {}
    }
}

fn take_input(served: &Served, input: FromPage) {
    let (display, events) = match input {
        FromPage::Pointer {
            display,
            x,
            y,
            buttons,
        } => (display, vec![testbox_vnc::pointer(x, y, buttons)]),
        FromPage::Wheel { display, x, y, up } => (display, testbox_vnc::wheel(x, y, up).to_vec()),
        FromPage::Key { display, key, down } => match dom_keysym(&key) {
            Some(keysym) => (display, vec![testbox_vnc::key(keysym, down)]),
            None => return,
        },
        FromPage::Token { .. } => return,
    };
    let Some(target) = served.displays.get(display) else {
        return;
    };
    for event in events {
        if target.input.send(event).is_err() {
            eprintln!("testbox-web: {} closed, input not sent", target.name);
        }
    }
}

/// The X keysym for a DOM `KeyboardEvent.key`: one character, or a key value the box's apps need
/// (UI Events KeyboardEvent key values).
fn dom_keysym(key: &str) -> Option<u32> {
    let mut characters = key.chars();
    if let (Some(character), None) = (characters.next(), characters.next()) {
        return Some(testbox_vnc::character_keysym(character));
    }
    let named = match key {
        "Backspace" => NamedKey::Backspace,
        "Tab" => NamedKey::Tab,
        "Enter" => NamedKey::Return,
        "Escape" => NamedKey::Escape,
        "Delete" => NamedKey::Delete,
        "Home" => NamedKey::Home,
        "ArrowLeft" => NamedKey::Left,
        "ArrowUp" => NamedKey::Up,
        "ArrowRight" => NamedKey::Right,
        "ArrowDown" => NamedKey::Down,
        "PageUp" => NamedKey::PageUp,
        "PageDown" => NamedKey::PageDown,
        "End" => NamedKey::End,
        "Insert" => NamedKey::Insert,
        "Shift" => NamedKey::ShiftLeft,
        "Control" => NamedKey::ControlLeft,
        "Alt" => NamedKey::Alt,
        "AltGraph" => NamedKey::AltGr,
        "Meta" => NamedKey::Meta,
        _ => return None,
    };
    Some(named.keysym())
}
