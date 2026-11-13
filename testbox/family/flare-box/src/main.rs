//! flare-box: the testbox's one-shot driver for Flare's backend (flare-manager), in the manner of
//! presage-cli: each invocation runs the real `flare_manager::Manager` receive loop on a store,
//! does one thing through the manager's handle, and exits.
//!
//!     flare-box <db> register <servers-file> <phone> <captcha> <code> <name>
//!     flare-box <db> whoami
//!     flare-box <db> sync [idle-ms]
//!     flare-box <db> send <aci> <text>
//!     flare-box <db> send-group <group-id-hex> <text>
//!     flare-box <db> groups
//!
//! What it prints mirrors presage-cli's, so that the testbox reads both alike: "From <aci> @
//! <ms>: <text>" for a text received, "To <thread> @ <ms>: <text>" for one this account sent (also
//! from another of its devices), "Delivery receipt from <aci>" for a receipt.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::str::FromStr;
use std::time::Duration;

use fmanager::{LinkSetup, Manager, ManagerHandle, RegisterSetup, SetupDecision, SignalClient};
use fstore::DirectPresageWrapperStore;
use ftypes::{
    AmendmentMessageContent, Error, MainMessageContent, ReceiptKind, ReceivedMessage,
    ReceivedMessageContent, SignalServers, TextMessage, ThreadId,
};
use tokio::sync::Notify;
use url::Url;

type Handle = ManagerHandle<DirectPresageWrapperStore>;

enum Command {
    Register {
        servers: SignalServers,
        phone: String,
        captcha: String,
        code: String,
    },
    Whoami,
    Sync { idle: Duration },
    Send { to: String, text: String },
    SendGroup { group: String, text: String },
    Groups,
    /// What the store holds of a thread: an ACI, or a group's id.
    Messages { thread: String },
}

struct Shared {
    command: Command,
    handle: RefCell<Option<Handle>>,
    queue_empty: Notify,
    /// The send's own echo (the manager stores what it sent and tells the client of it): the
    /// send is done when it comes.
    echoed: Notify,
    /// The times of the texts this account sent, as the manager told the client of them.
    own: RefCell<Vec<i64>>,
    failed: Cell<bool>,
    /// The exit status the command ended with. The process ends only when the runtime has: an
    /// `exit` from inside it runs the C library's exit handlers beside the threads still using
    /// the heap, and one run in sixty aborted there ("double free or corruption") after it had
    /// done its work.
    status: Cell<i32>,
}

#[derive(Clone)]
struct Box_(Rc<Shared>);

fn aci(thread: &ThreadId) -> String {
    match thread {
        ThreadId::Contact(account) => account.0.raw_uuid().to_string(),
        ThreadId::Group(group) => format!("group:{}", hex_of(&format!("{group:?}"))),
    }
}

/// `GroupId(<hex>)` Debug form's hex.
fn hex_of(debug: &str) -> String {
    debug
        .trim_start_matches("GroupId(\"")
        .trim_end_matches("\")")
        .to_owned()
}

impl SignalClient<DirectPresageWrapperStore> for Box_ {
    fn request_setup_decision(&self) -> impl Future<Output = SetupDecision> {
        let decision = match &self.0.command {
            Command::Register {
                servers,
                phone,
                captcha,
                ..
            } => SetupDecision::Register(RegisterSetup {
                servers: servers.clone(),
                phone_number: phonenumber::parse(None, phone).expect("a phone number"),
                captcha: captcha.clone(),
            }),
            _ => {
                eprintln!("flare-box: the store is not registered");
                std::process::exit(2);
            }
        };
        std::future::ready(decision)
    }

    fn request_qrcode_scan(&self, _: Url) -> impl Future<Output = ()> {
        std::future::ready(())
    }

    fn request_confirmation_code(&self) -> impl Future<Output = String> {
        let code = match &self.0.command {
            Command::Register { code, .. } => code.clone(),
            _ => String::new(),
        };
        std::future::ready(code)
    }

    fn setup_complete(&self, handle: Handle) -> impl Future<Output = ()> {
        *self.0.handle.borrow_mut() = Some(handle.clone());
        let this = self.clone();
        // Not awaited here: the manager takes requests only once this returns.
        tokio::task::spawn_local(async move {
            let status = this.run(handle.clone()).await;
            this.0.status.set(status);
            handle.shutdown_receive().await;
        });
        std::future::ready(())
    }

    fn queue_empty(&self) -> impl Future<Output = ()> {
        self.0.queue_empty.notify_one();
        std::future::ready(())
    }

    fn error(&self, error: Error) -> impl Future<Output = ()> {
        eprintln!("ERROR: {error}");
        self.0.failed.set(true);
        self.0.echoed.notify_waiters();
        std::future::ready(())
    }

    async fn on_message(&self, message: ReceivedMessage) {
        let handle = self.0.handle.borrow().clone();
        let me = handle.as_ref().map(|handle| handle.own_account_id());
        let ms = message.id.timestamp.timestamp_millis();
        let from_me = Some(message.id.sender) == me;
        let who = message.id.sender.0.raw_uuid();
        match &message.content {
            ReceivedMessageContent::Main(MainMessageContent::Text(text)) => {
                let body = text.body.as_deref().unwrap_or_default();
                if from_me {
                    println!("To {} @ {ms}: {body}", aci(&message.thread));
                    self.0.own.borrow_mut().push(ms);
                    self.0.echoed.notify_waiters();
                } else {
                    match message.thread {
                        ThreadId::Group(_) => {
                            println!("From {who} in {} @ {ms}: {body}", aci(&message.thread))
                        }
                        ThreadId::Contact(_) => println!("From {who} @ {ms}: {body}"),
                    }
                }
            }
            ReceivedMessageContent::Amendment(AmendmentMessageContent::Receipt(receipt)) => {
                let kind = match receipt.kind {
                    ReceiptKind::Delivery => "Delivery",
                    ReceiptKind::Read => "Read",
                    ReceiptKind::Viewed => "Viewed",
                };
                println!("{kind} receipt from {who}");
            }
            ReceivedMessageContent::Amendment(AmendmentMessageContent::Reaction(reaction)) => {
                println!("Reaction {} from {who}", reaction.emoji);
            }
            _ => {}
        }
    }
}

impl Box_ {
    /// The command, once the manager is set up; the process's exit status.
    async fn run(&self, handle: Handle) -> i32 {
        let ok = match &self.0.command {
            Command::Register { .. } | Command::Whoami => {
                println!("ACI {}", handle.own_account_id().0.raw_uuid());
                true
            }
            Command::Sync { idle } => {
                self.0.queue_empty.notified().await;
                tokio::time::sleep(*idle).await;
                true
            }
            Command::Send { to, text } => {
                self.0.queue_empty.notified().await;
                let to = uuid::Uuid::from_str(to).expect("an ACI");
                self.send(&handle, ThreadId::Contact(ftypes::AccountId::from_raw(to)), text)
                    .await
            }
            Command::SendGroup { group, text } => {
                self.0.queue_empty.notified().await;
                let found = handle
                    .groups()
                    .await
                    .expect("the groups")
                    .into_iter()
                    .find(|candidate| hex_of(&format!("{:?}", candidate.id)) == *group);
                match found {
                    Some(found) => self.send(&handle, ThreadId::Group(found.id), text).await,
                    None => {
                        eprintln!("ERROR: no group {group}");
                        false
                    }
                }
            }
            Command::Messages { thread } => {
                self.0.queue_empty.notified().await;
                let id = match uuid::Uuid::from_str(thread) {
                    Ok(uuid) => ThreadId::Contact(ftypes::AccountId::from_raw(uuid)),
                    Err(_) => {
                        let groups = handle.groups().await.expect("the groups");
                        match groups
                            .into_iter()
                            .find(|group| hex_of(&format!("{:?}", group.id)) == *thread)
                        {
                            Some(group) => ThreadId::Group(group.id),
                            None => {
                                eprintln!("ERROR: no thread {thread}");
                                return 1;
                            }
                        }
                    }
                };
                let stream = handle.messages(id, ..).await.expect("the messages");
                futures::pin_mut!(stream);
                while let Some(stored) = futures::StreamExt::next(&mut stream).await {
                    let stored = stored.expect("a stored message");
                    let text = match &stored.content.inner {
                        MainMessageContent::Text(text) => text.body.clone().unwrap_or_default(),
                        _ => String::new(),
                    };
                    let count = |kind: ReceiptKind| {
                        stored
                            .content
                            .receipts
                            .iter()
                            .filter(|receipt| receipt.content.kind == kind)
                            .count()
                    };
                    println!(
                        "Stored {} by {} delivery={} read={}: {text}",
                        stored.id.timestamp.timestamp_millis(),
                        stored.id.sender.0.raw_uuid(),
                        count(ReceiptKind::Delivery),
                        count(ReceiptKind::Read),
                    );
                }
                true
            }
            Command::Groups => {
                self.0.queue_empty.notified().await;
                for group in handle.groups().await.expect("the groups") {
                    println!(
                        "Group {} {:?} members {}",
                        hex_of(&format!("{:?}", group.id)),
                        group.name,
                        group.members.len()
                    );
                }
                true
            }
        };
        i32::from(!(ok && !self.0.failed.get()))
    }

    async fn send(&self, handle: &Handle, thread: ThreadId, text: &str) -> bool {
        let message = handle
            .send_text_message(
                thread,
                TextMessage {
                    body: Some(text.to_owned()),
                    attachments: vec![],
                    quote: None,
                    body_ranges: vec![],
                    previews: vec![],
                    contacts: vec![],
                    is_view_once: false,
                },
            );
        let ms = message.await.id.timestamp.timestamp_millis();
        // The manager stores what it sent and tells the client, after the server took it; a send
        // that failed says ERROR instead.
        let waiting = async {
            loop {
                let echoed = self.0.echoed.notified();
                tokio::pin!(echoed);
                echoed.as_mut().enable();
                if self.0.own.borrow().contains(&ms) {
                    return true;
                }
                if self.0.failed.get() {
                    return false;
                }
                echoed.await;
            }
        };
        tokio::select! {
            sent = waiting => sent,
            () = tokio::time::sleep(Duration::from_secs(60)) => {
                eprintln!("ERROR: no echo of the send in 60 s");
                false
            }
        }
    }
}

/// Gives the registered account a profile name, so that others can add it to a group outright.
async fn set_profile(db: &str, name: &str) -> i32 {
    use ftypes::Store as _;
    let store = DirectPresageWrapperStore::new(db, None)
        .await
        .expect("the store");
    let mut manager = match presage::Manager::load_registered(store.presage_store()).await {
        Ok(manager) => manager,
        Err(error) => {
            eprintln!("ERROR: {error}");
            return 1;
        }
    };
    let name = presage::libsignal_service::profile_name::ProfileName {
        given_name: name.to_owned(),
        family_name: None,
    };
    match manager.update_profile(name, None, None).await {
        Ok(()) => {
            println!("Profile updated.");
            0
        }
        Err(error) => {
            eprintln!("ERROR: {error}");
            1
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(db), Some(command)) = (args.next(), args.next()) else {
        eprintln!("usage: flare-box <db> register|whoami|sync|send|send-group|groups …");
        std::process::exit(64);
    };
    let rest: Vec<String> = args.collect();
    // Not Flare's: its manager has no profile to set (an account it registers has none, so that
    // nobody can add it to a group but by an invitation); presage's `update_profile`, as
    // presage-cli's `update-profile`, on the same store.
    if let ("set-profile", [name]) = (command.as_str(), rest.as_slice()) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        std::process::exit(runtime.block_on(set_profile(&db, name)));
    }
    let command = match (command.as_str(), rest.as_slice()) {
        ("register", [servers, phone, captcha, code, _name]) => {
            let text = std::fs::read_to_string(servers).expect("the servers file");
            Command::Register {
                servers: SignalServers::Custom(Box::new(toml::from_str(&text).expect("servers"))),
                phone: phone.clone(),
                captcha: captcha.clone(),
                code: code.clone(),
            }
        }
        ("whoami", []) => Command::Whoami,
        ("sync", []) => Command::Sync {
            idle: Duration::from_secs(1),
        },
        ("sync", [idle]) => Command::Sync {
            idle: Duration::from_millis(idle.parse().expect("milliseconds")),
        },
        ("send", [to, text]) => Command::Send {
            to: to.clone(),
            text: text.clone(),
        },
        ("send-group", [group, text]) => Command::SendGroup {
            group: group.clone(),
            text: text.clone(),
        },
        ("groups", []) => Command::Groups,
        ("messages", [thread]) => Command::Messages {
            thread: thread.clone(),
        },
        _ => {
            eprintln!("flare-box: unknown command {command} {rest:?}");
            std::process::exit(64);
        }
    };
    env_logger::init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()
        .expect("a runtime");
    let status = runtime.block_on(async {
        let store = DirectPresageWrapperStore::new(&db, None)
            .await
            .expect("the store");
        let client = Box_(Rc::new(Shared {
            command,
            handle: RefCell::new(None),
            queue_empty: Notify::new(),
            echoed: Notify::new(),
            own: RefCell::new(Vec::new()),
            failed: Cell::new(false),
            status: Cell::new(0),
        }));
        let manager = Manager::new(store, client.clone());
        let local = tokio::task::LocalSet::new();
        match local.run_until(manager.receive()).await {
            Ok(()) => client.0.status.get(),
            Err(error) => {
                eprintln!("ERROR: {error}");
                1
            }
        }
    });
    // The runtime's threads end before the process does.
    runtime.shutdown_timeout(std::time::Duration::from_secs(10));
    std::process::exit(status);
}

#[allow(dead_code)]
fn _unused(_: LinkSetup) {}
