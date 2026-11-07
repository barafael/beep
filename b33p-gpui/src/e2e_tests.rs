//! Drives the real app over the agent harness: the mock backend in, actions in, snapshots out.
//! The same tests as `b33p-slint/tests/harness.rs`, of the same behaviours, through the same
//! protocol: a client on a thread of its own talks to the agent's Unix socket, while this
//! thread runs the app on GPUI's test platform, which needs no compositor, no GPU and no
//! display (the Slint tests need Qt's offscreen platform for the same reason). Runs only with
//! `--features harness`, like those; every file it writes goes to a scratch home (`testing.rs`).

#![cfg(all(feature = "harness", target_family = "unix"))]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use b33p_harness::{
    Acted, Action, Call, ConversationSnapshot, Delivery, Download, MessageSnapshot, Outcome, Pong,
    Pushed, Reply, Request, Saved, ScreenSnapshot, Snapshot, Subscribed, TextSize, Theme, Trust,
};
use gpui::TestAppContext;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::testing;

/// How long any single wait may take: the mock needs a couple of seconds to link, its echo a
/// few more to come back.
const PATIENCE: Duration = Duration::from_secs(60);

/// Marks a run of this test binary that the parent started to run one test with an environment
/// of its own (see [`in_app`]).
const CHILD: &str = "B33P_E2E_CHILD";

/// Runs `test` against a fresh app, as a client of its agent socket.
///
/// The mock reads its switches (`B33P_MOCK_FAIL`) from the process's environment when it
/// starts, and every test in a process shares that: a test that needs one runs in a process of
/// its own, this test binary started again for that test alone, and passes if that one does.
fn in_app(
    cx: &mut TestAppContext,
    env: &[(&str, &str)],
    test: impl FnOnce(&mut Agent) + Send + 'static,
) {
    if !env.is_empty() && std::env::var_os(CHILD).is_none() {
        return in_a_process_of_its_own(cx, env);
    }
    testing::init();
    let socket = std::env::temp_dir()
        .join(format!("b33p-gpui-tests-{}", std::process::id()))
        .join(format!(
            "agent-{}.sock",
            cx.test_function_name().unwrap_or("test")
        ));
    let live = testing::boot_live(cx, &socket);
    // Work arrives from threads (the backend's, the client's): the app is not done when it
    // has nothing to run for the moment.
    cx.executor().allow_parking();
    let client = std::thread::spawn(move || {
        let stream = UnixStream::connect(&socket)
            .unwrap_or_else(|error| panic!("no agent socket at {}: {error}", socket.display()));
        let writer = stream.try_clone().expect("a second handle to the socket");
        let mut agent = Agent {
            stream: writer,
            lines: BufReader::new(stream),
            next_id: 1,
            skipped: Vec::new(),
            subscribed: false,
        };
        test(&mut agent);
    });
    // The app runs here, as the client's requests and the backend's events come in.
    while !client.is_finished() {
        cx.run_until_parked();
        // The app's own timers (a pause between batches of events, the redraw that follows
        // someone typing) run on the test platform's clock, which stands still until moved.
        cx.executor().advance_clock(Duration::from_millis(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    if let Err(panic) = client.join() {
        std::panic::resume_unwind(panic);
    }
    drop(live);
}

fn in_a_process_of_its_own(cx: &TestAppContext, env: &[(&str, &str)]) {
    let name = cx.test_function_name().expect("a named test");
    let status = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", &format!("e2e_tests::{name}"), "--nocapture"])
        .env(CHILD, "1")
        .envs(env.iter().copied())
        .status()
        .expect("the test binary starts");
    assert!(status.success(), "{name} failed in a process of its own");
}

/// One connection to the harness: requests out, JSON lines in.
struct Agent {
    stream: UnixStream,
    lines: BufReader<UnixStream>,
    next_id: u64,
    /// Replies read while waiting for something else, kept for whoever asks for them.
    skipped: Vec<String>,
    /// Whether the app pushes its state to this connection after every redraw yet.
    subscribed: bool,
}

/// Enough of a line to tell a reply from a pushed update, and whose reply it is.
#[derive(Deserialize)]
struct Header {
    id: Option<u64>,
    event: Option<String>,
}

impl Agent {
    fn send_line(&mut self, line: &str) {
        writeln!(self.stream, "{line}").expect("the socket takes writes");
    }

    /// Sends `call` without waiting for its answer; returns its id.
    fn send(&mut self, call: Call) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&Request { id, call }).expect("requests serialize");
        self.send_line(&line);
        id
    }

    fn reply<T: DeserializeOwned>(&mut self, call: Call) -> Reply<T> {
        let id = self.send(call);
        self.answer(id)
    }

    fn result<T: DeserializeOwned>(&mut self, call: Call) -> T {
        Result::from(self.reply(call).outcome)
            .unwrap_or_else(|error| panic!("the app refused: {error}"))
    }

    /// The answer to request `id`, whether read already or still to come.
    fn answer<T: DeserializeOwned>(&mut self, id: u64) -> Reply<T> {
        let is_answer = |line: &String| header(line).id == Some(id);
        let line = match self.skipped.iter().position(is_answer) {
            Some(at) => self.skipped.remove(at),
            None => {
                let deadline = Instant::now() + PATIENCE;
                loop {
                    let line = self.line(deadline);
                    if is_answer(&line) {
                        break line;
                    }
                }
            }
        };
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("{error}: {line}"))
    }

    fn act(&mut self, action: Action) -> bool {
        self.result::<Acted>(Call::Act { action }).command_sent
    }

    fn select(&mut self, conversation: &str) {
        self.act(Action::Select {
            conversation: conversation.to_owned(),
        });
    }

    fn type_text(&mut self, text: &str) {
        self.act(Action::Type {
            text: text.to_owned(),
        });
    }

    fn snapshot(&mut self) -> Snapshot {
        self.result(Call::Snapshot)
    }

    fn next_update_matching(&mut self, what: &str, until: impl Fn(&Pushed) -> bool) -> Pushed {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let line = self.line(deadline);
            if header(&line).event.is_some() {
                let update: Pushed = serde_json::from_str(&line).expect("an update");
                if until(&update) {
                    return update;
                }
            } else {
                self.skipped.push(line);
            }
            assert!(Instant::now() < deadline, "no update {what}");
        }
    }

    /// The next line: a read that blocks, bounded by the time left until `deadline`.
    fn line(&mut self, deadline: Instant) -> String {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "the app went quiet");
        self.lines
            .get_ref()
            .set_read_timeout(Some(left))
            .expect("the socket takes a read timeout");
        let mut line = String::new();
        match self.lines.read_line(&mut line) {
            Ok(0) => panic!("the app closed the connection"),
            Ok(_) => line,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                panic!("the app went quiet")
            }
            Err(error) => panic!("the socket answers: {error}"),
        }
    }

    /// Waits for the state to come to satisfy `until`, e.g. for the backend to catch up. Nothing
    /// polls: the app pushes its state after every redraw, and every change redraws.
    fn wait_for(&mut self, what: &str, until: impl Fn(&Snapshot) -> bool) -> Snapshot {
        // Subscribed before the first look, so no change between the two goes unseen.
        if !self.subscribed {
            self.result::<Subscribed>(Call::Subscribe);
            self.subscribed = true;
        }
        let snapshot = self.snapshot();
        if until(&snapshot) {
            return snapshot;
        }
        self.next_update_matching(what, |update| until(&update.snapshot))
            .snapshot
    }
}

fn header(line: &str) -> Header {
    serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}"))
}

/// The message with this exact text, if it is on screen.
fn message<'a>(snapshot: &'a Snapshot, text: &str) -> Option<&'a MessageSnapshot> {
    snapshot.messages.iter().find(|m| m.text == text)
}

fn conversation<'a>(snapshot: &'a Snapshot, title: &str) -> &'a ConversationSnapshot {
    snapshot
        .conversations
        .iter()
        .find(|c| c.title == title)
        .unwrap_or_else(|| panic!("a chat called {title} in {:?}", snapshot.conversations))
}

/// The chats screen with the mock's chats listed: a contact is listed once its latest
/// message is known, which comes after the screen.
fn wait_for_chats(agent: &mut Agent) -> Snapshot {
    agent.wait_for("the mock's chats", |s| {
        s.screen == ScreenSnapshot::Chats
            && ["Alice", "Bob"]
                .iter()
                .all(|name| s.conversations.iter().any(|c| c.title == *name))
    })
}

fn id_of(chats: &Snapshot, title: &str) -> String {
    conversation(chats, title).id.clone()
}

fn selected_is(snapshot: &Snapshot, id: &str) -> bool {
    snapshot.selected.as_deref() == Some(id)
}

/// Changes only the theme and the text size.
fn theme_and_text_size(theme: Theme, text_size: TextSize) -> Action {
    Action::SetSettings {
        theme: Some(theme),
        style: None,
        text_size: Some(text_size),
        animations: None,
        show_avatars: None,
        notifications: None,
        notification_content: None,
        show_activity: None,
        show_calendar: None,
        close_to_tray: None,
        read_receipts: None,
        typing_indicators: None,
        link_previews: None,
        sync_to_phone: None,
    }
}

/// Starting, connecting, and reading are the test; also that nonsense is refused.
#[gpui::test]
async fn an_agent_sees_the_chats(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        let ping: Pong = agent.result(Call::Ping);
        assert!(ping.mock);
        // Not a request at all, so by hand.
        agent.send_line(&json!({"id": 7, "method": "teleport"}).to_string());
        assert!(matches!(agent.answer::<Pong>(7).outcome, Outcome::Err(_)));
        let chats = wait_for_chats(agent);
        assert!(chats.conversations.len() > 3);
        let alice = conversation(&chats, "Alice");
        assert!(!alice.group);
        assert!(!alice.request);
        // The mock gives Alice a picture and Bob none.
        assert!(alice.avatar);
        assert!(!conversation(&chats, "Bob").avatar);
        // Note to Self is there and known as such.
        assert!(conversation(&chats, "Note to Self").note_to_self);
    });
}

/// The whole round trip of a message: typed, sent, delivered, read, answered.
#[gpui::test]
async fn a_sent_message_is_answered(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        let chats = wait_for_chats(agent);
        let alice = id_of(&chats, "Alice");
        agent.select(&alice);
        assert!(selected_is(
            &agent.wait_for("Alice selected", |s| selected_is(s, &alice)),
            &alice
        ));
        agent.type_text("Hello");
        assert_eq!(agent.snapshot().draft, "Hello");
        agent.act(Action::Send);
        assert_eq!(agent.snapshot().draft, "");
        let read = agent.wait_for("the message read", |s| {
            message(s, "Hello").and_then(|m| m.delivery) == Some(Delivery::Read)
        });
        assert!(message(&read, "Hello").unwrap().outgoing);
        // The mock answers in person.
        agent.wait_for("the echo", |s| message(s, "You said: Hello").is_some());
    });
}

/// An edit through the harness goes out as the composer's text, as the send button sends it:
/// the state keeps an edit out of the chat's draft, so sending the draft would send nothing.
#[gpui::test]
async fn a_sent_message_is_edited(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        let chats = wait_for_chats(agent);
        agent.select(&id_of(&chats, "Alice"));
        agent.type_text("Hello");
        agent.act(Action::Send);
        // Not while it is sending: only a sent message can be edited.
        let sent = agent.wait_for("the message sent", |s| {
            message(s, "Hello")
                .and_then(|m| m.delivery)
                .is_some_and(|delivery| delivery != Delivery::Sending)
        });
        let index = message(&sent, "Hello").unwrap().index;
        agent.act(Action::Edit { index });
        agent.type_text("Hello, edited");
        agent.act(Action::Send);
        let edited = agent.wait_for("the edit", |s| message(s, "Hello, edited").is_some());
        assert!(message(&edited, "Hello, edited").unwrap().edited);
        assert!(!edited.editing);
    });
}

/// A picture fetched for display and a file saved, with their states in the snapshot.
#[gpui::test]
async fn attachments_are_fetched_and_saved(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        let chats = wait_for_chats(agent);
        agent.select(&id_of(&chats, "Alice"));
        let attachment = |snapshot: &Snapshot, id: &str| {
            snapshot
                .messages
                .iter()
                .flat_map(|m| &m.attachments)
                .find(|a| a.id == id)
                .cloned()
        };
        agent.wait_for("Alice's photo", |s| attachment(s, "photo").is_some());
        agent.act(Action::FetchAttachment {
            id: "photo".to_owned(),
        });
        agent.wait_for("the photo downloaded", |s| {
            attachment(s, "photo").is_some_and(|a| a.download == Some(Download::Ready))
        });
        agent.act(Action::SaveAttachment {
            id: "notes".to_owned(),
        });
        let saved = agent.wait_for("the notes saved", |s| {
            attachment(s, "notes").is_some_and(|a| matches!(a.saved, Some(Saved::Saved(_))))
        });
        let Some(Saved::Saved(path)) = attachment(&saved, "notes").unwrap().saved else {
            unreachable!("waited for it")
        };
        assert!(path.ends_with("route-notes.pdf"));
    });
}

/// What fails for a reason that may pass can be tried again; what Signal refused can't. The
/// mock fails `/fail` as if offline and `/refuse` as if the recipient left Signal, and looks a
/// link ending in "offline" up as if Signal were out of reach.
#[gpui::test]
async fn failures_offer_to_try_again_when_that_may_work(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        let chats = wait_for_chats(agent);
        let alice = id_of(&chats, "Alice");
        agent.select(&alice);
        agent.wait_for("Alice selected", |s| selected_is(s, &alice));
        for text in ["/fail", "/refuse"] {
            agent.type_text(text);
            agent.act(Action::Send);
        }
        let failed = agent.wait_for("both sends failed", |s| {
            message(s, "/fail").and_then(|m| m.delivery) == Some(Delivery::Failed)
                && message(s, "/refuse").and_then(|m| m.delivery) == Some(Delivery::Refused)
        });
        assert!(failed.error.is_some());
        assert!(!failed.can_retry_error, "bubbles offer sends again");
        agent.act(Action::DismissError);

        agent.act(Action::OpenJoinGroup {
            link: Some("https://signal.group/#offline".to_owned()),
        });
        let offered = agent.wait_for("the lookup failed", |s| s.can_retry_error);
        assert!(
            offered
                .error
                .as_deref()
                .is_some_and(|e| e.contains("couldn't look the group up")),
            "{:?}",
            offered.error
        );
        // The mock fails the lookup at once, so a snapshot taken now may already show the next
        // failure: whether the lookup went out again is what the answer says.
        assert!(agent.act(Action::RetryError), "tried again");
        // The mock fails it again, and offers it again.
        agent.wait_for("failed again", |s| s.can_retry_error);
    });
}

/// A group joined through its invite link: looked up, asked to join, then taken back.
#[gpui::test]
async fn a_group_is_joined_through_its_link(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        wait_for_chats(agent);
        agent.act(Action::OpenJoinGroup {
            link: Some("https://signal.group/#mock-approve".to_owned()),
        });
        let looked_up = agent.wait_for("the link looked up", |s| {
            s.join.as_ref().is_some_and(|join| join.title.is_some())
        });
        let join = looked_up.join.unwrap();
        assert_eq!(join.title.as_deref(), Some("Climbing crew"));
        assert!(join.approval);
        agent.act(Action::JoinGroup);
        let asked = agent.wait_for("the group there, asked to join", |s| {
            s.join.is_none()
                && s.conversations
                    .iter()
                    .any(|c| c.title == "Climbing crew" && c.requested)
        });
        let group = id_of(&asked, "Climbing crew");
        assert!(selected_is(&asked, &group), "the group opens");
        agent.act(Action::CancelJoinRequest);
        let taken_back = agent.wait_for("the request taken back", |s| {
            message(s, "You took back the request to join.").is_some()
        });
        let listed = taken_back
            .conversations
            .iter()
            .find(|c| c.id == group)
            .expect("the group is still listed");
        assert!(!listed.requested);
        assert!(listed.left);
    });
}

/// Stories are shown one after another: a picture, then a text one, whose box is sized from the
/// window and once sized the window from itself (a binding loop that crashed the app).
#[gpui::test]
async fn stories_are_shown_in_turn(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        let chats = wait_for_chats(agent);
        let story_by = |s: &Snapshot, author: &str| {
            s.story.as_ref().is_some_and(|story| story.author == author)
        };
        agent.act(Action::OpenStory {
            conversation: id_of(&chats, "Alice"),
        });
        let picture = agent.wait_for("Alice's story", |s| story_by(s, "Alice"));
        let story = picture.story.unwrap();
        assert_eq!((story.position, story.count), (1, 2));
        assert!(story.text.is_none(), "the first is a picture");
        agent.act(Action::OpenStory {
            conversation: id_of(&chats, "Rustaceans 🦀"),
        });
        let text = agent.wait_for("Ferris's story", |s| story_by(s, "Ferris"));
        assert_eq!(
            text.story.unwrap().text.as_deref(),
            Some("cargo build --release 🦀")
        );
        // The window drew the text story (and did not die doing it).
        let drawn = agent.snapshot();
        assert!(story_by(&drawn, "Ferris"));
        assert!(drawn.window.is_some(), "the window answers");
    });
}

/// Whatever an agent does, a subscribed agent hears of it, numbered on.
#[gpui::test]
async fn a_subscriber_hears_updates(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        let chats = wait_for_chats(agent);
        let alice = id_of(&chats, "Alice");
        let bob = id_of(&chats, "Bob");
        agent.select(&alice);
        agent.wait_for("Alice selected", |s| selected_is(s, &alice));
        let subscribed: Subscribed = agent.result(Call::Subscribe);
        // Sent without waiting for its answer: reading for the answer could eat the very update
        // we are after.
        let select = |conversation: &str| Call::Act {
            action: Action::Select {
                conversation: conversation.to_owned(),
            },
        };
        let to_bob = agent.send(select(&bob));
        let update = agent.next_update_matching("of Bob selected", |update| {
            selected_is(&update.snapshot, &bob)
        });
        assert!(update.seq > subscribed.seq);
        let acted = Result::from(agent.answer::<Acted>(to_bob).outcome)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(acted.command_sent);
        agent.send(select(&alice));
        let second = agent.next_update_matching("of Alice selected again", |update| {
            selected_is(&update.snapshot, &alice)
        });
        assert!(second.seq > update.seq);
    });
}

/// Settings change as asked, keep what isn't named, and refuse what isn't known.
#[gpui::test]
async fn settings_can_be_changed(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        wait_for_chats(agent);
        agent.act(theme_and_text_size(Theme::Dark, TextSize::Large));
        let settings = agent.snapshot().settings;
        assert_eq!(settings.theme, Theme::Dark);
        assert_eq!(settings.text_size, TextSize::Large);
        assert!(settings.show_avatars);
        // No such theme, so no such `Theme`: by hand.
        agent.send_line(
            &json!({
                "id": 8,
                "method": "act",
                "action": {"type": "set_settings", "theme": "taupe"},
            })
            .to_string(),
        );
        assert!(matches!(agent.answer::<Acted>(8).outcome, Outcome::Err(_)));
        assert_eq!(agent.snapshot().settings.theme, Theme::Dark);
    });
}

/// A failure at the start has its own screen; trying again carries on.
#[gpui::test]
async fn a_failure_can_be_tried_again(cx: &mut TestAppContext) {
    in_app(cx, &[("B33P_MOCK_FAIL", "key-missing")], |agent| {
        let snapshot = agent.wait_for("the failure screen", |s| {
            matches!(s.screen, ScreenSnapshot::Failed { .. })
        });
        let ScreenSnapshot::Failed { failure, .. } = snapshot.screen else {
            unreachable!("waited for it")
        };
        assert_eq!(failure, "KeyMissing");
        agent.act(Action::Retry);
        wait_for_chats(agent);
    });
}

/// Unlinking forgets the chats and goes back to linking.
#[gpui::test]
async fn unlinking_starts_again_with_linking(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        wait_for_chats(agent);
        agent.act(Action::Unlink);
        let snapshot = agent.wait_for("the link screen", |s| {
            matches!(s.screen, ScreenSnapshot::Link { .. })
        });
        assert_eq!(snapshot.account, "");
        assert!(snapshot.conversations.is_empty());
        wait_for_chats(agent);
    });
}

/// The device name is kept, and empty goes back to the default one.
#[gpui::test]
async fn the_device_can_be_renamed(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        wait_for_chats(agent);
        let default = agent.snapshot().settings.device_name;
        assert!(default.starts_with("b33p"), "{default}");
        agent.act(Action::RenameDevice {
            name: "  Work\nlaptop ".to_owned(),
        });
        assert_eq!(agent.snapshot().settings.device_name, "Work laptop");
        agent.act(Action::RenameDevice {
            name: String::new(),
        });
        assert_eq!(agent.snapshot().settings.device_name, default);
    });
}

/// A contact's safety number is on the details page, and marking it verified shows on the chat;
/// a search in the chat lists what it found, and showing a result closes it.
#[gpui::test]
async fn a_safety_number_is_verified_and_a_chat_searched(cx: &mut TestAppContext) {
    in_app(cx, &[], |agent| {
        let chats = wait_for_chats(agent);
        let alice = id_of(&chats, "Alice");
        agent.select(&alice);
        agent.wait_for("Alice selected", |s| selected_is(s, &alice));
        assert!(agent.act(Action::OpenDetails));
        let shown = agent.wait_for("the safety number", |s| {
            s.details
                .as_ref()
                .is_some_and(|details| details.safety_number.is_some())
        });
        assert_eq!(conversation(&shown, "Alice").trust, Trust::Unverified);
        assert!(agent.act(Action::SetVerified { verified: true }));
        agent.wait_for("Alice verified", |s| {
            conversation(s, "Alice").trust == Trust::Verified
        });
        agent.act(Action::CloseDetails);

        agent.act(Action::OpenChatSearch);
        assert!(agent.act(Action::SearchChat {
            text: "rust".into()
        }));
        let found = agent.wait_for("the results", |s| {
            s.chat_search
                .as_ref()
                .and_then(|search| search.results.as_ref())
                .is_some_and(|results| !results.is_empty())
        });
        let search = found.chat_search.unwrap();
        assert_eq!(search.query, "rust");
        let results = search.results.unwrap();
        assert!(
            results
                .iter()
                .all(|result| result.text.to_lowercase().contains("rust")),
            "{results:?}"
        );
        agent.act(Action::ShowResult { index: 0 });
        agent.wait_for("the search closed", |s| s.chat_search.is_none());
    });
}
