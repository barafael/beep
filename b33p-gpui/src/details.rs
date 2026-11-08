//! A chat's details page over its messages: for a contact their about line, number and safety
//! number to verify; for a group its name, picture, description, members, invite link and who
//! asked to join. Both have the notifications and the disappearing-messages timer.

use std::rc::Rc;

use b33p_core::state::{self, Update};
use b33p_core::{ConversationId, Details, GroupEdit, InviteLinkAccess, Person, Trust};
use b33p_platform::desktop::Pick;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AsyncApp, ClipboardItem, Context, Entity, Focusable as _, FontWeight,
    IntoElement, Render, SharedString, Subscription, Window, div, px,
};

use crate::chat_list::MUTE_CHOICES;
use crate::images::Drawn;
use crate::panel;
use crate::shared::{Confirm, Shared};
use crate::theme::{self, Tokens};
use crate::widgets::{avatar, avatar_color, icon};

/// How long messages last in a chat with disappearing messages, in seconds (0 for off).
pub const TIMER_CHOICES: [(&str, u32); 8] = [
    ("Off", 0),
    ("30 seconds", 30),
    ("5 minutes", 300),
    ("1 hour", 3600),
    ("8 hours", 28800),
    ("1 day", 86400),
    ("1 week", 604800),
    ("4 weeks", 2419200),
];

/// The page reads as a column this wide, also in a wide window.
const COLUMN: f32 = 520.;

pub struct DetailsPanel {
    shared: Rc<Shared>,
    name: Entity<InputState>,
    description: Entity<InputState>,
    /// What the fields were last set to from the state, so that they follow it (the title
    /// changes on the phone) without overwriting what is being typed.
    shown: Option<(String, String)>,
    _subscriptions: Vec<Subscription>,
}

/// What the page shows of the chat, copied so that no borrow of the state outlives the draw.
struct Page {
    id: ConversationId,
    title: String,
    subtitle: String,
    initials: String,
    group: bool,
    left: bool,
    admin: bool,
    request: bool,
    blocked: bool,
    muted: bool,
    timer: u32,
    picture: Option<Drawn>,
    details: Option<Details>,
}

impl DetailsPanel {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let description = cx.new(|cx| InputState::new(window, cx).placeholder("Description"));
        let subscriptions = vec![
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.rename(cx);
                }
            }),
            cx.subscribe_in(
                &description,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.describe(cx);
                    }
                },
            ),
        ];
        Self {
            shared,
            name,
            description,
            shown: None,
            _subscriptions: subscriptions,
        }
    }

    /// The panel is not shown: the fields start from the state again when it is.
    pub fn closed(&mut self) {
        self.shown = None;
    }

    fn rename(&mut self, cx: &mut Context<Self>) {
        let text = self.name.read(cx).value().to_string();
        edit_group(&self.shared, cx, GroupEdit::Title(text));
    }

    fn describe(&mut self, cx: &mut Context<Self>) {
        let text = self.description.read(cx).value().to_string();
        edit_group(&self.shared, cx, GroupEdit::Description(text));
    }

    /// Puts the title and description the state has in the fields, when they changed and are
    /// not being typed in.
    fn follow_state(
        &mut self,
        title: &str,
        description: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = (title.to_owned(), description.to_owned());
        if self.shown.as_ref() == Some(&now) {
            return;
        }
        let first = self.shown.is_none();
        let (old_title, old_description) = self.shown.clone().unwrap_or_default();
        self.shown = Some(now);
        for (field, old, new) in [
            (&self.name, old_title, title),
            (&self.description, old_description, description),
        ] {
            if first || (new != old && !field.focus_handle(cx).is_focused(window)) {
                let new = new.to_owned();
                field.update(cx, |input, cx| input.set_value(new, window, cx));
            }
        }
    }
}

impl Render for DetailsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let Some((page, busy)) = page(&shared) else {
            return panel::sheet("details-panel", &t).into_any_element();
        };
        let description = page
            .details
            .as_ref()
            .and_then(|d| d.description.clone())
            .unwrap_or_default();
        self.follow_state(&page.title, &description, window, cx);
        let color = avatar_color(&page.id);
        let close = {
            let shared = shared.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                shared.edit(cx, |chat| chat.close_details());
            }
        };
        // The page is a copy already; its members and requests are not copied again.
        let no_details = Default::default();
        let details = page.details.as_ref().unwrap_or(&no_details);
        let about = details.about.clone().unwrap_or_default();
        let phone = details.phone.clone().filter(|p| !p.is_empty());
        let has_picture = page.picture.is_some();
        let mut column = div()
            .w(px(COLUMN))
            .max_w_full()
            .flex()
            .flex_col()
            .gap(px(theme::SPACE_M))
            .px(px(theme::SPACE_L))
            .pt(px(theme::SPACE_L))
            .pb(px(theme::SPACE_XL))
            .child(div().flex().justify_center().child(avatar(
                96.,
                &page.initials,
                color,
                page.picture.clone(),
                &t,
            )))
            .child(
                div()
                    .text_center()
                    .text_size(px(theme::BODY * theme::DISPLAY))
                    .font_weight(FontWeight::BOLD)
                    .child(SharedString::from(page.title.clone())),
            )
            .when(!about.is_empty(), |c| {
                c.child(
                    div()
                        .text_center()
                        .text_color(t.text_secondary)
                        .child(SharedString::from(about.clone())),
                )
            })
            .child(
                div()
                    .text_center()
                    .text_color(t.text_tertiary)
                    .child(SharedString::from(
                        phone.clone().unwrap_or_else(|| page.subtitle.clone()),
                    )),
            )
            .when(busy, |c| c.child(panel::busy()))
            .child(
                div().flex().justify_center().child(
                    Button::new("details-media")
                        .icon(icon("image"))
                        .label("Media, files and links")
                        .debug_selector(|| "Media, files and links".into())
                        .on_click({
                            let shared = shared.clone();
                            move |_, _, cx| {
                                shared.act(cx, |chat| {
                                    chat.close_details();
                                    (chat.open_media(), Update::default())
                                });
                            }
                        }),
                ),
            )
            .child(self.chat_section(&page, &t));
        if !description.is_empty() {
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(theme::SPACE_XS))
                    .child(panel::section_title(&t, "Description"))
                    .child(SharedString::from(description.clone())),
            );
        }
        if let Some(number) = details.safety_number.as_deref() {
            column = column.child(self.safety_section(number, details.trust, &t, cx));
        }
        let in_group = page.group && !page.left;
        if in_group {
            column = column.child(self.group_section(has_picture, &t, cx));
            column = column.child(self.invite_section(&page, details, &t));
            if page.admin && !details.requests.is_empty() {
                column = column.child(self.requests_section(&details.requests, &t));
            }
        }
        if !details.members.is_empty() {
            column = column.child(self.members_section(&page, details, &t));
        }
        panel::sheet("details-panel", &t)
            .child(panel::header(
                &t,
                "Back to chat",
                if page.group {
                    "Group details"
                } else {
                    "Details"
                },
                None,
                None,
                close,
            ))
            .child(
                div()
                    .id("details-scroll")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .justify_center()
                    .overflow_y_scrollbar()
                    .child(column),
            )
            .into_any_element()
    }
}

/// The open chat and what is known about it, if the details page is open; whether that is
/// still being gathered.
fn page(shared: &Rc<Shared>) -> Option<(Page, bool)> {
    let chat = shared.chat.borrow();
    let gathered = chat.details()?;
    let view = chat.selected_view()?;
    let c = view.conversation;
    let page = Page {
        id: c.id.clone(),
        title: c.title.clone(),
        subtitle: c.subtitle.clone(),
        initials: state::initials(&c.title),
        group: c.is_group(),
        left: c.left,
        admin: c.admin,
        request: chat.is_request(c),
        blocked: c.settings.blocked,
        muted: view.muted(),
        timer: c.expire_timer,
        picture: shared
            .images
            .avatar(&format!("chat:{}", c.id.0), view.avatar),
        details: gathered.cloned(),
    };
    Some((page, gathered.is_none()))
}

impl DetailsPanel {
    /// Notifications and the disappearing-messages timer.
    fn chat_section(&self, page: &Page, t: &Tokens) -> AnyElement {
        let shared = self.shared.clone();
        let id = page.id.clone();
        let notifications = if page.muted {
            Button::new("details-unmute")
                .label("Unmute")
                .debug_selector(|| "Unmute".into())
                .on_click({
                    let (shared, id) = (shared.clone(), id.clone());
                    move |_, _, cx| shared.chat_action(cx, &id, |chat, id| chat.mute(id, None))
                })
                .into_any_element()
        } else {
            let (shared, id) = (shared.clone(), id.clone());
            Button::new("details-mute")
                .outline()
                .label("On")
                .debug_selector(|| "On".into())
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for (label, seconds) in MUTE_CHOICES {
                        let (shared, id) = (shared.clone(), id.clone());
                        // "For 1 hour" reads "Off for 1 hour"; for good it is just "Off".
                        let label = if seconds < 0 {
                            "Off".to_owned()
                        } else {
                            format!("Off {}", label.to_lowercase())
                        };
                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            shared.chat_action(cx, &id, |chat, id| {
                                chat.mute(id, Some(crate::chat_list::mute_until(seconds)))
                            })
                        }));
                    }
                    menu
                })
                .into_any_element()
        };
        let current = TIMER_CHOICES.iter().position(|(_, s)| *s == page.timer);
        let current_label = current
            .map(|i| TIMER_CHOICES[i].0.to_owned())
            .unwrap_or_else(|| b33p_core::format_timer(page.timer));
        // Only where we can still write: the timer is a message to them.
        let timer = (!page.request && !page.blocked && !page.left).then(|| {
            let (shared, id) = (shared.clone(), id.clone());
            let button = Button::new("details-timer")
                .outline()
                .label(current_label.clone())
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for (index, (label, seconds)) in TIMER_CHOICES.into_iter().enumerate() {
                        let (shared, id) = (shared.clone(), id.clone());
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .checked(current == Some(index))
                                .on_click(move |_, _, cx| {
                                    shared.chat_action(cx, &id, |chat, id| {
                                        chat.set_timer(id, seconds)
                                    })
                                }),
                        );
                    }
                    menu
                });
            let hint = (page.timer > 0 && current.is_none())
                .then(|| format!("Set to {}", b33p_core::format_timer(page.timer)));
            setting_row(t, "Disappearing messages", hint, button.into_any_element())
        });
        div()
            .flex()
            .flex_col()
            .gap(px(theme::SPACE_S))
            .child(panel::section_title(t, "Chat"))
            .child(setting_row(
                t,
                "Notifications",
                page.muted.then(|| "Muted".to_owned()),
                notifications,
            ))
            .children(timer)
            .into_any_element()
    }

    fn safety_section(
        &self,
        number: &str,
        trust: Trust,
        t: &Tokens,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let shared = self.shared.clone();
        let grouped = safety_lines(number);
        let verified = trust == Trust::Verified;
        let copy = grouped.replace('\n', " ");
        div()
            .flex()
            .flex_col()
            .gap(px(theme::SPACE_S))
            .child(panel::section_title(t, "Safety number"))
            .child(panel::hint(
                t,
                "Compare it with the one on their phone, in person or another way you trust. \
                 If both match, nobody is in between: your messages are end-to-end encrypted \
                 with them.",
            ))
            .child(
                div().flex().flex_row().child(
                    div()
                        .px(px(theme::SPACE_M))
                        .py(px(theme::SPACE_M))
                        .rounded(px(theme::RADIUS_M))
                        .bg(t.surface_raised)
                        .border_1()
                        .border_color(t.divider)
                        .font_family("monospace")
                        .text_size(px(theme::BODY * theme::TITLE))
                        .child(SharedString::from(grouped)),
                ),
            )
            .when(trust == Trust::Changed, |c| {
                c.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(theme::SPACE_S))
                        .items_start()
                        .child(
                            icon("shield-alert")
                                .size(px(theme::ICON))
                                .text_color(t.danger_text),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(theme::BODY * theme::BODY_SMALL))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(
                                    "Changed since you verified it. This is the new one: \
                                     compare it with theirs again.",
                                ),
                        ),
                )
            })
            .when(verified, |c| {
                c.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(theme::SPACE_S))
                        .items_center()
                        .text_color(t.accent)
                        .child(icon("shield-check").size(px(theme::ICON)))
                        .child(
                            div()
                                .text_size(px(theme::BODY * theme::BODY_SMALL))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Verified: you compared it with theirs."),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(theme::SPACE_S))
                    .child(
                        Button::new("safety-copy")
                            .label("Copy")
                            .debug_selector(|| "Copy".into())
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                            }),
                    )
                    .child(
                        Button::new("safety-verify")
                            .label(if verified {
                                "Clear verification"
                            } else {
                                "Mark as verified"
                            })
                            .debug_selector(move || {
                                if verified {
                                    "Clear verification"
                                } else {
                                    "Mark as verified"
                                }
                                .into()
                            })
                            .on_click(move |_, _, cx| {
                                shared.command(cx, |chat| chat.set_verified(!verified))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// The group's name, description and picture; adding people, leaving.
    fn group_section(&self, has_picture: bool, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let shared = self.shared.clone();
        let field = |input: &Entity<InputState>| div().flex_1().min_w_0().child(Input::new(input));
        div()
            .flex()
            .flex_col()
            .gap(px(theme::SPACE_S))
            .child(panel::section_title(t, "Group"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(theme::SPACE_S))
                    .child(field(&self.name))
                    .child(
                        Button::new("group-rename")
                            .label("Rename")
                            .debug_selector(|| "Rename".into())
                            .on_click(cx.listener(|this, _, _, cx| this.rename(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(theme::SPACE_S))
                    .child(field(&self.description))
                    .child(
                        Button::new("group-describe")
                            .label("Save")
                            .debug_selector(|| "Save".into())
                            .on_click(cx.listener(|this, _, _, cx| this.describe(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(theme::SPACE_S))
                    .child(
                        Button::new("group-picture")
                            .label("Change picture…")
                            .debug_selector(|| "Change picture…".into())
                            .on_click({
                                let shared = shared.clone();
                                move |_, _, cx| choose_picture(&shared, cx)
                            }),
                    )
                    .when(has_picture, |row| {
                        row.child(
                            Button::new("group-picture-remove")
                                .label("Remove picture")
                                .debug_selector(|| "Remove picture".into())
                                .on_click({
                                    let shared = shared.clone();
                                    move |_, _, cx| {
                                        edit_group(&shared, cx, GroupEdit::RemovePicture)
                                    }
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(theme::SPACE_S))
                    .child(
                        Button::new("group-add")
                            .label("Add members…")
                            .debug_selector(|| "Add members…".into())
                            .on_click({
                                let shared = shared.clone();
                                move |_, _, cx| shared.edit(cx, |chat| chat.open_add_members())
                            }),
                    )
                    .child(
                        Button::new("group-leave")
                            .label("Leave group…")
                            .debug_selector(|| "Leave group…".into())
                            .on_click({
                                let shared = shared.clone();
                                move |_, _, cx| shared.confirm(Confirm::LeaveGroup, None, cx)
                            }),
                    ),
            )
            .into_any_element()
    }

    fn invite_section(&self, page: &Page, details: &Details, t: &Tokens) -> AnyElement {
        let shared = self.shared.clone();
        let link = details.invite_link.clone().filter(|l| !l.is_empty());
        let access = details.invite_link_access;
        let admin = page.admin;
        div()
            .flex()
            .flex_col()
            .gap(px(theme::SPACE_S))
            .child(panel::section_title(t, "Invite link"))
            .child(panel::hint(
                t,
                "Anyone with the link can join the group, or ask to.",
            ))
            .when(admin, |c| {
                c.child(
                    div().flex().flex_row().gap(px(theme::SPACE_XS)).children(
                        [
                            ("Off", InviteLinkAccess::Off),
                            ("Anyone", InviteLinkAccess::Anyone),
                            ("Admins approve", InviteLinkAccess::Approval),
                        ]
                        .into_iter()
                        .map(|(label, choice)| {
                            let shared = shared.clone();
                            panel::pill(t, label, label, access == choice, move |_, _, cx| {
                                edit_group(&shared, cx, GroupEdit::InviteLink(choice))
                            })
                        }),
                    ),
                )
            })
            .children(link.map(|link| {
                let copy = link.clone();
                let shared = shared.clone();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACE_S))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .px(px(10.))
                            .py(px(10.))
                            .rounded(px(theme::RADIUS_M))
                            .bg(t.surface_raised)
                            .border_1()
                            .border_color(t.divider)
                            .text_size(px(theme::BODY * theme::BODY_SMALL))
                            .child(SharedString::from(link)),
                    )
                    .child(
                        Button::new("invite-copy")
                            .label("Copy")
                            .debug_selector(|| "Copy".into())
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                            }),
                    )
                    .when(admin, |row| {
                        row.child(
                            Button::new("invite-reset")
                                .label("Reset")
                                .debug_selector(|| "Reset".into())
                                .on_click(move |_, _, cx| {
                                    shared.confirm(Confirm::ResetInviteLink, None, cx)
                                }),
                        )
                    })
            }))
            .into_any_element()
    }

    fn requests_section(&self, requests: &[Person], t: &Tokens) -> AnyElement {
        let shared = self.shared.clone();
        let title = if requests.len() == 1 {
            "1 request to join".to_owned()
        } else {
            format!("{} requests to join", requests.len())
        };
        div()
            .flex()
            .flex_col()
            .gap(px(theme::SPACE_XS))
            .child(panel::section_title(t, title))
            .children(requests.iter().enumerate().map(|(index, person)| {
                let (deny_shared, approve_shared) = (shared.clone(), shared.clone());
                let (deny_id, approve_id) = (person.service_id.clone(), person.service_id.clone());
                person_row(person, t)
                    .child(
                        Button::new(("request-deny", index))
                            .label("Turn down")
                            .debug_selector(|| "Turn down".into())
                            .on_click(move |_, _, cx| {
                                edit_group(
                                    &deny_shared,
                                    cx,
                                    GroupEdit::DenyRequest(deny_id.clone()),
                                )
                            }),
                    )
                    .child(
                        Button::new(("request-approve", index))
                            .primary()
                            .label("Let in")
                            .debug_selector(|| "Let in".into())
                            .on_click(move |_, _, cx| {
                                edit_group(
                                    &approve_shared,
                                    cx,
                                    GroupEdit::ApproveRequest(approve_id.clone()),
                                )
                            }),
                    )
            }))
            .into_any_element()
    }

    fn members_section(&self, page: &Page, details: &Details, t: &Tokens) -> AnyElement {
        let shared = self.shared.clone();
        let left = page.left;
        div()
            .flex()
            .flex_col()
            .gap(px(theme::SPACE_XS))
            .child(panel::section_title(
                t,
                format!("{} members", details.members.len()),
            ))
            .children(
                details
                    .members
                    .iter()
                    .enumerate()
                    .map(|(index, (person, admin))| {
                        let admin = *admin;
                        let manage = (person.name != "You" && !left).then(|| {
                            let shared = shared.clone();
                            let member = person.service_id.clone();
                            let name = person.name.clone();
                            Button::new(("member-manage", index))
                                .ghost()
                                .small()
                                .icon(icon("ellipsis-vertical"))
                                .tooltip(format!("Manage {}", person.name))
                                .dropdown_menu(move |menu, _, _| {
                                    let (toggle_shared, remove_shared) =
                                        (shared.clone(), shared.clone());
                                    let (toggle_id, remove_id) = (member.clone(), member.clone());
                                    let remove_name = name.clone();
                                    menu.item(
                                        PopupMenuItem::new(if admin {
                                            "Take admin rights"
                                        } else {
                                            "Make admin"
                                        })
                                        .on_click(
                                            move |_, _, cx| {
                                                edit_group(
                                                    &toggle_shared,
                                                    cx,
                                                    GroupEdit::SetAdmin(toggle_id.clone(), !admin),
                                                )
                                            },
                                        ),
                                    )
                                    .item(
                                        PopupMenuItem::new("Remove from group").on_click(
                                            move |_, _, cx| {
                                                remove_shared.confirm_removing(
                                                    &remove_id,
                                                    &remove_name,
                                                    cx,
                                                )
                                            },
                                        ),
                                    )
                                })
                        });
                        person_row(person, t)
                            .when(admin, |row| {
                                row.child(
                                    div()
                                        .text_size(px(theme::BODY * theme::SMALL))
                                        .text_color(t.text_tertiary)
                                        .child("Admin"),
                                )
                            })
                            .children(manage)
                    }),
            )
            .into_any_element()
    }
}

/// A person as a row: their picture of initials and name, with room for buttons after.
fn person_row(person: &Person, t: &Tokens) -> gpui_kit::Div {
    let color = avatar_color(&ConversationId::contact(&person.service_id));
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .py(px(2.))
        .child(avatar(32., &state::initials(&person.name), color, None, t))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(person.name.clone())),
        )
}

/// A label (with a hint under it) and what changes it, at the end of the row.
fn setting_row(
    t: &Tokens,
    label: &'static str,
    hint: Option<String>,
    control: AnyElement,
) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_L))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(label)
                .children(hint.map(|hint| panel::hint(t, hint))),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// Four groups of five digits per line, as on phones.
fn safety_lines(number: &str) -> String {
    let groups: Vec<&str> = number.split_whitespace().collect();
    groups
        .chunks(4)
        .map(|line| line.join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Changes the open group.
fn edit_group(shared: &Rc<Shared>, cx: &mut App, edit: GroupEdit) {
    shared.act(cx, |chat| {
        let id = chat.selected().map(|c| c.id.clone());
        (
            id.and_then(|id| chat.edit_group(&id, edit)),
            Update::default(),
        )
    });
}

/// Asks for a picture on the desktop and makes it the group's.
fn choose_picture(shared: &Rc<Shared>, cx: &mut App) {
    let Some(picked) = b33p_platform::desktop::ask_pick(
        &shared.desktop,
        "Choose the group's picture",
        Pick::Picture,
    ) else {
        return;
    };
    let shared = shared.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Ok(files) = picked.await
            && let Some(path) = files.into_iter().next()
        {
            cx.update(|cx| edit_group(&shared, cx, GroupEdit::SetPicture(path)));
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_numbers_are_four_groups_to_a_line() {
        let number = "11111 22222 33333 44444 55555 66666 77777 88888 99999 00000 12345 67890";
        assert_eq!(
            safety_lines(number),
            "11111 22222 33333 44444\n55555 66666 77777 88888\n99999 00000 12345 67890"
        );
    }

    #[test]
    fn the_timer_choices_are_in_order() {
        assert!(TIMER_CHOICES.windows(2).all(|w| w[0].1 < w[1].1));
    }
}
