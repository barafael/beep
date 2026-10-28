//! What opens over the chat: stories, details, stickers, media, the chat's search,
//! forwarding; and playing audio.

use super::*;

pub(super) fn wire(window: &AppWindow, shared: &Rc<Shared>) {
    window.global::<Panels>().on_open_stories({
        let shared = shared.clone();
        move || shared.refresh(Update::default())
    });
    window.global::<Panels>().on_open_story({
        let shared = shared.clone();
        move |id| shared.command(|chat| chat.open_story(&ConversationId(id.into())))
    });
    window.global::<Panels>().on_step_story({
        let shared = shared.clone();
        move |by| shared.command(|chat| chat.step_story(by as isize))
    });
    window.global::<Panels>().on_close_story({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.close_story())
    });
    window.global::<Panels>().on_post_story_text({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let text = window.global::<Panels>().get_story_text().to_string();
            let at = window.global::<Panels>().get_story_group().max(0) as usize;
            let group = window
                .global::<Panels>()
                .get_story_group_ids()
                .row_data(at)
                .map(|id| ConversationId(id.into()));
            let sent = shared.act(|chat| {
                let command =
                    group.and_then(|g| chat.send_story(&g, b33p_core::NewStory::Text(text)));
                (command, Update::default())
            });
            if sent {
                window
                    .global::<Panels>()
                    .set_story_text(SharedString::new());
            }
        }
    });
    window.global::<Panels>().on_post_story_photo({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let window = window.clone();
            let title = "Post a photo as a story";
            crate::desktop::pick(&shared.desktop, title, Pick::Picture, move |picked| {
                if let (Some(window), Some(path)) = (window.upgrade(), picked.first()) {
                    let path: SharedString = path.to_string_lossy().as_ref().into();
                    window.global::<Panels>().invoke_story_photo_chosen(path);
                }
            });
        }
    });
    window.global::<Panels>().on_story_photo_chosen({
        let shared = shared.clone();
        let window = window.as_weak();
        move |path| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let at = window.global::<Panels>().get_story_group().max(0) as usize;
            let group = window
                .global::<Panels>()
                .get_story_group_ids()
                .row_data(at)
                .map(|id| ConversationId(id.into()));
            shared.act(|chat| {
                let story = b33p_core::NewStory::Picture(std::path::PathBuf::from(path.as_str()));
                (
                    group.and_then(|g| chat.send_story(&g, story)),
                    Update::default(),
                )
            });
        }
    });
    window.global::<Groups>().on_open_details({
        let shared = shared.clone();
        move || shared.command(|chat| chat.open_details())
    });
    window.global::<Groups>().on_close_details({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.close_details())
    });
    window.global::<Composer>().on_open_stickers({
        let shared = shared.clone();
        move || shared.command(|chat| chat.load_stickers())
    });
    window.global::<Composer>().on_sticker_pack_chosen({
        let shared = shared.clone();
        move || shared.refresh(Update::default())
    });
    window.global::<Composer>().on_send_sticker({
        let shared = shared.clone();
        move |pack, id| shared.command(|chat| chat.send_sticker(&pack, id as u32))
    });
    window.global::<Panels>().on_open_media({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                window.global::<Panels>().set_media_tab(0);
            }
            shared.command(|chat| chat.open_media());
        }
    });
    window.global::<Panels>().on_close_media({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.close_media())
    });
    window.global::<Panels>().on_open_chat_search({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.open_chat_search())
    });
    window.global::<Panels>().on_close_chat_search({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.close_chat_search())
    });
    window.global::<Panels>().on_chat_search({
        let shared = shared.clone();
        move |text| shared.command(|chat| chat.search_chat(&text))
    });
    window.global::<Panels>().on_show_result({
        let shared = shared.clone();
        move |index| shared.redraw(|chat| chat.show_result(index as usize))
    });
    window.global::<Panels>().on_forward({
        let shared = shared.clone();
        move |key| {
            shared.edit(|chat| {
                if let Some(row) = row(chat, &key) {
                    chat.start_forward(row);
                }
            })
        }
    });
    window.global::<Panels>().on_forward_to({
        let shared = shared.clone();
        move |id| shared.command(|chat| chat.forward_to(&ConversationId(id.into())))
    });
    window.global::<Panels>().on_forward_filter_edited({
        let shared = shared.clone();
        move || shared.refresh(Update::default())
    });
    window.global::<Panels>().on_cancel_forward({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.cancel_forward())
    });
    window.global::<Playback>().on_toggle({
        let shared = Rc::downgrade(shared);
        move |id| {
            if let Some(shared) = shared.upgrade() {
                shared.toggle_playback(AttachmentId(id.into()));
            }
        }
    });
    window.global::<Playback>().on_seek({
        let shared = Rc::downgrade(shared);
        move |id, fraction| {
            if let Some(shared) = shared.upgrade() {
                shared.seek_playback(AttachmentId(id.into()), fraction);
            }
        }
    });
}
