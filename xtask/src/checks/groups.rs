use b33p_harness::{Action, InviteLinkAccess};

use super::b33p::B33pError;
use super::b33p::{
    accept_request, b33p_chat, b33p_details, b33p_shows, b33p_title, chat_previewing, select,
    story_shows, type_and_send,
};
use super::clients::{Android, android_containing, android_shows, desktop_shows, desktop_text};
use super::presage::{Account, Presage};
use super::setup::{AndroidBox, started_b33p};
use super::{Checks, Failure, PATIENCE, PATIENCE_MS};
use crate::Sockets;
use crate::harness::Harness;
use crate::pod::{B33P, Pod};
use crate::process;
use testbox_tools::drivers::{self, MemberChange};

/// What the group checks find wrong of their own.
#[derive(Debug, thiserror::Error)]
enum GroupsError {
    /// A thread the check sent something from panicked.
    #[error("{thread} panicked")]
    Panicked { thread: &'static str },
}

/// A group b33p makes of its account (A, with Android) and the peer (B, with Desktop) and a
/// presage-cli member (C); what b33p changes shows on Desktop and Android, what Desktop changes
/// on b33p.
pub(super) fn groups(
    checks: &mut Checks,
    fixture: &mut AndroidBox<'_>,
    containers: &Pod,
    sockets: &Sockets,
) -> Result<(), Failure> {
    let AndroidBox {
        peer,
        android,
        b33p,
        desktop,
        android_aci,
        conversation,
    } = fixture;
    let member = Presage::new(containers, Account::Member);
    checks.require("a presage-cli member registers", || {
        member.register()?;
        Ok(())
    })?;
    // The member's message brings b33p its profile key, as the fixture's brought the peer's: with
    // them b33p adds them to the group outright, without, it invites them (presage
    // `create_group`). The peer's at once, so that the two deliveries overlap.
    let (peer_text, member_text) = (
        "testbox groups: from the peer",
        "testbox groups: from the member",
    );
    checks.require("the peer and the member write to b33p's account", || {
        std::thread::scope(|scope| -> Result<(), Failure> {
            let from_peer = scope.spawn(|| peer.send(android_aci, peer_text));
            let from_member = member.send(android_aci, member_text);
            from_peer.join().map_err(|_| GroupsError::Panicked {
                thread: "the peer's send",
            })??;
            from_member?;
            Ok(())
        })
    })?;
    checks.require("the peer's text beside the member's reaches b33p", || {
        chat_previewing(b33p, peer_text)?;
        Ok(())
    })?;
    let member_chat = checks.require("text from the member reaches b33p", || {
        Ok(chat_previewing(b33p, member_text)?)
    })?;
    checks.require(
        "the member's first message is a message request in b33p, accepted",
        || Ok(accept_request(b33p, &member_chat)?),
    )?;

    // TESTPLAN.md §3.1.
    let title = "testbox group";
    let group = checks.require("b33p makes a group of the peer and the member", || {
        b33p.act(Action::CreateGroup {
            title: title.to_owned(),
            members: vec![conversation.clone(), member_chat.clone()],
        })?;
        Ok(b33p.wait_for("the group in b33p", PATIENCE, |snapshot| {
            snapshot
                .conversations
                .iter()
                .find(|chat| chat.group && chat.title == title && !chat.request)
                .map(|chat| chat.id.clone())
        })?)
    })?;
    // A first message in it: Desktop lists a conversation once it has one (its log: the group's
    // change kept, the empty message with it dropped). Sending isn't a group change, so it passes
    // where b33p's group changes fail (below).
    let hello = "testbox groups: hello, group";
    checks.record(
        "text b33p sends to the group reaches Desktop and Android",
        || {
            select(b33p, &group)?;
            type_and_send(b33p, hello)?;
            desktop.call(drivers::Desktop::OpenTitled {
                title: title.to_owned(),
            })?;
            desktop_shows(desktop, hello)?;
            // By its name: the chat list's preview of it is "You: <text>".
            android.call(drivers::Android::OpenConversation {
                text: title.to_owned(),
            })?;
            Ok(android_shows(android, hello)?)
        },
    );

    let by_desktop = "testbox group, named on Desktop";
    checks.record(
        "a name Desktop gives the group shows on b33p and Android",
        || {
            // The group's current name, as b33p has it: Desktop opens its settings by it.
            desktop.call(drivers::Desktop::EditGroup {
                old_title: b33p_title(b33p, &group)?,
                title: Some(by_desktop.to_owned()),
                description: None,
            })?;
            b33p_chat(b33p, &group, "named on Desktop", |chat| {
                chat.title == by_desktop
            })?;
            Ok(android_containing(android, by_desktop)?)
        },
    );
    let description = "testbox: a group to test groups with";
    // Desktop's and Android's notices are worded around who changed it (strings.xml
    // `MessageRecord_s_changed_the_group_description`); b33p shows the text in its details.
    checks.record(
        "a description Desktop gives the group shows on b33p and Android",
        || {
            desktop.call(drivers::Desktop::EditGroup {
                old_title: b33p_title(b33p, &group)?,
                title: None,
                description: Some(description.to_owned()),
            })?;
            b33p_details(
                b33p,
                "Peer changed the group's description.",
                "the group's description",
                |details| (details.description.as_deref() == Some(description)).then_some(()),
            )?;
            Ok(android_containing(
                android,
                "changed the group description",
            )?)
        },
    );
    // The picture goes by CDN0, by the form storage-service hands out (`GET
    // /v2/groups/avatar/form`, Groups.proto `AvatarUploadAttributes`): one of Desktop's presets,
    // which it uploads as a picture of its own.
    checks.record(
        "a picture Desktop gives the group shows on b33p and Android",
        || {
            desktop.call(drivers::Desktop::SetGroupPicture {
                old_title: b33p_title(b33p, &group)?,
            })?;
            b33p_shows(b33p, "the picture's notice", |message| {
                message.notice && message.text == "Peer changed the group's picture."
            })?;
            b33p_chat(b33p, &group, "with a picture", |chat| chat.avatar)?;
            // strings.xml `MessageRecord_s_changed_the_group_avatar`.
            Ok(android_containing(android, "changed the group avatar")?)
        },
    );

    // Android is b33p's account's phone, and the group's admin: b33p and Desktop show what it
    // changes about a member (b33p-core/src/signal.rs `group_notice`; Desktop's messages.json
    // `icu:GroupV2--member-privilege--promote--other--other`, `--demote--other--other`,
    // `icu:GroupV2--member-remove--other--other`). The member's place in b33p's details: an
    // admin or not, or gone.
    let members = [
        (
            "the member Android makes an admin is one on b33p and Desktop",
            MemberChange::MakeAdmin,
            "You made Member an admin.",
            "made Member an admin",
            Some(true),
        ),
        (
            "the admin rights Android takes from the member go on b33p and Desktop",
            MemberChange::RemoveAdmin,
            "You took Member's admin rights.",
            "revoked admin privileges from Member",
            Some(false),
        ),
        (
            "the member Android removes is gone on b33p and Desktop",
            MemberChange::Remove,
            "You removed Member.",
            "removed Member",
            None,
        ),
    ];
    let [made_admin, took_admin, removed] = &members;
    for (name, change, b33p_says, desktop_says, admin) in [made_admin, took_admin] {
        checks.record(name, || {
            member_change(android, b33p, by_desktop, *change, b33p_says, *admin)?;
            Ok(desktop_text(desktop, desktop_says)?)
        });
    }

    // What changes while b33p is stopped waits on the server, a message for each change; b33p
    // takes each change from its message, once it starts, in turn (presage `Group::apply`).
    checks.record(
        "two changes Android makes while b33p is stopped show on b33p once it starts",
        || {
            containers.stop(B33P)?;
            for change in [made_admin.1, took_admin.1] {
                android.call(drivers::Android::GroupMember {
                    group: by_desktop.to_owned(),
                    member: Account::Member.name().into_owned(),
                    change,
                })?;
            }
            let since = process::now()?;
            containers.start(B33P)?;
            *b33p = started_b33p(containers, &sockets.b33p, since)?;
            select(b33p, &group)?;
            // Each said once before.
            for notice in [made_admin.2, took_admin.2] {
                b33p.wait_for(
                    &format!("{notice:?} a second time in b33p"),
                    PATIENCE,
                    |snapshot| {
                        let said = snapshot
                            .messages
                            .iter()
                            .filter(|message| message.notice && message.text == notice)
                            .count();
                        (said >= 2).then_some(())
                    },
                )?;
            }
            Ok(member_change_shown(b33p, took_admin.2, took_admin.4)?)
        },
    );

    let (name, change, b33p_says, desktop_says, admin) = removed;
    checks.record(name, || {
        member_change(android, b33p, by_desktop, *change, b33p_says, *admin)?;
        Ok(desktop_text(desktop, desktop_says)?)
    });

    // The invite link, and a request to join by it: Desktop's account leaves and asks back in,
    // Android approves. One an admin removed may not rejoin by the link (Desktop's messages.json
    // `icu:GroupV2--join--link-forbidden`); one who left may.
    let linked = checks.report(
        "the invite link Android turns on, with approval, shows on b33p",
        || {
            android.call(drivers::Android::GroupLink {
                group: by_desktop.to_owned(),
                approval: true,
            })?;
            Ok(b33p_details(
                b33p,
                "You turned the invite link on, with admin approval.",
                "the invite link, and the peer among the members",
                |details| {
                    let link = details.invite_link.clone()?;
                    let peer = details
                        .members
                        .iter()
                        .find(|member| member.name == Account::Peer.name())?;
                    (details.invite_link_access == InviteLinkAccess::Approval)
                        .then(|| (link, peer.service_id.clone()))
                },
            )?)
        },
    );
    if let Some((link, peer)) = linked {
        checks.record("the peer leaving on Desktop shows on b33p", || {
            desktop.call(drivers::Desktop::LeaveGroup)?;
            Ok(b33p_shows(b33p, "the peer's leaving", |message| {
                message.notice && message.text == "Peer left the group."
            })?)
        });
        checks.record(
            "the peer's request to join by the link shows on b33p",
            || {
                desktop.call(drivers::Desktop::JoinGroup { link: link.clone() })?;
                Ok(b33p_details(
                    b33p,
                    "Peer asked to join the group.",
                    "the peer's request",
                    |details| details.requests.contains(&peer).then_some(()),
                )?)
            },
        );
        checks.record(
            "the request Android approves brings the peer back, on b33p and Desktop",
            || {
                android.call(drivers::Android::ApproveRequest {
                    group: by_desktop.to_owned(),
                    member: Account::Peer.name().into_owned(),
                })?;
                b33p_details(
                    b33p,
                    "You accepted Peer's request to join.",
                    "the peer among the members",
                    |details| {
                        details
                            .members
                            .iter()
                            .any(|member| member.service_id == peer)
                            .then_some(())
                    },
                )?;
                Ok(desktop_text(desktop, "approved your request to join")?)
            },
        );
    }

    // TESTPLAN.md §1.7 and §3.3, in the group: a mention from Desktop's account of b33p's, and a
    // story b33p posts to the group.
    let mentioned = "testbox groups: a mention";
    checks.record(
        "a mention Desktop sends reaches b33p as one of its account",
        || {
            desktop.call(drivers::Desktop::SendMention {
                member: "Android".to_owned(),
                text: mentioned.to_owned(),
            })?;
            Ok(b33p_shows(b33p, "the mention", |message| {
                !message.outgoing
                    && message.text.ends_with(mentioned)
                    && message.mentions.contains(android_aci)
            })?)
        },
    );
    let story = "testbox groups: a story";
    checks.record("a story b33p posts to the group shows on Desktop", || {
        b33p.act(Action::PostStoryText {
            group: group.clone(),
            text: story.to_owned(),
        })?;
        Ok(desktop.call(drivers::Desktop::StoryShows {
            from: by_desktop.to_owned(),
            text: story.to_owned(),
            timeout_ms: PATIENCE_MS,
        })?)
    });
    // The other way: Signal's apps send a story flagged as one (`?story=true`), which the
    // server delivers only to a device that asked for stories (`X-Signal-Receive-Stories`).
    let desktop_story = "testbox groups: a story from Desktop";
    checks.record("a story Desktop posts to the group shows on b33p", || {
        desktop.call(drivers::Desktop::PostStory {
            group: by_desktop.to_owned(),
            text: desktop_story.to_owned(),
        })?;
        Ok(story_shows(b33p, &group, desktop_story)?)
    });
    // Opening it had b33p send a view receipt (b33p-core stories.rs `send_viewed`, with read
    // receipts on, as Android's account has them), which Desktop counts on its own story.
    checks.record("b33p's view of Desktop's story counts on Desktop", || {
        // Desktop counts views only with read receipts on, which a linked Desktop's are not
        // until its primary says (presage-cli says nothing): on, unless they are.
        if let Err(error) = desktop.call(drivers::Desktop::SetReadReceipts {
            on: true,
            timeout_ms: PATIENCE_MS,
        }) && !error.to_string().contains("already")
        {
            return Err(error.into());
        }
        Ok(desktop.call(drivers::Desktop::WaitForStoryViews {
            views: 1,
            timeout_ms: PATIENCE_MS,
        })?)
    });
    // Replies and reactions to a group story go to the group, with the story as their context
    // (DataMessage `storyContext`): Signal's apps show them in the group's chat, a reply as a
    // message quoting the story, a reaction as "Reacted to your story". PLAN.md lists both
    // as b33p's next for stories; what b33p shows of them is checked here.
    let story_reply = "testbox groups: a reply to b33p's story";
    checks.record(
        "a reply Desktop gives b33p's story reaches b33p in the group",
        || {
            desktop.call(drivers::Desktop::ReplyToStory {
                from: by_desktop.to_owned(),
                text: story.to_owned(),
                reply: story_reply.to_owned(),
            })?;
            select(b33p, &group)?;
            Ok(b33p_shows(b33p, "the story's reply", |message| {
                !message.outgoing && message.text == story_reply
            })?)
        },
    );
    checks.record(
        "a reaction Desktop gives b33p's story shows in b33p",
        || {
            desktop.call(drivers::Desktop::ReactToStory {
                from: by_desktop.to_owned(),
                text: story.to_owned(),
            })?;
            select(b33p, &group)?;
            Ok(b33p_shows(b33p, "the story's reaction", |message| {
                !message.outgoing && message.text.to_lowercase().contains("story")
            })?)
        },
    );

    // A story from the phone: the account's own, which b33p hears of as a sync and files under
    // the group, as it does one of its own.
    let android_story = "testbox groups: a story from Android";
    checks.record(
        "a story Android posts to the group shows on b33p and Desktop",
        || {
            android.call(drivers::Android::PostStory {
                group: by_desktop.to_owned(),
                text: android_story.to_owned(),
            })?;
            story_shows(b33p, &group, android_story)?;
            Ok(desktop.call(drivers::Desktop::StoryShows {
                from: by_desktop.to_owned(),
                text: android_story.to_owned(),
                timeout_ms: PATIENCE_MS,
            })?)
        },
    );

    // b33p's own changes to the group. Android was left elsewhere (the request it approved), so
    // it opens the chat again, by the name it has once the change reached it.
    select(b33p, &group)?;
    let renamed = "testbox group, renamed";
    checks.record(
        "a name b33p gives the group shows on Desktop and Android",
        || {
            b33p.act(Action::RenameGroup {
                title: renamed.to_owned(),
            })?;
            b33p_chat(b33p, &group, "renamed", |chat| chat.title == renamed)?;
            desktop_text(desktop, renamed)?;
            android.call(drivers::Android::OpenConversation {
                text: renamed.to_owned(),
            })?;
            Ok(android_containing(android, renamed)?)
        },
    );
    // "set the disappearing message time(r) to 1 hour." (messages.json `icu:DisappearingTimer--set`,
    // strings.xml `MessageRecord_s_set_disappearing_message_time_to_s`).
    checks.record(
        "a timer b33p sets for the group shows on Desktop and Android",
        || {
            b33p.act(Action::SetTimer {
                conversation: group.clone(),
                seconds: 60 * 60,
            })?;
            b33p_chat(b33p, &group, "with a timer", |chat| {
                chat.expire_timer == 60 * 60
            })?;
            desktop_text(desktop, "to 1 hour")?;
            android.call(drivers::Android::OpenConversation {
                text: renamed.to_owned(),
            })?;
            Ok(android_containing(android, "to 1 hour")?)
        },
    );

    Ok(())
}

/// Android makes `change` to the member of the group Desktop named `group`, and b33p says so.
fn member_change(
    android: &mut Android,
    b33p: &mut Harness,
    group: &str,
    change: MemberChange,
    b33p_says: &str,
    admin: Option<bool>,
) -> Result<(), Failure> {
    android.call(drivers::Android::GroupMember {
        group: group.to_owned(),
        member: Account::Member.name().into_owned(),
        change,
    })?;
    Ok(member_change_shown(b33p, b33p_says, admin)?)
}

/// b33p says `notice`, and its details show the member an admin or not as `admin` says, or
/// gone.
fn member_change_shown(
    b33p: &mut Harness,
    notice: &str,
    admin: Option<bool>,
) -> Result<(), B33pError> {
    b33p_details(b33p, notice, "the member's place", |details| {
        let member = details
            .members
            .iter()
            .find(|member| member.name == Account::Member.name());
        (member.map(|member| member.admin) == admin).then_some(())
    })
}
