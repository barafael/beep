//! Before what can't be undone: deleting the data here, leaving a group, blocking or deleting a
//! message request, deleting for everyone. Cancel is the default (Enter takes it, as Esc does);
//! the action is a plain button saying exactly what it does.

use std::rc::Rc;

use b33p_core::state::Update;
use b33p_core::{GroupEdit, RequestResponse};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, FocusHandle, Focusable, IntoElement, Render, Window, div};

use crate::dialog;
use crate::shared::{Confirm, Shared};
use crate::theme;
use crate::widgets::modal;

/// What a confirmation says: its title, its explanation and the label of its action.
#[derive(Debug, PartialEq, Eq)]
pub struct Texts {
    pub title: String,
    pub text: String,
    pub action: &'static str,
}

/// The texts of `confirm`, for the open chat's `title` and the number of messages picked.
pub fn texts(confirm: Confirm, title: &str, picked: usize) -> Texts {
    if confirm == Confirm::OpenLink {
        // `title` is what the address really is and where it goes (see `web::spelled_out`).
        return Texts {
            title: "Open this link?".to_owned(),
            text: format!(
                "{title}\n\nThe address can look like another site's. Open it only if you trust who sent it."
            ),
            action: "Open link",
        };
    }
    let (heading, text, action) = match confirm {
        Confirm::ResetInviteLink => (
            "Reset the invite link?".to_owned(),
            "The link people have stops working, and the group gets a new one. Those who only have the old link can't join with it.",
            "Reset link",
        ),
        Confirm::RemoveMember => (
            format!("Remove {title} from the group?"),
            "They can't see new messages in it, unless someone adds them again.",
            "Remove",
        ),
        Confirm::BlockChat => (
            format!("Block {title}?"),
            "They won't be able to message or call you.",
            "Block",
        ),
        Confirm::Unlink => (
            "Unlink this device?".to_owned(),
            "This computer is removed from your Signal account, and the messages stored here are deleted. Your phone keeps its messages. To use b33p again, link it anew.",
            "Unlink and delete",
        ),
        Confirm::StartOver => (
            "Start over?".to_owned(),
            "The messages stored on this computer are deleted, and b33p is linked to your phone anew. Your phone keeps its messages; you can transfer them again while linking.",
            "Delete and start over",
        ),
        Confirm::LeaveGroup => (
            format!("Leave {title}?"),
            "You won't get its messages or be able to write to it, unless someone adds you again.",
            "Leave group",
        ),
        Confirm::BlockRequest => (
            format!("Block {title}?"),
            "They won't be able to message or call you.",
            "Block",
        ),
        Confirm::DeleteRequest => (
            "Delete this chat?".to_owned(),
            "The chat and its messages are deleted here.",
            "Delete",
        ),
        Confirm::DeleteSelectionForEveryone if picked > 1 => (
            format!("Delete {picked} messages for everyone?"),
            "Everyone in this chat sees that it was deleted.",
            "Delete for everyone",
        ),
        Confirm::DeleteForEveryone
        | Confirm::DeleteSelectionForEveryone
        | Confirm::OpenLink
        | Confirm::None => (
            "Delete for everyone?".to_owned(),
            "Everyone in this chat sees that it was deleted.",
            "Delete for everyone",
        ),
    };
    Texts {
        title: heading,
        text: text.to_owned(),
        action,
    }
}

/// Does what the confirmation was for, and closes it.
pub fn confirmed(shared: &Rc<Shared>, cx: &mut App) {
    let (confirm, message, asked_in) = {
        let ui = shared.ui.borrow();
        (
            ui.confirm,
            ui.confirm_message.clone(),
            ui.confirm_chat.clone(),
        )
    };
    let open = shared.chat.borrow().selected().map(|c| c.id.clone());
    // What the dialog named, not what happens to be open now: a chat can be switched while it
    // is up (a notification, a shortcut). Without a name (asked from outside), the open one.
    let selected = || asked_in.clone().or_else(|| open.clone());
    // About the open chat's messages: only if it is still the one the dialog was about.
    let still_there = asked_in.is_none() || asked_in == open;
    match confirm {
        Confirm::None => {}
        Confirm::Unlink => {
            shared.ui.borrow_mut().settings_open = false;
            // Both forget everything shown.
            shared.act(cx, |chat| {
                (
                    chat.unlink(),
                    Update {
                        screen: true,
                        ..Update::reset()
                    },
                )
            });
        }
        Confirm::StartOver => {
            shared.act(cx, |chat| {
                (
                    chat.start_over(),
                    Update {
                        screen: true,
                        ..Update::reset()
                    },
                )
            });
        }
        Confirm::LeaveGroup => {
            let id = selected();
            shared.command(cx, |chat| {
                id.and_then(|id| chat.edit_group(&id, GroupEdit::Leave))
            });
        }
        Confirm::BlockRequest | Confirm::DeleteRequest => {
            let response = if confirm == Confirm::BlockRequest {
                RequestResponse::Block
            } else {
                RequestResponse::Delete
            };
            let id = selected();
            shared.act(cx, |chat| {
                (
                    id.and_then(|id| chat.respond(&id, response)),
                    Update::reset(),
                )
            });
        }
        Confirm::DeleteForEveryone => {
            // Where the message is now, if it still is.
            if still_there {
                shared.act(cx, |chat| {
                    match message.as_ref().and_then(|target| chat.row_of(target)) {
                        Some(index) => chat.delete(index, true),
                        None => (None, Update::default()),
                    }
                });
            }
        }
        Confirm::OpenLink => {
            let link = shared.ui.borrow().confirm_link.clone();
            if let Some(link) = link {
                crate::web::open_confirmed(&link);
            }
        }
        Confirm::BlockChat => {
            let id = selected();
            shared.chat_action_if(cx, id, |chat, id| chat.block(id, true));
        }
        Confirm::RemoveMember => {
            let member = shared.ui.borrow().confirm_member.clone();
            let id = selected();
            if let (Some((service_id, _)), true) = (member, still_there) {
                shared.command(cx, |chat| {
                    id.and_then(|id| chat.edit_group(&id, GroupEdit::RemoveMember(service_id)))
                });
            }
        }
        Confirm::ResetInviteLink => {
            let id = selected();
            if still_there {
                shared.command(cx, |chat| {
                    id.and_then(|id| chat.edit_group(&id, GroupEdit::ResetInviteLink))
                });
            }
        }
        Confirm::DeleteSelectionForEveryone => {
            if still_there {
                shared.act(cx, |chat| chat.delete_selection(true));
            }
        }
    }
    dismissed(shared, cx);
}

/// Closes the confirmation without doing anything.
pub fn dismissed(shared: &Rc<Shared>, cx: &mut App) {
    {
        let mut ui = shared.ui.borrow_mut();
        ui.confirm = Confirm::None;
        ui.confirm_message = None;
        ui.confirm_chat = None;
        ui.confirm_link = None;
        ui.confirm_member = None;
    }
    shared.refresh(Update::default(), cx);
}

pub struct ConfirmView {
    shared: Rc<Shared>,
    focus: FocusHandle,
}

impl ConfirmView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self { shared, focus }
    }
}

impl Focusable for ConfirmView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConfirmView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let (confirm, title, picked) = {
            let chat = self.shared.chat.borrow();
            let ui = self.shared.ui.borrow();
            let about = match &ui.confirm_chat {
                Some(id) => chat.conversation(id),
                None => chat.selected(),
            };
            let title = match (&ui.confirm, &ui.confirm_link, &ui.confirm_member) {
                (Confirm::OpenLink, Some(link), _) => format!(
                    "{}\n\nGoes to: {}",
                    crate::web::spelled_out(link),
                    crate::web::destination(link)
                ),
                (Confirm::RemoveMember, _, Some((_, name))) => name.clone(),
                _ => about.map(|c| c.title.clone()).unwrap_or_default(),
            };
            (ui.confirm, title, chat.selection_len())
        };
        let texts = texts(confirm, &title, picked);
        let shared = self.shared.clone();
        let content = div()
            .track_focus(&self.focus)
            .key_context("Confirm")
            // Enter takes the default, which is not to do it.
            .on_key_down({
                let shared = shared.clone();
                move |event, _, cx| {
                    if event.keystroke.key == "enter" {
                        dismissed(&shared, cx);
                        cx.stop_propagation();
                    }
                }
            })
            .child(
                dialog::layout(texts.title)
                    .child(dialog::hint(&t, texts.text))
                    .child(
                        dialog::buttons()
                            .child(
                                Button::new("confirm-cancel")
                                    .primary()
                                    .label("Cancel")
                                    .debug_selector(|| "Cancel".into())
                                    .on_click({
                                        let shared = shared.clone();
                                        move |_, _, cx| dismissed(&shared, cx)
                                    }),
                            )
                            .child(
                                Button::new("confirm-action")
                                    .label(texts.action)
                                    .debug_selector(|| "Confirm".into())
                                    .on_click({
                                        let shared = shared.clone();
                                        move |_, _, cx| confirmed(&shared, cx)
                                    }),
                            ),
                    ),
            );
        modal(&t, 420., content, move |_, cx| dismissed(&shared, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaving_and_blocking_name_the_chat() {
        assert_eq!(texts(Confirm::LeaveGroup, "Rust", 0).title, "Leave Rust?");
        assert_eq!(texts(Confirm::BlockRequest, "Eve", 0).title, "Block Eve?");
        assert_eq!(texts(Confirm::LeaveGroup, "Rust", 0).action, "Leave group");
    }

    #[test]
    fn deleting_a_selection_counts_it_from_two() {
        let one = texts(Confirm::DeleteSelectionForEveryone, "", 1);
        assert_eq!(one.title, "Delete for everyone?");
        let many = texts(Confirm::DeleteSelectionForEveryone, "", 3);
        assert_eq!(many.title, "Delete 3 messages for everyone?");
        assert_eq!(many.action, "Delete for everyone");
    }

    #[test]
    fn every_confirmation_has_an_action_of_its_own() {
        let actions: Vec<_> = [
            Confirm::Unlink,
            Confirm::StartOver,
            Confirm::LeaveGroup,
            Confirm::BlockRequest,
            Confirm::DeleteRequest,
            Confirm::DeleteForEveryone,
        ]
        .into_iter()
        .map(|confirm| texts(confirm, "x", 0).action)
        .collect();
        let mut unique = actions.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), actions.len());
    }
}
