# Upstream PR: skip animations from a value to itself

Status: opened as a draft, https://github.com/slint-ui/slint/pull/14823 (branch pushed to the
`barafael/slint` fork). Branch `no-op-animation` (commit `d4a708214`) in
`/home/rafael/code/beep-gpui-patch/slint`, based on upstream `slint-ui/slint` master `8d51770e7`
(2026-11-07). b33p has it through the fork's `langis/integration`
(merge `84f66189a`); the patch file `0001-no-op-animation.patch` that carried it is deleted.

## How the draft PR was opened

The branch was pushed to the fork and the PR opened as a draft (per the owner's rule), with
these commands:

```sh
cd /home/rafael/code/beep-gpui-patch/slint
git remote add fork https://github.com/barafael/slint   # once
git push fork no-op-animation

gh pr create --draft --repo slint-ui/slint --base master --head barafael:no-op-animation \
  --title "core: Don't animate an animated binding from a value to itself" \
  --body-file /home/rafael/code/beep-gpui-patch/pr-body.md
```

`pr-body.md` is the text from "PR description" up to the "Notes for the owner" heading, saved to
that path (the title line is repeated in `--title`).
Slint squash-merges with the PR description as the commit message, so it follows
`scripts/commit-template.txt`, and the `ChangeLog:` trailer replaces a `CHANGELOG.md` edit
(`docs/internal/writing-style-guide.md` says not to edit it in a PR).

## PR description

core: Don't animate an animated binding from a value to itself

The first time the dependencies of an animated binding change, the binding has no previous target
value, so it always starts an animation from its current value to the new one.
When the value didn't change, for example because a model row was rewritten after its item was
created, that is an animation from a value to itself.
It shows nothing, but runs for its whole duration and keeps the window redrawing at the frame
rate of the display.

Skip the animation when the binding has never animated and its new value equals the current one.
Later changes already compare the new value with the previous target and don't restart the
animation when it is unchanged.

### Motivation

Found in a chat client (b33p) with a message list: each row has animated properties (background
colors, opacity, and an arrival and a flash value) bound to the model row.
A row gets its data after it is made, and is rewritten when a neighbor changes, so every new
row started an animation to its own value, and every incoming message kept the whole window
repainting for the animation's duration.

With the Qt backend, receiving 5 messages a second:

| | CPU |
|---|---|
| master | 23% |
| with this change | 7.6% |

(On master this was about 1420 wakeups a second.)

(Idle with a blinking text cursor is a separate cost, `TextCursorBlinker`; this PR doesn't touch it.)

### Reproduction

Properties with `animate`, whose binding depends on a value that changes without changing the
result.
A `Timer` stands in for the model rewrite.

```slint
export component App inherits Window {
    in-out property <bool> on: false;
    in-out property <int> ticks: 0;

    // Depends on `ticks`, but its value only changes with `on`.
    out property <length> offset: ticks >= 0 && on ? 200px : 40px;
    animate offset { duration: 1s; }

    Timer {
        interval: 100ms;
        triggered => { root.ticks += 1; }
    }
}
```

Driven with the testing backend and mocked time: 125 frames of 16 ms, reading `offset` each
frame and asking `window().has_active_animations()`, which the winit, Qt, and linuxkms backends
use to request the next frame.
`offset` stays at 40px all the time.

| | frames with an active animation |
|---|---|
| master | 63 of 125 (the 1 s animation, run once) |
| with this change | 0 of 125 |

The animation runs once per binding, at the first change of its dependencies, so a list with a
new item per message starts one per item.
The test case `tests/cases/properties/animation_unchanged_value.slint` checks the same thing, and
fails on master with `tick 1 started an animation`.

### Behavior change

- Only an animated binding that has never animated, and whose first change of dependencies gives
  the value it already has, is affected.
  It stays idle instead of animating from the value to itself.
  The value it shows is unchanged.
- Everything else behaves as before: a first change to a different value, a later change, a change
  during an animation (retargeting, spring velocity carry-over), `delay`, `iteration-count`,
  `direction`, and `enabled`.
- Floats compare exactly (`PartialEq`), with no tolerance: a value that differs by any amount still
  animates.
  Colors, brushes, and structs compare field by field, so a brush with the same stops is a no-op.
- `has_active_animations()` is false in the cases above, so backends go back to sleep.

### Testing

- New unit test `properties_test_animation_triggered_by_binding_without_value_change` in
  `internal/core/properties/properties_animations.rs`: three rewrites of an unrelated field keep
  the value and the driver idle, then a real change animates from the value it had.
- New case `tests/cases/properties/animation_unchanged_value.slint`, with a Rust block that uses a
  `Timer` and `Window::has_active_animations()`.
- Both fail without the change.
- `cargo test -p i-slint-core`: 457 + 73 + others passed.
- `test-driver-rust` and `test-driver-interpreter` with `SLINT_TEST_FILTER` set to `animat`,
  `transition`, `states`, `flickable`, `spring`, `two_way`, `issue_4811`, `issue_345`, and
  `issue_10448`: all pass.

ChangeLog: Fixed an animated property keeping the window redrawing when its dependencies changed without changing its value

## Notes for the owner (not part of the PR)

- The numbers in the table (23%, about 1420 wakeups/s on master, 7.6%) are from the b33p
  measurements, with the Qt backend; the repro table is from this tree
  (`/home/rafael/code/beep-gpui-patch/repro`, `cargo run` there with
  `CARGO_TARGET_DIR=/home/rafael/code/beep-gpui-patch/target`). The wakeups with the patch were
  not recorded, so the PR text gives only the CPU for it.
- The b33p row properties named in the motivation were checked against `b33p-slint/ui/bubble.slint`
  (animated `background`, `opacity`, `arrived`, `flash`).
- b33p pins `84f66189a` of the fork, which merges this branch into `langis/integration`.
  (Until then it pinned `f0c244a22` and applied the patch file `0001-no-op-animation.patch`.)
- The ported change differs from `0001-no-op-animation.patch` in one detail: the new value is not
  stored as the target when the animation is skipped, so the binding stays "never animated".
  The fork's version passes the same tests (the leftover state is masked by the default
  zero-duration animation), but this is cleaner.
- Upstream issue search found nothing equivalent; #5426 and #9828 are related animation topics only.
