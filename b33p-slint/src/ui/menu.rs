//! Our own menus' keyboard: which line the arrow keys go to (see `menu.slint`).

use super::*;

/// The next line from `from` (-1 for none yet) in steps of `by` (1 or -1) that `choosable`
/// allows, round the end; -1 if none does.
fn step(choosable: &[bool], from: i32, by: i32) -> i32 {
    let n = choosable.len() as i32;
    if n == 0 {
        return -1;
    }
    // From none yet, down lands on the first line and up on the last.
    let start = if from < 0 {
        if by > 0 { n - 1 } else { 0 }
    } else {
        from
    };
    (1..=n)
        .map(|k| (start + k * by).rem_euclid(n))
        .find(|&i| choosable[i as usize])
        .unwrap_or(-1)
}

pub(crate) fn wire_menus(window: &AppWindow) {
    window.global::<MenuNav>().on_step(|choices, from, by| {
        let choosable: Vec<bool> = choices
            .iter()
            .map(|c| !c.hidden && !c.separator && !c.disabled)
            .collect();
        step(&choosable, from, by)
    });
}

#[cfg(test)]
mod tests {
    use super::step;

    #[test]
    fn down_from_none_is_the_first_choosable_line() {
        assert_eq!(step(&[false, true, true], -1, 1), 1);
    }

    #[test]
    fn up_from_none_is_the_last_choosable_line() {
        assert_eq!(step(&[true, true, false], -1, -1), 1);
    }

    #[test]
    fn steps_skip_what_cant_be_chosen_and_go_round() {
        assert_eq!(step(&[true, false, true], 0, 1), 2);
        assert_eq!(step(&[true, false, true], 2, 1), 0);
        assert_eq!(step(&[true, false, true], 0, -1), 2);
    }

    #[test]
    fn a_menu_with_nothing_to_choose_has_no_line() {
        assert_eq!(step(&[false, false], -1, 1), -1);
        assert_eq!(step(&[], -1, 1), -1);
    }

    #[test]
    fn the_only_choosable_line_stays() {
        assert_eq!(step(&[false, true, false], 1, 1), 1);
    }
}
