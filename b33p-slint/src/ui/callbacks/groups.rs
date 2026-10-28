//! Groups: making one, joining one, its members and settings.

use super::*;

pub(super) fn wire(window: &AppWindow, shared: &Rc<Shared>) {
    let edit_selected = |shared: &Rc<Shared>, edit: fn(String, bool) -> b33p_core::GroupEdit| {
        let shared = shared.clone();
        move |text: SharedString, flag: bool| {
            shared.act(|chat| {
                let id = chat.selected().map(|c| c.id.clone());
                (
                    id.and_then(|id| chat.edit_group(&id, edit(text.to_string(), flag))),
                    Update::default(),
                )
            });
        }
    };
    window.global::<Groups>().on_answer_invitation({
        let shared = shared.clone();
        move |id, accept| {
            let edit = if accept {
                b33p_core::GroupEdit::AcceptInvitation
            } else {
                b33p_core::GroupEdit::DeclineInvitation
            };
            shared.command(|chat| chat.edit_group(&ConversationId(id.into()), edit));
        }
    });
    window.global::<ChatList>().on_find_number({
        let shared = shared.clone();
        move |number, open| {
            let then = if open {
                b33p_core::AfterFinding::Open
            } else {
                b33p_core::AfterFinding::Choose
            };
            shared.command(|chat| chat.find_number(&number, then));
        }
    });
    window.global::<Groups>().on_confirm_safety_number({
        let shared = shared.clone();
        move |id| shared.command(|chat| chat.confirm_safety_number(&ConversationId(id.into())))
    });
    window.global::<Groups>().on_set_verified({
        let shared = shared.clone();
        move |verified| shared.command(|chat| chat.set_verified(verified))
    });
    window.global::<Groups>().on_rename_group({
        let edit = edit_selected(shared, |text, _| b33p_core::GroupEdit::Title(text));
        move |text| edit(text, false)
    });
    window.global::<Groups>().on_describe_group({
        let edit = edit_selected(shared, |text, _| b33p_core::GroupEdit::Description(text));
        move |text| edit(text, false)
    });
    window.global::<Groups>().on_change_group_picture({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let window = window.clone();
            let title = "Choose the group's picture";
            crate::desktop::pick(&shared.desktop, title, Pick::Picture, move |picked| {
                if let (Some(window), Some(path)) = (window.upgrade(), picked.first()) {
                    let path: SharedString = path.to_string_lossy().as_ref().into();
                    window.global::<Groups>().invoke_group_picture_chosen(path);
                }
            });
        }
    });
    window.global::<Groups>().on_group_picture_chosen({
        let edit = edit_selected(shared, |path, _| {
            b33p_core::GroupEdit::SetPicture(path.into())
        });
        move |path| edit(path, false)
    });
    window.global::<Groups>().on_remove_group_picture({
        let edit = edit_selected(shared, |_, _| b33p_core::GroupEdit::RemovePicture);
        move || edit(SharedString::new(), false)
    });
    window
        .global::<Groups>()
        .on_set_member_admin(edit_selected(shared, b33p_core::GroupEdit::SetAdmin));
    window.global::<Groups>().on_remove_member({
        let edit = edit_selected(shared, |id, _| b33p_core::GroupEdit::RemoveMember(id));
        move |id| edit(id, false)
    });
    window.global::<Groups>().on_leave_group({
        let edit = edit_selected(shared, |_, _| b33p_core::GroupEdit::Leave);
        move || edit(SharedString::new(), false)
    });
    window.global::<Groups>().on_open_new_group({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.open_group_draft(None))
    });
    window.global::<Groups>().on_open_join_group({
        let shared = shared.clone();
        move || shared.command(|chat| chat.open_join(None))
    });
    window.global::<Groups>().on_close_join_group({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.close_join())
    });
    window.global::<Groups>().on_preview_group_link({
        let shared = shared.clone();
        move |link| shared.command(|chat| chat.preview_link(&link))
    });
    window.global::<Groups>().on_join_group({
        let shared = shared.clone();
        move || shared.command(|chat| chat.join_group())
    });
    window.global::<Groups>().on_set_invite_link({
        let shared = shared.clone();
        move |access| {
            let access = match access {
                1 => b33p_core::InviteLinkAccess::Anyone,
                2 => b33p_core::InviteLinkAccess::Approval,
                _ => b33p_core::InviteLinkAccess::Off,
            };
            shared.act(|chat| {
                let id = chat.selected().map(|c| c.id.clone());
                (
                    id.and_then(|id| {
                        chat.edit_group(&id, b33p_core::GroupEdit::InviteLink(access))
                    }),
                    Update::default(),
                )
            });
        }
    });
    window.global::<Groups>().on_reset_invite_link({
        let edit = edit_selected(shared, |_, _| b33p_core::GroupEdit::ResetInviteLink);
        move || edit(SharedString::new(), false)
    });
    window
        .global::<Groups>()
        .on_answer_join_request(edit_selected(shared, |member, approve| {
            if approve {
                b33p_core::GroupEdit::ApproveRequest(member)
            } else {
                b33p_core::GroupEdit::DenyRequest(member)
            }
        }));
    window.global::<Groups>().on_cancel_join_request({
        let edit = edit_selected(shared, |_, _| b33p_core::GroupEdit::CancelJoinRequest);
        move || edit(SharedString::new(), false)
    });
    window.global::<Groups>().on_open_add_members({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.open_add_members();
                (None, Update::default())
            });
        }
    });
    window.global::<Groups>().on_toggle_candidate({
        let shared = shared.clone();
        move |id| shared.edit(|chat| chat.toggle_group_member(&ConversationId(id.into())))
    });
    window.global::<Groups>().on_group_draft_edited({
        let shared = shared.clone();
        move || shared.refresh(Update::default())
    });
    window
        .global::<Composer>()
        .on_is_blank(|text| text.trim().is_empty());
    window.global::<Groups>().on_finish_group_draft({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let name = window.global::<Groups>().get_group_draft_name();
            shared.command(|chat| chat.finish_group_draft(&name));
        }
    });
    window.global::<Groups>().on_close_group_draft({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.close_group_draft())
    });
}
