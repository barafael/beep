//! Drives the real app over the agent harness: the mock backend in, actions in, snapshots
//! out. Runs only with `--features harness` (otherwise this file is empty); the window draws
//! on Qt's offscreen platform, since a test has no desktop, and every file it writes goes to
//! a scratch home, so a test leaves nothing behind.

#![cfg(all(feature = "harness", target_family = "unix"))]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long any single wait may take: the mock needs a couple of seconds to link, its echo a
/// few more to come back.
const PATIENCE: Duration = Duration::from_secs(60);

/// The app under the test. Killed and taken down whatever happens, panic included.
struct App {
    child: Child,
    scratch: PathBuf,
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// One connection to the harness: requests out, JSON lines in.
struct Agent {
    stream: UnixStream,
    lines: BufReader<UnixStream>,
    next_id: u64,
}

/// Starts b33p with the mock backend and connects to its harness.
fn app(name: &str) -> (App, Agent) {
    let scratch = std::env::temp_dir().join(format!("b33p-harness-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    for dir in ["config", "data", "cache", "runtime"] {
        std::fs::create_dir_all(scratch.join(dir)).unwrap();
    }
    let socket = scratch.join("agent.sock");
    let child = Command::new(env!("CARGO_BIN_EXE_b33p"))
        .env("B33P_MOCK", "1")
        .env("B33P_AGENT_SOCKET", &socket)
        .env("XDG_CONFIG_HOME", scratch.join("config"))
        .env("XDG_DATA_HOME", scratch.join("data"))
        .env("XDG_CACHE_HOME", scratch.join("cache"))
        .env("XDG_RUNTIME_DIR", scratch.join("runtime"))
        // A bus of its own that isn't there: b33p can't tell then whether another copy runs
        // and starts anyway. In a shared session the mock name would be taken once, and every
        // later app would bow out at once.
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", scratch.join("bus").display()),
        )
        .env("QT_QPA_PLATFORM", "offscreen")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("b33p starts");
    let deadline = Instant::now() + PATIENCE;
    let stream = loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => panic!("no agent socket at {}: {error}", socket.display()),
        }
    };
    let writer = stream.try_clone().expect("a second handle to the socket");
    // Reads come back at least once a second, so a quiet app can't stall the test past its
    // deadlines; the read methods treat a timeout as "not yet".
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("the socket takes a read timeout");
    (
        App { child, scratch },
        Agent {
            stream: writer,
            lines: BufReader::new(stream),
            next_id: 1,
        },
    )
}

impl Agent {
    fn send(&mut self, line: &str) {
        writeln!(self.stream, "{line}").expect("the socket takes writes");
    }

    /// Sends one request and waits for its answer, skipping pushed updates on the way.
    fn call(&mut self, request: &str) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&format!("{{\"id\":{id},{request}}}"));
        self.answer(id)
    }

    /// Reads lines until the answer to `id` comes.
    fn answer(&mut self, id: u64) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let value = self.line(deadline);
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    }

    /// The next pushed update that satisfies `until`, skipping unrelated ones on the way —
    /// the backend produces redraws of its own (read receipts, …), which an agent waiting
    /// for one thing must wade past.
    fn next_event_matching(&mut self, what: &str, until: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let value = self.line(deadline);
            if value.get("event").is_some() && until(&value) {
                return value;
            }
            assert!(Instant::now() < deadline, "no update {what}");
        }
    }

    fn line(&mut self, deadline: Instant) -> Value {
        loop {
            let mut line = String::new();
            match self.lines.read_line(&mut line) {
                Ok(0) => panic!("the app closed the connection"),
                Ok(_) => {
                    return serde_json::from_str(&line).expect("a JSON line");
                }
                // The read timeout: not yet, but the deadline still holds.
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(error) => panic!("the socket answers: {error}"),
            }
            assert!(Instant::now() < deadline, "the app went quiet");
        }
    }

    fn result(&mut self, request: &str) -> Value {
        self.call(request)["result"].clone()
    }

    fn act(&mut self, action: &str) -> Value {
        self.call(&format!("\"method\":\"act\",\"action\":{action}"))
    }

    fn snapshot(&mut self) -> Value {
        self.result("\"method\":\"snapshot\"")
    }

    /// Polls for the state to come to satisfy `until`, e.g. for the backend to catch up.
    fn wait_for(&mut self, what: &str, until: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let snapshot = self.snapshot();
            if until(&snapshot) {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "never {what}: {snapshot}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn screen(snapshot: &Value) -> &str {
    snapshot["screen"]["name"].as_str().unwrap_or("")
}

/// The message with this exact text, if it is on screen.
fn message<'a>(snapshot: &'a Value, text: &str) -> Option<&'a Value> {
    snapshot["messages"]
        .as_array()?
        .iter()
        .find(|m| m["text"].as_str() == Some(text))
}

/// The mock links, then shows its canned chats.
fn wait_for_chats(agent: &mut Agent) -> Value {
    agent.wait_for("the mock's chats", |s| screen(s) == "chats")
}

fn id_of(chats: &Value, title: &str) -> String {
    chats["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["title"].as_str() == Some(title))
        .unwrap_or_else(|| panic!("a chat called {title}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn selected_is(snapshot: &Value, id: &str) -> bool {
    snapshot["selected"].as_str() == Some(id)
}

/// Starting, connecting, and reading are the test; also that nonsense is refused.
#[test]
fn an_agent_sees_the_chats() {
    let (_app, mut agent) = app("chats");
    let ping = agent.result("\"method\":\"ping\"");
    assert_eq!(ping["mock"], true);
    let error = agent.call("\"method\":\"teleport\"");
    assert!(error.get("error").is_some());
    let chats = wait_for_chats(&mut agent);
    assert!(chats["conversations"].as_array().unwrap().len() > 3);
    let alice = id_of(&chats, "Alice");
    let chats = agent.snapshot();
    let alice_row = chats["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_str() == Some(&alice))
        .unwrap();
    assert_eq!(alice_row["group"], false);
    assert_eq!(alice_row["request"], false);
    // Note to Self is there and known as such.
    let own = chats["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["title"].as_str() == Some("Note to Self"))
        .unwrap();
    assert_eq!(own["note_to_self"], true);
}

/// The whole round trip of a message: typed, sent, delivered, read, answered.
#[test]
fn a_sent_message_is_answered() {
    let (_app, mut agent) = app("send");
    let chats = wait_for_chats(&mut agent);
    let alice = id_of(&chats, "Alice");
    agent.act(&format!(
        "{{\"type\":\"select\",\"conversation\":\"{alice}\"}}"
    ));
    assert!(selected_is(
        &agent.wait_for("Alice selected", |s| selected_is(s, &alice)),
        &alice
    ));
    agent.act("{\"type\":\"type\",\"text\":\"Hello\"}");
    assert_eq!(agent.snapshot()["draft"], "Hello");
    agent.act("{\"type\":\"send\"}");
    assert_eq!(agent.snapshot()["draft"], "");
    let read = agent.wait_for("the message read", |s| {
        message(s, "Hello").and_then(|m| m["delivery"].as_str()) == Some("read")
    });
    assert_eq!(message(&read, "Hello").unwrap()["outgoing"], true);
    // The mock answers in person.
    agent.wait_for("the echo", |s| message(s, "You said: Hello").is_some());
}

/// A group joined through its invite link: looked up, asked to join, then taken back.
#[test]
fn a_group_is_joined_through_its_link() {
    let (_app, mut agent) = app("join");
    wait_for_chats(&mut agent);
    agent.act("{\"type\":\"open_join_group\",\"link\":\"https://signal.group/#mock-approve\"}");
    let looked_up = agent.wait_for("the link looked up", |s| s["join"]["title"].is_string());
    assert_eq!(looked_up["join"]["title"], "Climbing crew");
    assert_eq!(looked_up["join"]["approval"], true);
    agent.act("{\"type\":\"join_group\"}");
    let asked = agent.wait_for("the group there, asked to join", |s| {
        s["join"].is_null()
            && s["conversations"].as_array().is_some_and(|all| {
                all.iter()
                    .any(|c| c["title"] == "Climbing crew" && c["requested"] == true)
            })
    });
    let group = id_of(&asked, "Climbing crew");
    assert!(selected_is(&asked, &group), "the group opens");
    agent.act("{\"type\":\"cancel_join_request\"}");
    let taken_back = agent.wait_for("the request taken back", |s| {
        message(s, "You took back the request to join.").is_some()
    });
    let listed = taken_back["conversations"]
        .as_array()
        .and_then(|all| all.iter().find(|c| c["id"] == group.as_str()))
        .expect("the group is still listed");
    assert_eq!(listed["requested"], false);
    assert_eq!(listed["left"], true);
}

/// Whatever an agent does, a subscribed agent hears of it, numbered on.
#[test]
fn a_subscriber_hears_updates() {
    let (_app, mut agent) = app("subscribe");
    let chats = wait_for_chats(&mut agent);
    let alice = id_of(&chats, "Alice");
    let bob = id_of(&chats, "Bob");
    agent.act(&format!(
        "{{\"type\":\"select\",\"conversation\":\"{alice}\"}}"
    ));
    agent.wait_for("Alice selected", |s| selected_is(s, &alice));
    let seq = agent.result("\"method\":\"subscribe\"")["seq"]
        .as_u64()
        .unwrap();
    // Sent without waiting for its answer: the update for it comes first, and reading it
    // here would eat the very event we are after.
    agent.send(&format!(
        "{{\"id\":900,\"method\":\"act\",\"action\":{{\"type\":\"select\",\"conversation\":\"{bob}\"}}}}"
    ));
    let update = agent.next_event_matching("of Bob selected", |update| {
        update["snapshot"]["selected"].as_str() == Some(bob.as_str())
    });
    assert!(update["seq"].as_u64().unwrap() > seq);
    let answer = agent.answer(900);
    assert_eq!(answer["result"]["command_sent"], true);
    agent.send(&format!(
        "{{\"id\":901,\"method\":\"act\",\"action\":{{\"type\":\"select\",\"conversation\":\"{alice}\"}}}}"
    ));
    let second = agent.next_event_matching("of Alice selected again", |update| {
        update["snapshot"]["selected"].as_str() == Some(alice.as_str())
    });
    assert!(second["seq"].as_u64().unwrap() > update["seq"].as_u64().unwrap());
}

/// Settings change as asked, keep what isn't named, and refuse what isn't known.
#[test]
fn settings_can_be_changed() {
    let (_app, mut agent) = app("settings");
    wait_for_chats(&mut agent);
    agent.act("{\"type\":\"set_settings\",\"theme\":\"dark\",\"text_size\":\"large\"}");
    let snapshot = agent.snapshot();
    assert_eq!(snapshot["settings"]["theme"], "dark");
    assert_eq!(snapshot["settings"]["text_size"], "large");
    assert_eq!(snapshot["settings"]["show_avatars"], true);
    let error =
        agent.call("\"method\":\"act\",\"action\":{\"type\":\"set_settings\",\"theme\":\"taupe\"}");
    assert!(error.get("error").is_some());
    assert_eq!(agent.snapshot()["settings"]["theme"], "dark");
}
