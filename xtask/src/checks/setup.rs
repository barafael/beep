//! The fixtures the phases start from: the box confined and serving, presage-cli accounts, b33p,
//! Desktop and Android linked, and the peer's first message to b33p's account accepted. The
//! phases on one fixture share it, one after the other (`super::run`).

use std::path::Path;
use std::time::{Duration, Instant};

use b33p_harness::{Action, ScreenSnapshot};

use super::b33p::{
    REQUEST_ACCEPTED, accept_request, b33p_chat, own_aci, peer_text_reaches, unlock, wait_for_chats,
};
use super::clients::{Android, Desktop, android_shows};
use super::presage::{Account, Presage};
use super::{Checks, HISTORY_PATIENCE, PATIENCE, START};
use crate::Sockets;
use crate::error::{Error, Result};
use crate::harness::Harness;
use crate::pod::{B33P, Pod, Ready, Service};
use crate::process;
use testbox_tools::drivers;

const ANDROID_NUMBER: &str = "+15555550103";
/// Written by Android before b33p links: it reaches b33p in the message history, through CDN3.
pub(super) const NOTE: &str = "testbox android: a note from before b33p linked";
/// The peer's first message to b33p's account, which b33p accepts as a message request.
const FIRST: &str = "testbox: the peer's first message";
/// b33p-slint/src/ui.rs `sync_status` for `HistoryTransfer::Waiting`.
const WAITING_FOR_HISTORY: &str = "Waiting for your phone to send your message history…";

/// b33p linked to a presage-cli primary, Desktop to a presage-cli peer, and the peer's first
/// message to b33p's account accepted in `conversation`.
pub(super) struct LinkedBox<'pod> {
    pub(super) primary: Presage<'pod>,
    pub(super) peer: Presage<'pod>,
    pub(super) primary_aci: String,
    pub(super) b33p: Harness,
    pub(super) desktop: Desktop,
    pub(super) conversation: String,
}

pub(super) fn linked_box<'pod>(
    checks: &mut Checks,
    containers: &'pod Pod,
    sockets: &Sockets,
) -> Result<LinkedBox<'pod>> {
    let PlainBox {
        primary,
        peer,
        primary_aci,
        mut b33p,
    } = plain_box(checks, containers, sockets)?;
    // Before the peer writes: Desktop gets the peer's message as a sync of its own.
    let desktop = linked_desktop(checks, &sockets.desktop, &peer)?;
    // b33p can't start a conversation by phone number (contact discovery, not in the box), so the
    // peer writes first.
    let conversation = checks.require("text from the peer reaches b33p", || {
        peer_text_reaches(&mut b33p, &peer, &primary_aci, FIRST)
    })?;
    checks.require(REQUEST_ACCEPTED, || {
        accept_request(&mut b33p, &conversation)
    })?;
    Ok(LinkedBox {
        primary,
        peer,
        primary_aci,
        b33p,
        desktop,
        conversation,
    })
}

/// b33p linked to a presage-cli primary, and a presage-cli peer: what [`LinkedBox`] starts from,
/// and the stress phases, each on a box of its own (`super::stress`).
pub(super) struct PlainBox<'pod> {
    pub(super) primary: Presage<'pod>,
    pub(super) peer: Presage<'pod>,
    pub(super) primary_aci: String,
    pub(super) b33p: Harness,
}

pub(super) fn plain_box<'pod>(
    checks: &mut Checks,
    containers: &'pod Pod,
    sockets: &Sockets,
) -> Result<PlainBox<'pod>> {
    confined_and_healthy(checks, containers)?;
    let primary = Presage::new(containers, Account::Primary);
    let peer = Presage::new(containers, Account::Peer);
    let primary_aci = checks.require("two presage-cli accounts register", || {
        peer.register()?;
        primary.register()
    })?;
    containers.await_ready(Ready::B33p(0), None, START)?;
    let mut b33p = Harness::connect(&sockets.b33p)?;
    checks.require("b33p links to the primary", || {
        link(&mut b33p, false, |url| primary.add_device(url))
    })?;
    Ok(PlainBox {
        primary,
        peer,
        primary_aci,
        b33p,
    })
}

/// Several b33ps, each linked to a presage-cli primary of its own, each listing every other's
/// account as an accepted chat: each primary wrote to every later one's account first (contact
/// discovery isn't in the box), and the later one's b33p accepted it.
pub(super) struct MeshBox<'pod> {
    pub(super) b33ps: Vec<MeshB33p<'pod>>,
}

/// One b33p of a [`MeshBox`].
pub(super) struct MeshB33p<'pod> {
    /// Which of the box's b33ps it is (`Service::B33p`).
    pub(super) n: u8,
    pub(super) primary: Presage<'pod>,
    /// Its account's.
    pub(super) aci: String,
    pub(super) b33p: Harness,
    /// How long it took to link, from its harness listening to its chats.
    pub(super) linked_in: Duration,
}

impl MeshB33p<'_> {
    /// The id of the other b33ps' chat with its account.
    pub(super) fn chat(&self) -> String {
        format!("c:{}", self.aci)
    }
}

/// The text each primary of a mesh writes to every later one's account first.
pub(super) fn mesh_hello(from: u8, to: u8) -> String {
    format!("testbox mesh: hello from {from} to {to}")
}

pub(super) fn mesh_box<'pod>(
    checks: &mut Checks,
    containers: &'pod Pod,
    sockets: &Sockets,
    b33ps: u8,
) -> Result<MeshBox<'pod>> {
    confined_and_healthy(checks, containers)?;
    let primaries: Vec<Presage<'pod>> = (0..b33ps)
        .map(|n| Presage::new(containers, Account::Mesh(n)))
        .collect();
    let acis = checks.require(&format!("{b33ps} presage-cli primaries register"), || {
        each(&primaries, |primary| primary.register())
    })?;
    let linked = checks.require(&format!("{b33ps} b33ps link, each to its primary"), || {
        each(&primaries, |primary| {
            let n = primaries
                .iter()
                .position(|each| std::ptr::eq(each, primary))
                .and_then(|n| u8::try_from(n).ok())
                .ok_or("a primary of the mesh's")?;
            containers.await_ready(Ready::B33p(n), None, START)?;
            let started = Instant::now();
            let mut b33p = Harness::connect(&sockets.agent(n))?;
            link(&mut b33p, false, |url| primary.add_device(url))?;
            Ok((b33p, started.elapsed()))
        })
    })?;
    let mut mesh = MeshBox {
        b33ps: primaries
            .into_iter()
            .zip(acis)
            .zip(linked)
            .zip(0..)
            .map(|(((primary, aci), (b33p, linked_in)), n)| MeshB33p {
                n,
                primary,
                aci,
                b33p,
                linked_in,
            })
            .collect(),
    };
    checks.require("each primary writes to every later one's account", || {
        for (i, from) in mesh.b33ps.iter().enumerate() {
            for to in &mesh.b33ps[i + 1..] {
                from.primary.send(&to.aci, &mesh_hello(from.n, to.n))?;
            }
        }
        Ok(())
    })?;
    checks.require(
        "each b33p accepts the earlier ones' first messages, and lists the later ones' chats",
        || {
            for i in 0..mesh.b33ps.len() {
                let (earlier, rest) = mesh.b33ps.split_at_mut(i);
                let (this, later) = rest.split_first_mut().ok_or("a b33p of the mesh's")?;
                for other in earlier.iter() {
                    // Once b33p shows the sender by name: until presage has saved them, the
                    // chat is titled by their id, and once saved b33p reloads every chat's
                    // settings, which would take back an acceptance not yet written
                    // (pending/tour.md, finding 8).
                    let name = Account::Mesh(other.n).name();
                    b33p_chat(&mut this.b33p, &other.chat(), "titled by name", |chat| {
                        chat.title == name
                    })?;
                    accept_request(&mut this.b33p, &other.chat())?;
                }
                for other in later.iter() {
                    let chat = other.chat();
                    b33p_chat(&mut this.b33p, &chat, "of its primary's hello", |chat| {
                        // b33p-core's `ConversationView::preview` of one's own.
                        chat.preview == format!("You: {}", mesh_hello(this.n, other.n))
                            && !chat.request
                    })?;
                }
            }
            Ok(())
        },
    )?;
    Ok(mesh)
}

/// `work` on each of `items` at once, in threads; every outcome, in order, or the first failure.
fn each<Item: Sync, Done: Send>(
    items: &[Item],
    work: impl Fn(&Item) -> Result<Done> + Sync,
) -> Result<Vec<Done>> {
    std::thread::scope(|scope| {
        let work = &work;
        let running: Vec<_> = items
            .iter()
            .map(|item| scope.spawn(move || work(item)))
            .collect();
        running
            .into_iter()
            .map(|thread| {
                thread
                    .join()
                    .map_err(|_| Error::Failed("a thread panicked".to_owned()))?
            })
            .collect()
    })
}

/// Connects to Signal-Desktop's driver, links it to `primary`, and waits for its inbox.
pub(super) fn linked_desktop(
    checks: &mut Checks,
    socket: &Path,
    primary: &Presage<'_>,
) -> Result<Desktop> {
    let mut desktop = linking_desktop(checks, socket, primary)?;
    checks.require("Desktop shows its inbox", || desktop_inbox(&mut desktop))?;
    Ok(desktop)
}

fn confined_and_healthy(checks: &mut Checks, containers: &Pod) -> Result<()> {
    checks.require("no egress", || {
        let probe = [
            "curl",
            "-sS",
            "--max-time",
            "5",
            "-o",
            "/dev/null",
            "https://chat.signal.org",
        ];
        // curl's exit codes (curl(1)): 6 the name doesn't resolve, 7 no connection, 28 timed
        // out, as without egress; anything else, podman's own included, is no answer.
        let probed = containers.exec_status(Service::Presage, &probe)?;
        match probed.status.code() {
            Some(6 | 7 | 28) => Ok(()),
            Some(0) => Err("reached chat.signal.org".into()),
            code => Err(Error::Failed(format!(
                "the egress probe ended with {code:?}: {}",
                String::from_utf8_lossy(&probed.stderr)
            ))),
        }
    })?;

    // `podman kube play` returns once the containers exist: each says when it serves.
    checks.require("the server is healthy", || server_healthy(containers))?;
    checks.require("the storage service serves", || {
        containers.await_ready(Ready::Storage, None, START)
    })
}

fn server_healthy(containers: &Pod) -> Result<()> {
    containers.await_ready(Ready::Server, None, PATIENCE)
}

/// Android registered with a [`NOTE`] to itself, b33p linked to it with the message history,
/// Desktop linked to a presage-cli peer, and the peer's first message to Android's account
/// accepted in b33p's `conversation`.
pub(super) struct AndroidBox<'pod> {
    pub(super) peer: Presage<'pod>,
    pub(super) android: Android,
    pub(super) b33p: Harness,
    pub(super) desktop: Desktop,
    pub(super) android_aci: String,
    pub(super) conversation: String,
}

pub(super) fn android_box<'pod>(
    checks: &mut Checks,
    containers: &'pod Pod,
    sockets: &Sockets,
) -> Result<AndroidBox<'pod>> {
    confined_and_healthy(checks, containers)?;
    let peer = Presage::new(containers, Account::Peer);
    checks.require("a presage-cli peer registers", || peer.register().map(drop))?;
    // Desktop syncs after linking (driver.ts `inbox`): while Android and b33p set up.
    let mut desktop = linking_desktop(checks, &sockets.desktop, &peer)?;

    // The driver listens once the emulator has booted and the agent listens.
    containers.await_ready(Ready::Android, None, START)?;
    let mut android = Android::connect(&sockets.android)?;
    checks.require("Android registers", || {
        android.call(drivers::Android::Register {
            number: ANDROID_NUMBER.to_owned(),
            name: "Android".to_owned(),
        })
    })?;
    checks.require("Android writes a note to itself", || {
        android.call(drivers::Android::NoteToSelf {
            text: NOTE.to_owned(),
        })
    })?;

    containers.await_ready(Ready::B33p(0), None, START)?;
    let mut b33p = Harness::connect(&sockets.b33p)?;
    checks.require("b33p links to Android, with the message history", || {
        link(&mut b33p, true, |url| {
            android.call(drivers::Android::LinkDevice {
                url: url.to_owned(),
                transfer: true,
            })
        })
    })?;
    let android_aci = checks.require("b33p knows Android's account", || own_aci(&mut b33p))?;
    checks.require("Desktop shows its inbox", || desktop_inbox(&mut desktop))?;

    let conversation = checks.require("text from the peer reaches b33p", || {
        peer_text_reaches(&mut b33p, &peer, &android_aci, FIRST)
    })?;
    // Before b33p accepts it for the account: a first message from someone Android doesn't know
    // is a message request there, its row reading "Message Request", its text shown once open.
    checks.record("text from the peer reaches Android", || {
        android.call(drivers::Android::OpenConversation {
            text: "Message Request".to_owned(),
        })?;
        android_shows(&mut android, FIRST)
    });
    checks.require(REQUEST_ACCEPTED, || {
        accept_request(&mut b33p, &conversation)
    })?;
    Ok(AndroidBox {
        peer,
        android,
        b33p,
        desktop,
        android_aci,
        conversation,
    })
}

/// Links Signal-Desktop to `primary`: the primary takes Desktop's link URL.
fn link_desktop(desktop: &mut Desktop, primary: &Presage<'_>) -> Result<()> {
    let url: String = desktop.ask(drivers::Desktop::ProvisioningUrl)?;
    primary.add_device(&url)
}

/// Waits for Desktop's inbox once it is linked: up to ten minutes, as Desktop waits for syncs
/// nobody in the box answers (driver.ts `inbox`).
fn desktop_inbox(desktop: &mut Desktop) -> Result<()> {
    desktop.call(drivers::Desktop::Inbox {
        timeout_ms: 10 * 60 * 1000,
    })
}

/// Launches Desktop and links it to `primary`, without waiting for its inbox
/// ([`desktop_inbox`]), so that other setup can go on meanwhile.
fn linking_desktop(checks: &mut Checks, socket: &Path, primary: &Presage<'_>) -> Result<Desktop> {
    primary
        .containers
        .await_ready(Ready::Desktop, None, PATIENCE)?;
    let mut desktop = Desktop::connect(socket)?;
    // Only once the server serves: Desktop gives up linking for good if it doesn't yet.
    server_healthy(primary.containers)?;
    desktop.call(drivers::Desktop::Launch)?;
    checks.require("Desktop links to the peer", || {
        link_desktop(&mut desktop, primary)
    })?;
    Ok(desktop)
}

/// Links b33p as a new device; `primary` takes the link screen's URL.
/// With `history`, b33p waits for the primary's message history and imports it; without, it skips
/// a history it is told to wait for.
fn link(b33p: &mut Harness, history: bool, primary: impl FnOnce(&str) -> Result<()>) -> Result<()> {
    let new_store = b33p.wait_for(
        "b33p asking for a password",
        PATIENCE,
        |snapshot| match snapshot.screen {
            ScreenSnapshot::Password { new, .. } => Some(new),
            _ => None,
        },
    )?;
    if !new_store {
        return Err(
            "b33p already has a store: the box isn't fresh (cargo testbox down --wipe)".into(),
        );
    }
    unlock(b33p)?;
    let url = b33p.wait_for("the link screen", PATIENCE, |snapshot| {
        match &snapshot.screen {
            ScreenSnapshot::Link { url } => Some(Ok(url.clone())),
            ScreenSnapshot::Password {
                error: Some(error), ..
            } => Some(Err(Error::Failed(format!(
                "b33p refused the store password: {error}"
            )))),
            _ => None,
        }
    })??;
    primary(&url)?;
    if history {
        return wait_for_chats(b33p, HISTORY_PATIENCE);
    }
    // presage-cli sends no history archive: if b33p waits for one, skip it.
    let waiting_for_history =
        b33p.wait_for("the chat list", PATIENCE, |snapshot| {
            match &snapshot.screen {
                ScreenSnapshot::Chats => Some(false),
                ScreenSnapshot::Syncing { status } if status == WAITING_FOR_HISTORY => Some(true),
                _ => None,
            }
        })?;
    if waiting_for_history {
        b33p.act(Action::SkipHistory)?;
        wait_for_chats(b33p, PATIENCE)?;
    }
    Ok(())
}

/// Restarts b33p and connects to it again, its store unlocked and its chats shown.
pub(super) fn restart_b33p(containers: &Pod, socket: &Path) -> Result<Harness> {
    let since = process::now()?;
    containers.restart(B33P)?;
    started_b33p(containers, socket, since)
}

/// b33p unlocked and showing its chats again, once it has started after `since`.
pub(super) fn started_b33p(containers: &Pod, socket: &Path, since: u64) -> Result<Harness> {
    started(containers, 0, socket, since)
}

/// The `n`th b33p as [`started_b33p`].
pub(super) fn started(containers: &Pod, n: u8, socket: &Path, since: u64) -> Result<Harness> {
    containers.await_ready(Ready::B33p(n), Some(since), PATIENCE)?;
    let mut b33p = Harness::connect(socket)?;
    b33p.wait_for("b33p asking for its password", PATIENCE, |snapshot| {
        matches!(snapshot.screen, ScreenSnapshot::Password { .. }).then_some(())
    })?;
    unlock(&mut b33p)?;
    wait_for_chats(&mut b33p, PATIENCE)?;
    Ok(b33p)
}
