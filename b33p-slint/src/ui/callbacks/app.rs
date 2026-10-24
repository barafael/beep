//! The window itself, the screens before the chats (password, failure), settings and
//! the session (unlinking, starting over).

use super::*;

pub(super) fn wire(window: &AppWindow, shared: &Rc<Shared>) {
    window.window().on_close_requested({
        let shared = shared.clone();
        move || shared.close_requested()
    });
    window.global::<Prefs>().on_settings_opened({
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                window
                    .global::<Prefs>()
                    .set_tray_available(crate::tray::host_present());
            }
        }
    });
    window.global::<Prefs>().on_start_with_system_changed({
        let window = window.as_weak();
        move |enabled| {
            if let Err(error) = crate::autostart::set(enabled) {
                tracing::warn!(%error, "failed to change starting with the system");
            }
            if let Some(window) = window.upgrade() {
                window
                    .global::<Prefs>()
                    .set_start_with_system(crate::autostart::enabled());
            }
        }
    });
    window.global::<Account>().on_unlock({
        let shared = shared.clone();
        move |password, confirmation| {
            shared.act(|chat| {
                (
                    chat.unlock(&password, &confirmation),
                    Update {
                        screen: true,
                        ..Update::default()
                    },
                )
            });
        }
    });
    window.global::<Prefs>().on_settings_changed({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let settings = Settings {
                // Not among the page's choices: named by `rename_device`, and picked emoji.
                device_name: shared.settings.borrow().device_name.clone(),
                recent_emoji: shared.settings.borrow().recent_emoji.clone(),
                ..read_settings(&window)
            };
            window
                .global::<Prefs>()
                .set_font_scale(settings.text_size.scale());
            shared.apply_settings(settings);
            window
                .global::<Prefs>()
                .set_style_note(shared.style_note().into());
            render_activity(&window, &shared.chat.borrow());
        }
    });
    window.on_back({
        let shared = shared.clone();
        move || shared.redraw(|chat| chat.deselect())
    });
    window.global::<ChatList>().on_search({
        let shared = shared.clone();
        move |text| shared.redraw(|chat| chat.search(&text))
    });
    window.global::<Account>().on_skip_history({
        let shared = shared.clone();
        move || shared.command(|chat| chat.skip_history())
    });
    window.global::<Account>().on_start_chats({
        let shared = shared.clone();
        move || shared.redraw(|chat| chat.start())
    });
    window.global::<Account>().on_retry_error({
        let shared = shared.clone();
        move || shared.command(|chat| chat.retry_error())
    });
    window.global::<Account>().on_dismiss_error({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.dismiss_error())
    });
    window.global::<Account>().on_retry({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                (
                    chat.retry(),
                    Update {
                        screen: true,
                        ..Update::default()
                    },
                )
            });
        }
    });
    window.global::<Account>().on_use_password({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                (
                    chat.use_password(),
                    Update {
                        screen: true,
                        ..Update::default()
                    },
                )
            });
        }
    });
    // Both forget everything shown.
    window.global::<Account>().on_start_over({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                (
                    chat.start_over(),
                    Update {
                        screen: true,
                        ..Update::reset()
                    },
                )
            });
        }
    });
    window.global::<Account>().on_unlink({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                (
                    chat.unlink(),
                    Update {
                        screen: true,
                        ..Update::reset()
                    },
                )
            });
        }
    });
    window.global::<Account>().on_rename_device({
        let shared = shared.clone();
        move |name| {
            shared.rename_device(&name);
        }
    });
    window
        .global::<Account>()
        .on_open_data_folder(|| open_folder(b33p_core::settings::data_dir()));
    window
        .global::<Account>()
        .on_open_log_folder(|| open_folder(b33p_core::settings::log_dir()));
    window.global::<Account>().on_open_url(|url| {
        if let Err(error) = open::that_detached(url.as_str()) {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "could not open a web page");
        }
    });
}
