use b33p_harness::Action;

use super::b33p::{b33p_chat, b33p_shows, edit, outgoing_index, read, select, type_and_send};
use super::clients::{Android, desktop_shows, desktop_status};
use super::setup::AndroidBox;
use super::{Checks, PATIENCE, PATIENCE_MS};
use crate::error::Result;
use crate::harness::Harness;
use testbox_tools::drivers;

/// Android's suggested folder of every 1:1 chat (strings.xml `ChatFoldersFragment__one_on_one_chats`).
const FOLDER: &str = "1:1 chats";

/// Chat settings through the storage service, between b33p, Android (the primary: it writes
/// there, presage-cli doesn't) and Desktop, linked to the peer.
pub(super) fn storage(checks: &mut Checks, fixture: &mut AndroidBox<'_>) -> Result<()> {
    use drivers::ChatMenuItem::{Archive, Mute, Pin, Read, Unmute, Unpin, Unread};

    let AndroidBox {
        android,
        b33p,
        desktop,
        android_aci,
        conversation,
        ..
    } = fixture;

    // Desktop sends read receipts only with the setting on, which presage-cli, its primary,
    // links it without (libsignal-service-rs `ProvisionMessage` `read_receipts: None`). Desktop
    // then writes it to the account record.
    checks.require(
        "Desktop writes its read-receipt setting to the storage service",
        || {
            desktop.call(drivers::Desktop::SetReadReceipts {
                on: true,
                timeout_ms: PATIENCE_MS,
            })
        },
    )?;
    desktop.call(drivers::Desktop::OpenConversation {
        aci: android_aci.clone(),
    })?;
    // Android's chat list shows the chat by its latest message, which this stays.
    let row = "testbox storage: read on Desktop";
    checks.require("text from b33p reaches Desktop", || {
        select(b33p, conversation)?;
        type_and_send(b33p, row)?;
        desktop_shows(desktop, row)
    })?;
    // TESTPLAN.md §1.1 item 2.
    checks.record("Desktop's read receipt shows on b33p as read", || {
        read(b33p, row)
    });
    // From here on the chat isn't open in b33p: an open chat is read, which would undo a mark.
    b33p.act(Action::Back)?;

    // TESTPLAN.md §1.8, §1.10.
    checks.record("a chat pinned on Android shows pinned in b33p", || {
        chat_menu(android, row, Pin)?;
        b33p_chat(b33p, conversation, "pinned", |chat| chat.pinned)
    });
    checks.record("a chat muted on Android shows muted in b33p", || {
        chat_menu(android, row, Mute)?;
        b33p_chat(b33p, conversation, "muted", |chat| chat.muted)
    });
    // Archiving unpins, on Android as in b33p. b33p lists archived chats with its archive open
    // (b33p-core/src/state.rs `listed`).
    checks.record("a chat archived on Android shows archived in b33p", || {
        chat_menu(android, row, Archive)?;
        b33p.act(Action::ShowArchived { show: true })?;
        let archived = b33p_chat(b33p, conversation, "archived, not pinned", |chat| {
            chat.archived && !chat.pinned
        });
        b33p.act(Action::ShowArchived { show: false })?;
        archived
    });

    // TESTPLAN.md §2.12. Not Android's "Unread" folder: its chip would carry the label of the
    // menu's "Unread".
    checks.record("a chat folder added on Android shows in b33p", || {
        android.call(drivers::Android::AddSuggestedFolder {
            name: FOLDER.to_owned(),
        })?;
        b33p.wait_for(
            &format!("the folder {FOLDER:?} in b33p"),
            PATIENCE,
            |snapshot| {
                snapshot
                    .folders
                    .iter()
                    .any(|folder| folder.name == FOLDER && folder.all_individual)
                    .then_some(())
            },
        )
    });

    // TESTPLAN.md §2.13: b33p writes only when told to sync to the phone.
    checks.record("b33p's unarchive and pin show on Android", || {
        android_takes(android, b33p, |b33p| {
            b33p.act(Action::Archive {
                conversation: conversation.clone(),
                archived: false,
            })?;
            b33p.act(Action::TogglePin {
                conversation: conversation.clone(),
            })
            .map(drop)
        })?;
        // Unarchived, pinned, and still muted from Android.
        menu_offers(android, row, &[Unpin, Unmute])
    });

    // Not a 409: b33p reads right before each write (b33p-core/src/signal.rs `follow_storage`),
    // and nothing outside can time a write between the two. What can be arranged is a change on
    // each side between two of b33p's writes, which b33p merges field by field
    // (b33p-core/src/storage.rs `merge`).
    checks.record(
        "changes on both sides between b33p's writes end with both agreeing",
        || {
            // Read on Android first: its menu offers "Unread" only for a read chat.
            android.call(drivers::Android::OpenConversation {
                text: row.to_owned(),
            })?;
            sync_to_phone(b33p, false)?;
            b33p.act(Action::Mute {
                conversation: conversation.clone(),
                minutes: 0,
            })?;
            chat_menu(android, row, Unread)?;
            b33p_chat(
                b33p,
                conversation,
                "marked unread from Android, unmuted here",
                |chat| chat.marked_unread && !chat.muted,
            )?;
            android_takes(android, b33p, |_b33p| Ok(()))?;
            menu_offers(android, row, &[Read, Mute, Unpin])?;
            b33p_chat(b33p, conversation, "as on Android", |chat| {
                chat.marked_unread && !chat.muted && chat.pinned
            })
        },
    );

    // Signal's apps name an edited message by its latest edit's time, and show only what the
    // receipts for the edit say (Signal-Desktop MessageReceipts.preload.ts
    // `updateMessageSendStateWithReceipt`, markConversationRead.preload.ts).
    checks.record(
        "Desktop's read receipt for b33p's edit shows on b33p",
        || {
            select(b33p, conversation)?;
            let edited = "testbox storage: read on Desktop, edited";
            let index = outgoing_index(b33p, row)?;
            edit(b33p, index, edited)?;
            desktop_shows(desktop, edited)?;
            // Desktop reads an edit with the conversation's next read (markConversationRead's
            // `unreadEditedMessages`), and reads only while someone is at it: for 15 s after a
            // click or key (ActiveWindowService.std.ts `ACTIVE_TIMEOUT`), and again at the next
            // one (Timeline.dom.tsx `registerForActive`). A click on the chat is that.
            desktop.call(drivers::Desktop::OpenConversation {
                aci: android_aci.clone(),
            })?;
            read(b33p, edited)
        },
    );
    checks.record(
        "b33p's receipts for Desktop's edit show on Desktop: delivered, then read",
        || {
            let text = "testbox storage: from Desktop";
            let edited = "testbox storage: from Desktop, edited";
            select(b33p, conversation)?;
            b33p.act(Action::SetWindowFocused { focused: true })?;
            desktop.call(drivers::Desktop::Send {
                text: text.to_owned(),
            })?;
            desktop_status(desktop, text, "read")?;
            // Away from the window: the edit is delivered, not read.
            b33p.act(Action::SetWindowFocused { focused: false })?;
            desktop.call(drivers::Desktop::EditMessage {
                old_text: text.to_owned(),
                text: edited.to_owned(),
            })?;
            b33p_shows(b33p, &format!("{edited:?}"), |message| {
                message.text == edited
            })?;
            desktop_status(desktop, edited, "delivered")?;
            b33p.act(Action::SetWindowFocused { focused: true })?;
            desktop_status(desktop, edited, "read")
        },
    );

    Ok(())
}

/// Picks `item` from the menu of Android's chat list row showing `row`.
fn chat_menu(android: &mut Android, row: &str, item: drivers::ChatMenuItem) -> Result<()> {
    android.call(drivers::Android::ChatMenu {
        text: row.to_owned(),
        item,
    })
}

/// Fails unless the menu of Android's row showing `row` offers each of `items`.
fn menu_offers(android: &mut Android, row: &str, items: &[drivers::ChatMenuItem]) -> Result<()> {
    android.call(drivers::Android::ChatMenuOffers {
        text: row.to_owned(),
        items: items.to_vec(),
    })
}

/// Turns b33p's sync to the phone on, has b33p `act`, and waits until Android has taken in a
/// storage manifest newer than before: b33p's write, which it fetches on b33p's FetchLatest. The
/// mark comes first, so that a sync quicker than the wait still counts.
fn android_takes(
    android: &mut Android,
    b33p: &mut Harness,
    act: impl FnOnce(&mut Harness) -> Result<()>,
) -> Result<()> {
    let mark: u64 = android.ask(drivers::Android::LogMark)?;
    sync_to_phone(b33p, true)?;
    act(b33p)?;
    android.call(drivers::Android::AwaitStorageSync {
        after: mark,
        timeout_ms: PATIENCE_MS,
    })
}

/// Turns b33p's "sync to phone" on or off, as its settings switch does: the next write to the
/// storage service, or none.
fn sync_to_phone(b33p: &mut Harness, on: bool) -> Result<()> {
    b33p.act(Action::SetSettings {
        theme: None,
        style: None,
        text_size: None,
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
        sync_to_phone: Some(on),
    })?;
    b33p.wait_for("b33p's sync to phone", PATIENCE, |snapshot| {
        (snapshot.settings.sync_to_phone == on).then_some(())
    })
}
