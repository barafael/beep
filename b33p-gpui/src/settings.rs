//! The settings page. Every change goes through [`Shared::apply_settings`], which saves it,
//! applies the theme, and tells the backend; the page reads [`Shared::settings`] each time it is
//! drawn, so what the phone changes (`Shared::take_preferences`) shows without anything more.

use std::rc::Rc;

use b33p_core::settings::{Animations, NotificationContent, Settings, TextSize, Theme};
use b33p_platform::autostart;
use gpui_kit::component::button::{Button, ButtonGroup};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, Focusable as _, FontWeight, IntoElement, Render, Role,
    SharedString, Subscription, Window, div, px,
};

use crate::app::AppView;
use crate::shared::{Confirm, Shared};
use crate::theme::{self, Tokens};
use crate::widgets::{divider, icon_button};

/// The page's column is this wide at most.
const COLUMN: f32 = 520.;
/// How wide the controls on the right of a row are.
const CONTROL: f32 = 200.;

const URL_SOURCE: &str = "https://github.com/barafael/beep";
const URL_REPORT: &str = "https://github.com/barafael/beep/issues/new";

/// The page, made anew each time settings open: what it holds (the device name being typed,
/// whether b33p starts with the system) is read afresh then, as the Slint page's `opened` does.
pub fn page(
    slot: &mut Option<Entity<SettingsView>>,
    shared: &Rc<Shared>,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> AnyElement {
    slot.get_or_insert_with(|| cx.new(|cx| SettingsView::new(shared.clone(), window, cx)))
        .clone()
        .into_any_element()
}

pub struct SettingsView {
    shared: Rc<Shared>,
    device_name: Entity<InputState>,
    /// Whether b33p starts with the system, as the system says.
    start_with_system: bool,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = shared.settings.borrow().device_name();
        let device_name = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let subscription = cx.subscribe(&device_name, |this, input, event: &InputEvent, cx| {
            // "Press Enter to keep a new one".
            if let InputEvent::PressEnter { .. } = event {
                let name = input.read(cx).value().to_string();
                this.shared.rename_device(&name, cx);
            }
        });
        Self {
            shared,
            device_name,
            start_with_system: autostart::enabled(),
            _subscriptions: vec![subscription],
        }
    }

    fn set_start_with_system(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Err(error) = autostart::set(enabled) {
            tracing::warn!(
                error = %b33p_core::scrub::scrubbed(&error),
                "failed to change starting with the system"
            );
        }
        // As the system has it now, which is not what was asked if that failed.
        self.start_with_system = autostart::enabled();
        cx.notify();
    }
}

/// Changes the settings with `edit`, and applies them.
fn change(shared: &Rc<Shared>, cx: &mut App, edit: impl FnOnce(&mut Settings)) {
    let mut settings = shared.settings.borrow().clone();
    edit(&mut settings);
    shared.apply_settings(settings, cx);
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let settings = shared.settings.borrow().clone();
        // The field shows the name in use, unless it is being typed into: left unfinished (no
        // Enter), it goes back to it, and a rename from elsewhere shows.
        let name = settings.device_name();
        let editing = self
            .device_name
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if !editing && self.device_name.read(cx).value().as_ref() != name {
            self.device_name
                .update(cx, |input, cx| input.set_value(name, window, cx));
        }
        let account = shared.chat.borrow().account().to_owned();
        let notifications_supported = shared.notifier.available();
        let tray_available = crate::tray::host_present();
        let macos = cfg!(target_os = "macos");

        let mut column: Vec<AnyElement> = Vec::new();

        column.push(section(&t, "Appearance"));
        column.push(row(
            &t,
            "Theme",
            None,
            segmented(
                &t,
                "theme",
                "Theme",
                &["System", "Light", "Dark"],
                Theme::ALL.iter().position(|t| *t == settings.theme),
                false,
                {
                    let shared = shared.clone();
                    move |index, cx| change(&shared, cx, |s| s.theme = Theme::ALL[index])
                },
            ),
        ));
        column.push(row(
            &t,
            "Text size",
            None,
            segmented(
                &t,
                "text-size",
                "Text size",
                &["Small", "Default", "Large"],
                TextSize::ALL.iter().position(|t| *t == settings.text_size),
                false,
                {
                    let shared = shared.clone();
                    move |index, cx| change(&shared, cx, |s| s.text_size = TextSize::ALL[index])
                },
            ),
        ));
        column.push(row(
            &t,
            "Animations",
            Some("System follows your desktop's setting for reduced motion."),
            segmented(
                &t,
                "animations",
                "Animations",
                &["System", "On", "Off"],
                Animations::ALL
                    .iter()
                    .position(|a| *a == settings.animations),
                false,
                {
                    let shared = shared.clone();
                    move |index, cx| change(&shared, cx, |s| s.animations = Animations::ALL[index])
                },
            ),
        ));
        column.push(switch_row(
            &t,
            "avatars",
            "Profile pictures",
            "Show contacts' and groups' pictures instead of initials.",
            settings.show_avatars,
            true,
            {
                let shared = shared.clone();
                move |on, cx| change(&shared, cx, |s| s.show_avatars = on)
            },
        ));

        column.push(section(&t, "Chats"));
        column.push(switch_row(
            &t,
            "activity",
            "Activity strip",
            "A bar beside each chat showing when you wrote, and how much. Click it to jump there.",
            settings.show_activity,
            true,
            {
                let shared = shared.clone();
                move |on, cx| change(&shared, cx, |s| s.show_activity = on)
            },
        ));
        column.push(switch_row(
            &t,
            "calendar",
            "Activity calendar",
            "A button above each chat opens a calendar of it, year by year. Click a day to go there.",
            settings.show_calendar,
            true,
            {
                let shared = shared.clone();
                move |on, cx| change(&shared, cx, |s| s.show_calendar = on)
            },
        ));

        column.push(section(&t, "Notifications"));
        if notifications_supported {
            column.push(switch_row(
                &t,
                "notifications",
                "New messages",
                "Notify about messages unless their chat is open in front.",
                settings.notifications,
                true,
                {
                    let shared = shared.clone();
                    move |on, cx| change(&shared, cx, |s| s.notifications = on)
                },
            ));
            column.push(row(
                &t,
                "Show",
                None,
                segmented(
                    &t,
                    "notification-content",
                    "Show in notifications",
                    &["Name and message", "Name only", "Nothing"],
                    NotificationContent::ALL
                        .iter()
                        .position(|c| *c == settings.notification_content),
                    !settings.notifications,
                    {
                        let shared = shared.clone();
                        move |index, cx| {
                            change(&shared, cx, |s| {
                                s.notification_content = NotificationContent::ALL[index]
                            })
                        }
                    },
                ),
            ));
        } else {
            column.push(
                div()
                    .text_color(t.text_secondary)
                    .child("b33p can't show notifications on this system yet.")
                    .into_any_element(),
            );
        }

        column.push(section(&t, "Privacy"));
        column.push(switch_row(
            &t,
            "read-receipts",
            "Read receipts",
            "Let people know when you've read their messages. Turned off, you don't see theirs \
             either on your phone.",
            settings.preferences.read_receipts,
            true,
            {
                let shared = shared.clone();
                move |on, cx| change(&shared, cx, |s| s.preferences.read_receipts = on)
            },
        ));
        column.push(switch_row(
            &t,
            "typing-indicators",
            "Typing indicators",
            "Let people see when you're typing.",
            settings.preferences.typing_indicators,
            true,
            {
                let shared = shared.clone();
                move |on, cx| change(&shared, cx, |s| s.preferences.typing_indicators = on)
            },
        ));
        column.push(switch_row(
            &t,
            "link-previews",
            "Link previews",
            "Add a preview of the first link to messages you send. b33p fetches the page itself, \
             so the site sees your IP address.",
            settings.preferences.link_previews,
            true,
            {
                let shared = shared.clone();
                move |on, cx| change(&shared, cx, |s| s.preferences.link_previews = on)
            },
        ));

        column.push(section(&t, "Background"));
        column.push(switch_row(
            &t,
            "tray",
            "Keep running in the tray",
            if tray_available {
                "Closing the window leaves b33p in the tray, still receiving messages."
            } else {
                "Your desktop doesn't show tray icons, so closing the window quits b33p."
            },
            settings.close_to_tray,
            tray_available,
            {
                let shared = shared.clone();
                move |on, cx| change(&shared, cx, |s| s.close_to_tray = on)
            },
        ));
        if autostart::SUPPORTED {
            let in_tray = settings.close_to_tray && tray_available;
            column.push(switch_row(
                &t,
                "autostart",
                "Start with the system",
                if in_tray {
                    "Starts in the tray when you log in."
                } else {
                    "Opens b33p when you log in."
                },
                self.start_with_system,
                true,
                {
                    let this = cx.entity();
                    move |on, cx| this.update(cx, |this, cx| this.set_start_with_system(on, cx))
                },
            ));
        }

        column.push(section(&t, "Keyboard shortcuts"));
        column.push(shortcuts(&t, macos));

        column.push(section(&t, "Account"));
        column.push(row(
            &t,
            "Account",
            None,
            div()
                .w(px(CONTROL))
                .truncate()
                .text_right()
                .text_color(t.text_secondary)
                .child(account)
                .into_any_element(),
        ));
        column.push(switch_row(
            &t,
            "sync-to-phone",
            "Sync chat settings to your phone",
            "Pinned, muted, archived and blocked chats and accepted requests changed here go to \
             your phone too. Off: they stay on this computer, and changes on the phone still come \
             here.",
            settings.preferences.sync_to_phone,
            true,
            {
                let shared = shared.clone();
                move |on, cx| change(&shared, cx, |s| s.preferences.sync_to_phone = on)
            },
        ));
        column.push(row(
            &t,
            "Device name",
            Some(
                "How this computer appears under Linked devices on your phone. Press Enter to \
                 keep a new one; it is used the next time this device is linked.",
            ),
            div()
                .w(px(CONTROL))
                .child(Input::new(&self.device_name).aria_label("Device name"))
                .into_any_element(),
        ));
        column.push(row(
            &t,
            "Unlink this device",
            Some(
                "Removes this computer from your Signal account and deletes the messages stored \
                 here. Your phone keeps them.",
            ),
            Button::new("unlink")
                .label("Unlink…")
                .debug_selector(|| "Unlink…".into())
                .on_click({
                    let shared = shared.clone();
                    move |_, _, cx| {
                        shared.ui.borrow_mut().confirm = Confirm::Unlink;
                        cx.refresh_windows();
                    }
                })
                .into_any_element(),
        ));

        column.push(section(&t, "About"));
        column.push(row(
            &t,
            &format!("b33p {}", env!("CARGO_PKG_VERSION")),
            Some("Free software under the GNU AGPL 3.0. Not made by Signal."),
            Button::new("source")
                .label("Source code")
                .debug_selector(|| "Source code".into())
                .on_click(|_, _, _| crate::web::open(URL_SOURCE))
                .into_any_element(),
        ));
        column.push(row(
            &t,
            "Report a problem",
            Some(
                "Describe what happened and attach the log. It leaves out messages, names and \
                 numbers, but look it over first.",
            ),
            div()
                .flex()
                .flex_row()
                .gap(px(theme::SPACE_S))
                .child(
                    Button::new("log-folder")
                        .label("Open log folder")
                        .debug_selector(|| "Open log folder".into())
                        .on_click(|_, _, _| {
                            crate::start::open_folder(b33p_core::settings::log_dir())
                        }),
                )
                .child(
                    Button::new("report")
                        .label("Report…")
                        .debug_selector(|| "Report…".into())
                        .on_click(|_, _, _| crate::web::open(URL_REPORT)),
                )
                .into_any_element(),
        ));

        div()
            .id("settings")
            .role(Role::Group)
            .aria_label("Settings")
            .size_full()
            // Shrinks with the window, however long its rows: next to the chat list, a page
            // that can't would run out of the window.
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                // As tall as the chat's header, which it takes the place of.
                div()
                    .flex_none()
                    .h(px(theme::HEADER))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACE_S))
                    .pl(px(theme::SPACE_S))
                    .pr(px(theme::SPACE_M))
                    .child(icon_button(
                        "close-settings",
                        "arrow-left",
                        "Close settings",
                        {
                            let shared = shared.clone();
                            move |_, _, cx| {
                                shared.ui.borrow_mut().settings_open = false;
                                cx.refresh_windows();
                            }
                        },
                    ))
                    .child(
                        div()
                            .id("settings-title")
                            .role(Role::Heading)
                            .aria_label("Settings")
                            .flex_1()
                            .truncate()
                            .text_size(px(theme::BODY * theme::TITLE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Settings"),
                    ),
            )
            .child(divider(&t))
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scrollbar()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_center()
                            .px(px(theme::SPACE_XL))
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .max_w(px(COLUMN))
                                    .py(px(theme::SPACE_XL))
                                    .flex()
                                    .flex_col()
                                    .gap(px(14.))
                                    .children(column),
                            ),
                    ),
            )
    }
}

/// A group's title, with room above it.
fn section(t: &Tokens, title: &'static str) -> AnyElement {
    div()
        .id(SharedString::from(format!("section-{title}")))
        .role(Role::Heading)
        .aria_label(title)
        .pt(px(theme::SPACE_M))
        .text_size(px(theme::BODY * theme::BODY_SMALL))
        .font_weight(FontWeight::BOLD)
        .text_color(t.accent)
        .child(title)
        .into_any_element()
}

/// A label and what explains it on the left, the control on the right; the control goes under
/// them where the window is too narrow for both.
fn row(t: &Tokens, label: &str, hint: Option<&str>, control: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_x(px(theme::SPACE_L))
        .gap_y(px(theme::SPACE_S))
        .child(
            div()
                .flex_1()
                .min_w(px(180.))
                .flex()
                .flex_col()
                .child(SharedString::from(label.to_owned()))
                .children(hint.map(|hint| {
                    div()
                        .text_size(px(theme::BODY * theme::SMALL))
                        .text_color(t.text_secondary)
                        .child(SharedString::from(hint.to_owned()))
                })),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .justify_end()
                .child(control),
        )
        .into_any_element()
}

/// A setting that is on or off.
fn switch_row(
    t: &Tokens,
    id: &'static str,
    label: &'static str,
    hint: &'static str,
    checked: bool,
    enabled: bool,
    on_change: impl Fn(bool, &mut App) + 'static,
) -> AnyElement {
    let switch = Switch::new(SharedString::from(format!("switch-{id}")))
        .checked(checked)
        .disabled(!enabled)
        .accessibility_label(label)
        .on_change(move |on, _, cx| on_change(*on, cx));
    row(t, label, Some(hint), switch.into_any_element())
}

/// One of a few choices, side by side.
fn segmented(
    t: &Tokens,
    id: &'static str,
    label: &'static str,
    options: &[&'static str],
    selected: Option<usize>,
    disabled: bool,
    on_pick: impl Fn(usize, &mut App) + 'static,
) -> AnyElement {
    let accent = t.accent;
    let group = ButtonGroup::new(SharedString::from(format!("choice-{id}")))
        .small()
        .disabled(disabled)
        .children(options.iter().enumerate().map(|(index, option)| {
            Button::new(SharedString::from(format!("choice-{id}-{index}")))
                .label(*option)
                .selected(selected == Some(index))
                .map(|button| {
                    // The choice in force is in the accent; the others are outlined.
                    if selected == Some(index) {
                        button.outline().bg(accent.opacity(0.18)).text_color(accent)
                    } else {
                        button.outline()
                    }
                })
        }))
        .on_click(move |picked: &Vec<usize>, _, cx| {
            if let Some(index) = picked.first() {
                on_pick(*index, cx);
            }
        });
    div()
        .id(SharedString::from(format!("choices-{id}")))
        .role(Role::RadioGroup)
        .aria_label(label)
        .child(group)
        .into_any_element()
}

/// The shortcuts and what they do, in the signs of the desktop's own keyboard: `(keys, what)`.
fn shortcut_list(macos: bool) -> Vec<(String, String)> {
    let pick = |mac: &str, other: &str| if macos { mac } else { other }.to_owned();
    let both = |keys: &str| keys.to_owned();
    let row = |keys: String, what: &str| (keys, what.to_owned());
    vec![
        row(pick("⌘K", "Ctrl+K"), "Search chats"),
        row(pick("⌘F", "Ctrl+F"), "Search this chat"),
        row(pick("⌥↑ / ⌥↓", "Alt+↑ / Alt+↓"), "Previous / next chat"),
        row(pick("⌥⇧↓", "Alt+Shift+↓"), "Next unread chat"),
        row(pick("⌥1 … ⌥9", "Alt+1 … Alt+9"), "Pinned chats"),
        row(
            both("↑ ↓ Enter"),
            "In the search for chats: go through those found, open one",
        ),
        row(both("↑"), "Edit your last message (in an empty composer)"),
        row(pick("⇧↩", "Shift+Enter"), "New line"),
        row(pick("⌘V", "Ctrl+V"), "Paste text, a picture or files"),
        row(
            pick("⌘-click", "Ctrl+click"),
            if macos {
                "Select a message; ⇧-click selects up to it, Esc clears"
            } else {
                "Select a message; Shift+click selects up to it, Esc clears"
            },
        ),
        row(pick("⌘C", "Ctrl+C"), "Copy the text selected in a message"),
        row(
            both("Esc"),
            "Close, stop replying or editing, or clear the search or selection",
        ),
    ]
}

fn shortcuts(t: &Tokens, macos: bool) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .children(shortcut_list(macos).into_iter().map(|(keys, what)| {
            div()
                .flex()
                .flex_row()
                .gap(px(theme::SPACE_L))
                .child(
                    // The keys as a key cap, in a column of their own.
                    div()
                        .flex_none()
                        .w(px(140.))
                        .flex()
                        .flex_row()
                        .items_start()
                        .child(
                            div()
                                .flex_none()
                                .px(px(theme::SPACE_S))
                                .py(px(2.))
                                .rounded(px(theme::RADIUS_S))
                                .bg(t.surface_raised)
                                .border_1()
                                .border_color(t.divider)
                                .text_size(px(theme::BODY * theme::BODY_SMALL))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(SharedString::from(keys)),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(t.text_secondary)
                        .child(SharedString::from(what)),
                )
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shortcuts_are_in_the_signs_of_the_keyboard() {
        let mac = shortcut_list(true);
        let other = shortcut_list(false);
        assert_eq!(mac.len(), other.len());
        assert_eq!(mac[0].0, "⌘K");
        assert_eq!(other[0].0, "Ctrl+K");
        assert!(mac.iter().all(|(keys, _)| !keys.contains("Ctrl")));
        assert!(other.iter().all(|(keys, _)| !keys.contains('⌘')));
        // What each does doesn't depend on the keyboard, except the one that names a key.
        let differing = mac.iter().zip(&other).filter(|(m, o)| m.1 != o.1).count();
        assert_eq!(differing, 1);
    }
}
