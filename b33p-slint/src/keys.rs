//! Ctrl shortcuts while an input method composes (Windows). With a Korean IME on, Windows
//! reports Ctrl+C, Ctrl+V, Ctrl+A and the rest as the key `Process`, and so does winit, both for
//! the key and for the key without modifiers; Slint finds no text in it and drops the press.
//! Copy, paste and b33p's own shortcuts then do nothing until the IME is switched off. The
//! physical key still says which letter it was, so the press is passed on as that letter.
//!
//! Ported from Othereum/sigwin e3055df, through Slint's public winit hook, so no fork patch.
//! The Caps Lock half of that change isn't needed: Slint compares shortcuts in lower case.

use slint::ComponentHandle;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use slint::winit_030::{EventResult, WinitWindowAccessor};

use crate::generated::AppWindow;

/// Passes Ctrl+letter on to Slint as that letter while the IME holds the key.
pub fn fix_ctrl_shortcuts(window: &AppWindow) {
    let modifiers = std::cell::Cell::new(ModifiersState::empty());
    window
        .window()
        .on_winit_window_event(move |window, event| match event {
            WindowEvent::ModifiersChanged(changed) => {
                modifiers.set(changed.state());
                EventResult::Propagate
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state.is_pressed() => {
                let Some(letter) =
                    ctrl_letter(&event.logical_key, event.physical_key, modifiers.get())
                else {
                    return EventResult::Propagate;
                };
                let text = letter.to_string().into();
                let press = if event.repeat {
                    slint::platform::WindowEvent::KeyPressRepeated { text }
                } else {
                    slint::platform::WindowEvent::KeyPressed { text }
                };
                if let Err(error) = window.dispatch_event_with_result(press) {
                    tracing::debug!(%error, "a Ctrl shortcut during IME composition was lost");
                }
                EventResult::PreventDefault
            }
            _ => EventResult::Propagate,
        });
}

/// The letter of a Ctrl+letter press the IME turned into `Process`; `None` for any other key,
/// and with Alt (AltGr on many layouts), which types characters rather than shortcuts.
fn ctrl_letter(logical: &Key, physical: PhysicalKey, modifiers: ModifiersState) -> Option<char> {
    if !modifiers.control_key() || modifiers.alt_key() {
        return None;
    }
    if *logical != Key::Named(NamedKey::Process) {
        return None;
    }
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    letter(code)
}

fn letter(code: KeyCode) -> Option<char> {
    use KeyCode::*;
    const LETTERS: [KeyCode; 26] = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    let index = LETTERS.iter().position(|&key| key == code)?;
    Some(char::from(b'a' + index as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROCESS: Key = Key::Named(NamedKey::Process);

    #[test]
    fn a_ctrl_letter_held_by_the_ime_is_that_letter() {
        let ctrl = ModifiersState::CONTROL;
        let c = PhysicalKey::Code(KeyCode::KeyC);
        assert_eq!(ctrl_letter(&PROCESS, c, ctrl), Some('c'));
        assert_eq!(
            ctrl_letter(
                &PROCESS,
                PhysicalKey::Code(KeyCode::KeyZ),
                ctrl | ModifiersState::SHIFT
            ),
            Some('z'),
            "Ctrl+Shift+Z, redo"
        );
    }

    #[test]
    fn other_presses_are_left_alone() {
        let ctrl = ModifiersState::CONTROL;
        let c = PhysicalKey::Code(KeyCode::KeyC);
        assert_eq!(
            ctrl_letter(&PROCESS, c, ModifiersState::empty()),
            None,
            "typing"
        );
        assert_eq!(
            ctrl_letter(&PROCESS, c, ctrl | ModifiersState::ALT),
            None,
            "AltGr"
        );
        assert_eq!(
            ctrl_letter(&Key::Character("c".into()), c, ctrl),
            None,
            "no IME"
        );
        assert_eq!(
            ctrl_letter(&PROCESS, PhysicalKey::Code(KeyCode::Digit1), ctrl),
            None
        );
    }
}
