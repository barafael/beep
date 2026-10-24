//! The emoji picker's data (`Emojis` in `ui/emoji.slint`): every emoji Unicode has, by group,
//! found by name, and the ones picked lately, which are kept in the settings.

use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::Shared;
use crate::generated::{AppWindow, EmojiGroup, Emojis};

/// Results shown for a search at most: more don't fit on a screen anyway.
const FOUND: usize = 120;

/// Fills the picker and answers it: searches, and what was picked.
pub fn set_up(window: &AppWindow, shared: &Rc<Shared>) {
    let emojis = window.global::<Emojis>();
    emojis.set_groups(ModelRc::new(VecModel::from(groups())));
    emojis.set_recent(recent(&shared.settings.borrow().recent_emoji));
    emojis.on_search(|text| ModelRc::new(VecModel::from(search(&text))));
    emojis.on_used({
        let shared = Rc::downgrade(shared);
        let window = window.as_weak();
        move |emoji| {
            let (Some(shared), Some(window)) = (shared.upgrade(), window.upgrade()) else {
                return;
            };
            let mut settings = shared.settings.borrow().clone();
            settings.used_emoji(&emoji);
            if settings == *shared.settings.borrow() {
                return;
            }
            if let Err(error) = settings.save() {
                tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "failed to save settings");
            }
            window
                .global::<Emojis>()
                .set_recent(recent(&settings.recent_emoji));
            shared.settings.replace(settings);
        }
    });
}

fn recent(emoji: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        emoji
            .iter()
            .map(|e| SharedString::from(e.as_str()))
            .collect::<Vec<_>>(),
    ))
}

fn groups() -> Vec<EmojiGroup> {
    emojis::Group::iter()
        .map(|group| {
            let all: Vec<SharedString> = group.emojis().map(|e| e.as_str().into()).collect();
            EmojiGroup {
                name: group_name(group).into(),
                icon: all.first().cloned().unwrap_or_default(),
                emoji: ModelRc::new(VecModel::from(all)),
            }
        })
        .collect()
}

fn group_name(group: emojis::Group) -> &'static str {
    match group {
        emojis::Group::SmileysAndEmotion => "Smileys and emotion",
        emojis::Group::PeopleAndBody => "People and body",
        emojis::Group::AnimalsAndNature => "Animals and nature",
        emojis::Group::FoodAndDrink => "Food and drink",
        emojis::Group::TravelAndPlaces => "Travel and places",
        emojis::Group::Activities => "Activities",
        emojis::Group::Objects => "Objects",
        emojis::Group::Symbols => "Symbols",
        emojis::Group::Flags => "Flags",
    }
}

/// Emoji whose names or short codes contain every word of `text`, in Unicode's order.
pub(crate) fn search(text: &str) -> Vec<SharedString> {
    let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    emojis::iter()
        .filter(|emoji| {
            words.iter().all(|word| {
                emoji.name().contains(word.as_str())
                    || emoji.shortcodes().any(|code| code.contains(word.as_str()))
            })
        })
        .take(FOUND)
        .map(|emoji| emoji.as_str().into())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_are_found_by_name() {
        let found = search("thumbs up");
        assert_eq!(found.first().map(SharedString::as_str), Some("👍"));
        assert!(search("Red Heart").iter().any(|e| e == "❤️"));
        assert!(search("   ").is_empty());
        assert!(search("no such emoji at all").is_empty());
    }

    #[test]
    fn every_group_has_emoji() {
        let groups = groups();
        assert_eq!(groups.len(), 9);
        for group in groups {
            assert!(slint::Model::row_count(&group.emoji) > 0, "{}", group.name);
            assert!(!group.icon.is_empty());
        }
    }
}
