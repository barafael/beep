//! Stories to My Story, between b33p (linked to Android), Android, the peer (with Desktop) and a
//! presage-cli member: who My Story reaches is the account's Signal connections, as the phone
//! keeps it in the storage service (a `StoryDistributionListRecord`).
//!
//! b33p's story to My Story goes as Signal's apps send it: with its sender key for the list,
//! sealed for every recipient's devices in one request (`PUT /v1/messages/multi_recipient
//! ?story=true`, presage `send_story_to_list`), which b33p's log says. The phone files it under
//! My Story from b33p's sync, and counts Desktop's view of it. Android's own story to My Story shows on b33p under its "My Story",
//! the peer's under the peer. Once Android leaves the peer out of My Story, b33p reads that from
//! the storage service, and its next story reaches the member and not the peer.

use std::time::{Duration, Instant};

use b33p_harness::Action;

use super::b33p::{accept_request, chat_previewing, story_shows};
use super::presage::{Account, Presage, PresageError};
use super::setup::AndroidBox;
use super::{Checks, Failure, PATIENCE, PATIENCE_MS};
use crate::harness::{Harness, HarnessError};
use crate::pod::{B33P, Pod};
use crate::process;
use testbox_tools::drivers;

/// My Story's conversation in b33p: `s:` and its id, all zeros (b33p-core
/// `ConversationId::story_list`).
const MY_STORY: &str = "s:00000000000000000000000000000000";

/// What b33p's log says of a story to a list sent in one request (presage
/// `send_story_to_list`, logged at info, which the box's b33p logs).
const ONE_REQUEST: &str = "sent a story to a story list in one request, with its sender key";

/// How long a story that shouldn't reach someone is given to show anyway, once it reached the
/// others: the member had it, so the server had it for everyone.
const NOT_SHOWN: Duration = Duration::from_secs(30);

/// What the stories checks find wrong of their own.
#[derive(Debug, thiserror::Error)]
enum StoriesError {
    /// presage-cli printed no story with the text in its syncs while it was waited for.
    #[error("{account} received no story {text:?} in {seconds} s; it printed:\n{printed}")]
    NotReceived {
        account: Account,
        text: String,
        seconds: u64,
        printed: String,
    },
    /// A story reached someone b33p's My Story leaves out.
    #[error("the story {text:?} showed on {client}, which My Story leaves out")]
    ShownAnyway { text: String, client: &'static str },
    /// A conversation id that isn't a 1:1 chat's (`c:` and a service id).
    #[error("{id} isn't a 1:1 chat's id")]
    NotAContact { id: String },
}

pub(super) fn stories(
    checks: &mut Checks,
    fixture: &mut AndroidBox<'_>,
    containers: &Pod,
) -> Result<(), Failure> {
    let AndroidBox {
        peer,
        android,
        b33p,
        desktop,
        android_aci,
        conversation,
    } = fixture;
    let peer_aci = conversation
        .strip_prefix("c:")
        .ok_or_else(|| StoriesError::NotAContact {
            id: conversation.clone(),
        })?
        .to_owned();
    let member = Presage::new(containers, Account::Member);
    // The member is a connection of b33p's account: after `groups` already (registered, its
    // request accepted), else made one here.
    let member_aci = checks.require("the member is a connection of b33p's account", || {
        let aci = match member.aci() {
            Ok(aci) => aci,
            Err(_) => member.register()?,
        };
        let chat = format!("c:{aci}");
        let accepted = b33p
            .snapshot()?
            .conversations
            .iter()
            .any(|c| c.id == chat && !c.request);
        if !accepted {
            let hello = "testbox stories: hello from the member";
            member.send(android_aci, hello)?;
            let chat = chat_previewing(b33p, hello)?;
            accept_request(b33p, &chat)?;
        }
        Ok(aci)
    })?;
    checks.require(
        "b33p's My Story goes to both connections, the peer and the member",
        || Ok(my_story_reaches(b33p, &[&peer_aci, &member_aci], &[])?),
    )?;

    // b33p's story to My Story, to both, in one request.
    let to_both = "testbox stories: b33p's to My Story";
    checks.record(
        "a story b33p posts to My Story reaches the peer's Desktop and the member, in one request",
        || {
            let since = process::now()?;
            b33p.act(Action::PostStoryText {
                group: MY_STORY.to_owned(),
                text: to_both.to_owned(),
            })?;
            containers.await_log(B33P, ONE_REQUEST, Some(since), PATIENCE)?;
            desktop.call(drivers::Desktop::StoryShows {
                from: "Android".to_owned(),
                text: to_both.to_owned(),
                timeout_ms: PATIENCE_MS,
            })?;
            received(&member, to_both, PATIENCE)
        },
    );
    checks.record("b33p files its story under My Story", || {
        Ok(story_shows(b33p, MY_STORY, to_both)?)
    });
    // b33p's sync names the list (`StoryMessageRecipient.distribution_list_ids`): the phone
    // files the story under its own My Story, and counts the view Desktop's opening it sent to
    // the account. (It's the only story under My Story by then: those before were groups'. Its
    // text isn't there to read: Android's viewer draws a text story without a text node.)
    checks.record(
        "the phone files b33p's story under its My Story, counting Desktop's view",
        || {
            Ok(android.call(drivers::Android::OwnStoryViews {
                views: 1,
                timeout_ms: PATIENCE_MS,
            })?)
        },
    );

    // The phone's own story to My Story: b33p hears of it by a sync naming the list.
    let from_android = "testbox stories: Android's to My Story";
    checks.record(
        "a story Android posts to My Story shows on b33p under My Story, and on Desktop",
        || {
            android.call(drivers::Android::PostStory {
                group: "My Story".to_owned(),
                text: from_android.to_owned(),
            })?;
            story_shows(b33p, MY_STORY, from_android)?;
            Ok(desktop.call(drivers::Desktop::StoryShows {
                from: "Android".to_owned(),
                text: from_android.to_owned(),
                timeout_ms: PATIENCE_MS,
            })?)
        },
    );

    // A connection's own My Story: no group, filed under them.
    let from_peer = "testbox stories: the peer's to My Story";
    checks.record(
        "a story the peer posts to My Story on Desktop shows on b33p under the peer",
        || {
            desktop.call(drivers::Desktop::PostStory {
                group: "My Story".to_owned(),
                text: from_peer.to_owned(),
            })?;
            Ok(story_shows(b33p, conversation, from_peer)?)
        },
    );

    // The phone leaves the peer out of My Story; b33p reads it from the storage service.
    let left_out = checks.report(
        "the phone leaving the peer out of My Story reaches b33p",
        || {
            // By the name Android shows: the peer's own, or a nickname given on the phone
            // (`storage`), which b33p took from the storage service too.
            let shown = b33p
                .snapshot()?
                .conversations
                .iter()
                .find(|c| &c.id == conversation)
                .map_or_else(|| peer.name().into_owned(), |c| c.title.clone());
            android.call(drivers::Android::MyStoryPrivacy {
                except: vec![shown],
            })?;
            Ok(my_story_reaches(b33p, &[&member_aci], &[&peer_aci])?)
        },
    );
    if left_out.is_some() {
        let to_member = "testbox stories: b33p's to My Story, the peer left out";
        checks.record(
            "b33p's next story to My Story reaches the member, and not the peer",
            || {
                b33p.act(Action::PostStoryText {
                    group: MY_STORY.to_owned(),
                    text: to_member.to_owned(),
                })?;
                received(&member, to_member, PATIENCE)?;
                let shown = desktop.call(drivers::Desktop::StoryShows {
                    from: "Android".to_owned(),
                    text: to_member.to_owned(),
                    timeout_ms: NOT_SHOWN.as_millis() as u64,
                });
                if shown.is_ok() {
                    return Err(StoriesError::ShownAnyway {
                        text: to_member.to_owned(),
                        client: "the peer's Desktop",
                    }
                    .into());
                }
                Ok(())
            },
        );
    }
    // My Story to everyone again, for what runs after on this box.
    checks.record(
        "the phone's My Story goes to everyone again, b33p's too",
        || {
            android.call(drivers::Android::MyStoryPrivacy { except: Vec::new() })?;
            Ok(my_story_reaches(b33p, &[&peer_aci, &member_aci], &[])?)
        },
    );
    Ok(())
}

/// Waits until b33p's My Story goes to each of `to` and to none of `not`.
fn my_story_reaches(b33p: &mut Harness, to: &[&str], not: &[&str]) -> Result<(), HarnessError> {
    b33p.wait_for("b33p's My Story's recipients", PATIENCE, |snapshot| {
        let my_story = snapshot.story_lists.iter().find(|l| l.id == MY_STORY)?;
        let has = |aci: &&str| my_story.recipients.iter().any(|r| r == aci);
        (to.iter().all(has) && !not.iter().any(has)).then_some(())
    })
}

/// Syncs `account`'s presage-cli asking for stories until it printed one with `text`.
fn received(account: &Presage<'_>, text: &str, patience: Duration) -> Result<(), Failure> {
    let start = Instant::now();
    let mut printed = String::new();
    loop {
        let output: Result<String, PresageError> = account.sync_stories();
        printed.push_str(&output?);
        if printed
            .lines()
            .any(|l| l.contains("new story") && l.contains(text))
        {
            return Ok(());
        }
        if start.elapsed() > patience {
            return Err(StoriesError::NotReceived {
                account: account.account(),
                text: text.to_owned(),
                seconds: patience.as_secs(),
                printed,
            }
            .into());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
