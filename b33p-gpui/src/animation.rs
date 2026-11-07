//! What moves in the window, and for how long: the durations of `Motion` in
//! `b33p-slint/ui/theme.slint`, and the few shapes of motion the window uses. Every one goes
//! through GPUI's animation element, which asks [`gpui_kit::App::reduce_motion`] (set from the
//! "Animations" setting in `window::apply_motion`) and with it set draws the end state in the
//! first frame, with no animation frame asked for. Without it, a running animation asks for a
//! frame at a time until it is done, and then nothing: no motion here keeps the window drawing
//! while it rests.

use std::time::Duration;

use gpui_kit::{Animation, AnimationExt as _, ElementId, IntoElement, Styled};

/// Panels, dialogs and screens coming and going; bars sliding open.
pub const NORMAL: Duration = Duration::from_millis(160);
/// Pictures fading in over their placeholder; a message arriving.
pub const SLOW: Duration = Duration::from_millis(240);
/// Hover and press feedback, small things appearing.
pub const FAST: Duration = Duration::from_millis(120);

/// How the animations ease: quickly at first, then settling, as Slint's `ease-out`.
pub fn ease_out(delta: f32) -> f32 {
    1. - (1. - delta).powi(3)
}

/// Whether messages that come `now` are not part of a stream, after those that came `before`:
/// only these fade in. A fade draws a frame at a time, and a stream (the benchmark's five a
/// second) of them would have the window drawing all the time.
pub fn is_calm(before: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    before.is_none_or(|before| now.duration_since(before) >= Duration::from_secs(1))
}

/// `element`, fading in from nothing as it first appears. `id` names it among the elements of
/// the frame: an element with the same id in the frame before is not new, and doesn't fade.
pub fn fade_in<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    duration: Duration,
    element: E,
) -> impl IntoElement {
    element.with_animation(
        id,
        Animation::new(duration).with_easing(ease_out),
        |element, delta| element.opacity(delta),
    )
}

#[cfg(test)]
mod tests {
    use b33p_core::settings::Animations;
    use b33p_core::state::Update;
    use b33p_core::{ChatMessage, ConversationId, Event};
    use gpui::TestAppContext;

    use super::{NORMAL, ease_out, is_calm};
    use crate::testing::{self, Ui};

    fn chat_with_a_message() -> b33p_core::state::ChatState {
        let mut chat = testing::chats(&["Ann"]);
        let ann = ConversationId::contact("ann");
        chat.select(&ann);
        chat.apply(Event::History {
            conversation: ann,
            messages: vec![ChatMessage {
                conversation: ConversationId::contact("ann"),
                sender_id: "ann".into(),
                sender: "Ann".into(),
                body: "hello".into(),
                timestamp: 1_700_000_000_000,
                ..ChatMessage::default()
            }],
            reactions: Vec::new(),
            older: None,
        });
        chat
    }

    fn animations(ui: &mut Ui, animations: Animations) {
        let shared = ui.booted.shared.clone();
        let mut settings = shared.settings.borrow().clone();
        settings.animations = animations;
        ui.cx.update(|_, cx| shared.apply_settings(settings, cx));
        ui.settle();
    }

    fn banner_height(ui: &mut Ui) -> f32 {
        ui.bounds("Error banner")
            .map_or(0., |bounds| bounds.size.height.into())
    }

    fn fail(ui: &mut Ui) {
        ui.act(|chat| {
            chat.apply(Event::Error(testing::error(
                b33p_core::ErrorKind::Unreachable,
                "Couldn't reach Signal.",
            )));
            Update::default()
        });
    }

    /// The error banner slides open and closed, and with motion off it is just there or not.
    #[gpui::test]
    async fn the_error_banner_slides(tc: &mut TestAppContext) {
        let mut ui = Ui::new(tc, chat_with_a_message());
        animations(&mut ui, Animations::On);
        fail(&mut ui);
        let at_first = banner_height(&mut ui);
        ui.wait(NORMAL / 3);
        let on_its_way = banner_height(&mut ui);
        ui.wait(NORMAL);
        let open = banner_height(&mut ui);
        assert!(open >= 36., "it opens to its height: {open}");
        assert!(
            at_first < on_its_way && on_its_way < open,
            "it grows: {at_first}, {on_its_way}, {open}"
        );

        ui.act(|chat| {
            chat.dismiss_error();
            Update::default()
        });
        ui.wait(NORMAL / 3);
        let closing = banner_height(&mut ui);
        assert!(
            0. < closing && closing < open,
            "it is still going: {closing}"
        );
        ui.wait(NORMAL);
        assert_eq!(banner_height(&mut ui), 0., "and gone");
    }

    /// What comes in (a dialog, a message) fades in, over `NORMAL` or a little more, unless
    /// motion is off.
    #[gpui::test]
    async fn dialogs_and_new_messages_fade_in_unless_motion_is_off(tc: &mut TestAppContext) {
        let failed = || {
            let mut chat = b33p_core::state::ChatState::default();
            chat.apply(Event::Failed {
                failure: b33p_core::Failure::KeyMissing,
                message: "The key is missing.".into(),
            });
            chat
        };
        for (setting, moves) in [(Animations::On, true), (Animations::Off, false)] {
            // A dialog.
            let mut ui = Ui::new(tc, failed());
            animations(&mut ui, setting);
            ui.click("Start over…");
            let at_first = ui.backgrounds();
            ui.wait(NORMAL * 2);
            assert_eq!(
                at_first != ui.backgrounds(),
                moves,
                "the dialog, with {setting:?}"
            );

            // A message that comes in.
            let mut ui = Ui::new(tc, chat_with_a_message());
            animations(&mut ui, setting);
            ui.act(|chat| {
                chat.apply(Event::Message(ChatMessage {
                    conversation: ConversationId::contact("ann"),
                    sender_id: "ann".into(),
                    sender: "Ann".into(),
                    body: "and another".into(),
                    timestamp: 1_700_000_001_000,
                    ..ChatMessage::default()
                }))
            });
            let at_first = ui.backgrounds();
            ui.wait(super::SLOW * 2);
            assert_eq!(
                at_first != ui.backgrounds(),
                moves,
                "the message, with {setting:?}"
            );
        }
    }

    #[gpui::test]
    async fn the_error_banner_does_not_slide_with_motion_off(tc: &mut TestAppContext) {
        let mut ui = Ui::new(tc, chat_with_a_message());
        animations(&mut ui, Animations::Off);
        fail(&mut ui);
        assert!(banner_height(&mut ui) >= 36., "open at once");
        ui.act(|chat| {
            chat.dismiss_error();
            Update::default()
        });
        assert_eq!(banner_height(&mut ui), 0., "gone at once");
    }

    #[test]
    fn a_stream_of_messages_is_not_calm() {
        let now = std::time::Instant::now();
        let later = |millis| now + std::time::Duration::from_millis(millis);
        assert!(is_calm(None, now), "the first one");
        assert!(!is_calm(Some(now), later(200)), "five a second");
        assert!(is_calm(Some(now), later(1500)), "after a pause");
    }

    #[test]
    fn easing_runs_from_nothing_to_all_without_overshooting() {
        assert_eq!(ease_out(0.), 0.);
        assert_eq!(ease_out(1.), 1.);
        let mut last = 0.;
        for step in 1..=20 {
            let now = ease_out(step as f32 / 20.);
            assert!(now > last && now <= 1.);
            last = now;
        }
        // Quick at first: more than half done by the middle.
        assert!(ease_out(0.5) > 0.5);
    }
}
