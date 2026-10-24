//! What the phases start from: the box confined and serving, presage-cli accounts, and b33p,
//! Desktop and Android linked.

use std::path::Path;

use b33p_harness::{Action, ScreenSnapshot};

use super::b33p::{own_aci, unlock, wait_for_chats};
use super::clients::{Android, Desktop};
use super::presage::{Account, Presage};
use super::{Checks, HISTORY_PATIENCE, PATIENCE, START};
use crate::Sockets;
use crate::error::{Error, Result};
use crate::harness::Harness;
use crate::pod::{Pod, Ready, Service};
use crate::process;
use testbox_tools::drivers;

const ANDROID_NUMBER: &str = "+15555550103";
/// b33p-slint/src/ui.rs `sync_status` for `HistoryTransfer::Waiting`.
const WAITING_FOR_HISTORY: &str = "Waiting for your phone to send your message history…";


/// What the checks without Android start from: no egress, the server healthy, two presage-cli
/// accounts, and b33p linked to the primary.
pub(super) struct LinkedBox<'pod> {
    pub(super) primary: Presage<'pod>,
    pub(super) peer: Presage<'pod>,
    pub(super) primary_aci: String,
    pub(super) b33p: Harness,
}

pub(super) fn linked_box<'pod>(
    checks: &mut Checks,
    containers: &'pod Pod,
    sockets: &Sockets,
) -> Result<LinkedBox<'pod>> {
    confined_and_healthy(checks, containers)?;
    let primary = Presage::new(containers, Account::Primary);
    let peer = Presage::new(containers, Account::Peer);
    let primary_aci = checks.require("two presage-cli accounts register", || {
        peer.register()?;
        primary.register()
    })?;

    containers.await_ready(Ready::B33p, None, START)?;
    let mut b33p = Harness::connect(&sockets.b33p)?;
    checks.require("b33p links to the primary", || {
        link(&mut b33p, false, |url| primary.add_device(url))
    })?;
    Ok(LinkedBox {
        primary,
        peer,
        primary_aci,
        b33p,
    })
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

/// What the checks with Android start from: no egress, a presage-cli peer, Android registered, b33p
/// linked to it, and Desktop linked to the peer.
pub(super) struct AndroidBox<'pod> {
    pub(super) peer: Presage<'pod>,
    pub(super) android: Android,
    pub(super) b33p: Harness,
    pub(super) desktop: Desktop,
    pub(super) android_aci: String,
}

/// With a `note`, Android writes it to itself first, and b33p links with the message history.
pub(super) fn android_box<'pod>(
    checks: &mut Checks,
    containers: &'pod Pod,
    sockets: &Sockets,
    note: Option<&str>,
) -> Result<AndroidBox<'pod>> {
    confined_and_healthy(checks, containers)?;
    let peer = Presage::new(containers, Account::Peer);
    checks.require("a presage-cli peer registers", || peer.register().map(drop))?;
    // Desktop syncs after linking for minutes (driver.ts `inbox`): while Android and b33p set up.
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
    if let Some(note) = note {
        checks.require("Android writes a note to itself", || {
            android.call(drivers::Android::NoteToSelf {
                text: note.to_owned(),
            })
        })?;
    }

    containers.await_ready(Ready::B33p, None, START)?;
    let mut b33p = Harness::connect(&sockets.b33p)?;
    let history = note.is_some();
    let linked = if history {
        "b33p links to Android, with the message history"
    } else {
        "b33p links to Android"
    };
    checks.require(linked, || {
        link(&mut b33p, history, |url| {
            android.call(drivers::Android::LinkDevice {
                url: url.to_owned(),
                transfer: history,
            })
        })
    })?;
    let android_aci = checks.require("b33p knows Android's account", || own_aci(&mut b33p))?;
    checks.require("Desktop shows its inbox", || desktop_inbox(&mut desktop))?;
    Ok(AndroidBox {
        peer,
        android,
        b33p,
        desktop,
        android_aci,
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

/// Connects to Signal-Desktop's driver and links it to `primary`.
pub(super) fn linked_desktop(
    checks: &mut Checks,
    socket: &Path,
    primary: &Presage<'_>,
) -> Result<Desktop> {
    let mut desktop = linking_desktop(checks, socket, primary)?;
    checks.require("Desktop shows its inbox", || desktop_inbox(&mut desktop))?;
    Ok(desktop)
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
    containers.restart(Service::B33p)?;
    containers.await_ready(Ready::B33p, Some(since), PATIENCE)?;
    let mut b33p = Harness::connect(socket)?;
    b33p.wait_for("b33p asking for its password", PATIENCE, |snapshot| {
        matches!(snapshot.screen, ScreenSnapshot::Password { .. }).then_some(())
    })?;
    unlock(&mut b33p)?;
    wait_for_chats(&mut b33p, PATIENCE)?;
    Ok(b33p)
}
