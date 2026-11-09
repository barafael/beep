//! A large group: b33p's account, the peer (with Desktop) and twenty presage-cli members more,
//! made by b33p. A message of b33p's to it goes with its sender key, sealed for every member's
//! devices in one request (`PUT /v1/messages/multi_recipient`), after the key's distribution to
//! each (libsignal-service `send_with_sender_key`); the wire record shows it with `--capture`.
//! Every member answers at once.

use b33p_harness::Action;

use super::b33p::{
    accept_request, b33p_details, b33p_shows, chat_previewing, select, type_and_send,
};
use super::clients::{android_containing, android_shows, desktop_shows};
use super::presage::{Account, Presage};
use super::setup::{AndroidBox, each};
use super::{Checks, Failure, PATIENCE};
use crate::harness::HarnessError;
use crate::pod::Pod;
use testbox_tools::{Report, drivers};

/// What the large group's checks find wrong of their own.
#[derive(Debug, thiserror::Error)]
enum LargeGroupError {
    /// The conversation b33p made isn't a group's (`g:` and the master key).
    #[error("{id} isn't a group's id")]
    NotAGroup { id: String },
}

/// How many presage-cli members join the peer.
const MEMBERS: u8 = 20;

pub(super) fn large_group(
    checks: &mut Checks,
    fixture: &mut AndroidBox<'_>,
    containers: &Pod,
) -> Result<(), Failure> {
    let AndroidBox {
        android,
        b33p,
        desktop,
        android_aci,
        conversation,
        ..
    } = fixture;
    let crowd: Vec<Presage<'_>> = (0..MEMBERS)
        .map(|n| Presage::new(containers, Account::Crowd(n)))
        .collect();
    // Twenty presage-cli processes at once: one has crashed in its `whoami` ("double free or
    // corruption", in presage-cli, not the server), so a registration gets a second go.
    checks.require(&format!("{MEMBERS} presage-cli members register"), || {
        each(&crowd, |member| {
            member.register().or_else(|error| {
                eprintln!("  {}: registering again: {}", member.name(), Report(&error));
                member.register()
            })
        })?;
        Ok(())
    })?;
    // Each writes to b33p's account first: that brings b33p their profile keys, which it adds
    // them to a group with outright (`groups`); b33p accepts each.
    let hello = |member: &Presage<'_>| format!("testbox large group: hello from {}", member.name());
    checks.require("each member writes to b33p's account", || {
        each(&crowd, |member| member.send(android_aci, &hello(member)))
    })?;
    let chats = checks.require(
        &format!("the {MEMBERS} message requests are accepted in b33p"),
        || {
            Ok(crowd
                .iter()
                .map(|member| {
                    let chat = chat_previewing(b33p, &hello(member))?;
                    accept_request(b33p, &chat)?;
                    Ok(chat)
                })
                .collect::<Result<Vec<String>, HarnessError>>()?)
        },
    )?;

    let title = "testbox large group";
    let group = checks.require(
        &format!("b33p makes a group of the peer and the {MEMBERS} members"),
        || {
            let mut members = vec![conversation.clone()];
            members.extend(chats);
            b33p.act(Action::CreateGroup {
                title: title.to_owned(),
                members,
            })?;
            Ok(
                b33p.wait_for("the large group in b33p", PATIENCE, |snapshot| {
                    snapshot
                        .conversations
                        .iter()
                        .find(|chat| chat.group && chat.title == title && !chat.request)
                        .map(|chat| chat.id.clone())
                })?,
            )
        },
    )?;
    let everyone = usize::from(MEMBERS) + 2;
    checks.record(
        &format!("b33p's details of the group list all {everyone} members"),
        || {
            select(b33p, &group)?;
            Ok(b33p_details(
                b33p,
                "You created the group.",
                "every member",
                |details| (details.members.len() == everyone).then_some(()),
            )?)
        },
    );

    let greeting = "testbox large group: hello, everyone";
    checks.record(
        "text b33p sends to the large group reaches Desktop, Android and every member",
        || {
            select(b33p, &group)?;
            type_and_send(b33p, greeting)?;
            desktop.call(drivers::Desktop::OpenTitled {
                title: title.to_owned(),
            })?;
            desktop_shows(desktop, greeting)?;
            android.call(drivers::Android::OpenConversation {
                text: title.to_owned(),
            })?;
            android_shows(android, greeting)?;
            each(&crowd, |member| member.sync_printing(greeting))?;
            Ok(())
        },
    );
    // The members as Android names them under the group's name: three, "and N others";
    // Desktop's header doesn't say them.
    checks.record(
        &format!("Android names the group's {everyone} members"),
        || {
            Ok(android_containing(
                android,
                &format!("and {} others", everyone - 3),
            )?)
        },
    );

    // presage-cli knows the group from b33p's greeting; each member writes at once.
    let master_key = b33p_core::ConversationId(group.clone())
        .group_key()
        .ok_or_else(|| LargeGroupError::NotAGroup { id: group.clone() })?
        .to_owned();
    let answer = |member: &Presage<'_>| format!("testbox large group: {} answers", member.name());
    checks.record(
        &format!("texts from all {MEMBERS} members at once reach b33p, Desktop and Android"),
        || {
            each(&crowd, |member| {
                member.send_to_group(&master_key, &answer(member))
            })?;
            for member in &crowd {
                let text = answer(member);
                b33p_shows(b33p, &format!("{text:?}"), |message| {
                    !message.outgoing && message.text == text
                })?;
                desktop_shows(desktop, &text)?;
            }
            // Whichever came last is on Android's screen.
            Ok(android_containing(android, "testbox large group: ")?)
        },
    );
    Ok(())
}
