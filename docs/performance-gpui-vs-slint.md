# Making the two b33p windows faster: the GPUI and Slint story

This is the history of a performance pass over `b33p-slint` (Slint on Qt's
backend, CPU rendering) and `b33p-gpui` (GPUI through `gpui-kit`, GPU rendering
through Vulkan). Both windows are views over the same `b33p-core` `ChatState`,
so anything that is not the window was shared and fixed once.

The numbers come from `testbox/gpui-view/bench.py` (see its header and
`b33p-gpui/README.md`, "Measuring it against the Slint client"). Raw reports:
`target/bench-final/report.md` and `target/bench-final-patched/report.md` in the
`gpui` worktree (not committed). Medians of 3 interleaved rounds. The machine was
shared and its load average was 5 to 7 at the start of the final run, so
differences of a few percent are noise.

## 1. How it was measured

- Each client runs on the mock backend in a display of its own: `cage` plus
  `wayvnc` in a container, with `WITH_DISPLAY_RENDERER=gles2` and the host's
  render node, so GPUI draws on the real GPU (RADV, AMD Radeon 8060S) and not on
  llvmpipe. The first attempt silently fell back to llvmpipe because cage used
  pixman; that is why the renderer is now explicit.
- Input is real: wheel and key events through the display's VNC `interact`
  socket. A Shift press there creates a keyboard, which makes the window
  "active" (on wayland GPUI's active means `wl_keyboard.enter`). Without it focus
  and blur events never arrive and the caret blinks forever.
- The mock scales with `B33P_MOCK_SCALE=chats=200,messages=20000,members=500,
  picture=640,incoming=5,backlog=200000`.
- CPU, RSS and context switches come from `/proc`; GPU use from DRM fdinfo;
  frame gaps from `WAYLAND_DEBUG=client` commit timestamps. There is no
  profiler on the machine (`perf_event_paranoid=2`, no valgrind).
- The mock needs debug assertions, so measurements use the `testbox` cargo
  profile (release plus assertions, fat LTO).
- The host desktop is never captured. Only the private display's own shots.

## 2. Where it started

First comparison, medians of 5 rounds, Slint / GPUI:

| | Slint | GPUI |
|---|---|---|
| Incoming messages, 5 a second, CPU | 24% | 7% |
| Idle in a chat after the caret stops | 1.4% | 0.0% |
| Scrolling the 20,000-message thread | 33% | 33% |
| Typing 80 keys | 9.8% | 14.1% |
| Switching 100 chats | 34% | 15% |
| RSS after all phases | 228 MB | 194 MB |
| Threads | 22 | 60 |
| Release binary | 40.9 MB | 48.2 MB |
| Catching up with 200,000 messages | 2.9 CPU s | 2.6 CPU s |

## 3. What was wrong, and what fixed it

### GPUI: the idle redraw (before the measured pass)

GPUI redraws the whole window on every `cx.notify()` and re-runs every uncached
view's `render` each frame. An idle GPUI window was repainting because the
composer's text caret blinked forever, and a hidden composer input never
received blur (an input that is not in the rendered frame gets no blur event),
so its blink kept running even when a banner replaced the field.

- `theme.rs`: the caret blinks for 10 s and then rests
  (`CursorBlink::default().with_idle_timeout(..)` via `Theme.cursor_blink`).
- `composer.rs`: when a bar replaces the field, the row is kept in the frame at
  zero height just long enough to deliver the blur; focus returns with the field.

Result: idle in a chat is 0% CPU and 0 wakeups a second.

### GPUI: render cost and size

- The messages list became a cached view (`Entity::cached`), observing only what
  is not `ChatState`. Scrolling went from 33% to 19% CPU, switching chats from
  15% to 11%, typing from 14% to 5%.
- Hover toolbars and menus on a bubble are built only while hovered.
- Images and the atlas: dropped textures are released instead of kept.
- Icons: about 133 Lucide icons are embedded instead of the whole set (the
  binary went from 53.3 to 52.2 MB). A test keeps the list honest, excluding
  test files.
- RSS after all phases: 194 to 184 MB.

### Slint: the animation that never stopped

Every incoming message kept the Slint window repainting at 60 fps. Cause: a
Slint animated binding with no previous value "animates" from a value to itself
the first time its inputs change, and the animation runs for its full duration.
A text cursor blinker also ran forever.

- App-side: the relative time on a bubble is now an integer minute
  (`sent-minute: int`) so the binding does not change every tick, and a fade
  gap check avoids restarting fades (`b33p-slint/src/ui/`, `ui/types.slint`).
  This gave 24% to 23% incoming CPU and 228 to 204 MB RSS: small.
- Fork patches in `b33p-slint/fork-patches/` (against `barafael/slint`):
  `0001-no-op-animation.patch` skips animations from a value to itself and
  `0002-cursor-blink-timeout.patch` stops the caret after 10 s. These bring
  incoming CPU from 23% to 7.6% and idle from 1.0% to 0 CPU, 0 wakeups. They are
  **not pushed**: the fork and the five workspace `rev` entries are unchanged.
  Patch 0001 looks like a real upstream bug and may deserve a draft PR.

## 4. Initial synchronisation

`bench.py --sync-rounds N --backlog M` measures catching up: the link comes up,
the backlog is delivered, `Event::Synced` arrives.

The first number (2.6 to 2.9 CPU s for 200,000 messages) was mostly an artefact
of the mock: it sent the backlog newest first, so every message was inserted at
the front of its chat's history (quadratic). Real servers deliver oldest first.
Fixes:

- the mock sends oldest first, and the backlog goes out before `Event::Synced`;
- messages are 25% smaller (`LinkPreview.image` is boxed);
- notification text is no longer built per message while catching up;
- the real backend's trust refresh no longer reads a whole table per message.

Now: about 0.4 CPU s (Slint) and 0.35 CPU s (GPUI), RSS about 290 MB on both
(was 330 and 347 MB). Wall time sits at the mock's 2 s link-wait floor plus
about 0.5 s. The real backend was not run against Signal. The sync worker saw
roughly 6 to 8 small database calls per received message in `signal/receive.rs`;
they are unchanged.

## 5. Final numbers

| | Slint before | Slint merged | Slint + fork patches | GPUI before | GPUI after |
|---|---|---|---|---|---|
| 5 messages a second arriving, CPU | 24% | 23% | **7.6%** | 7.2% | **4.1%** |
| Idle in a chat after 20 s: CPU / wakeups a second | 1.4% / 72 | 1.0% / 69 | **0 / 0** | 0 / 0 | 0 / 0 |
| Scrolling the 20k thread, CPU | 36% | 27% | 28% | 33% | **19%** |
| Typing 80 keys, CPU | 9.8% | 6.3% | 6.3% | 14.1% | **4.9%** |
| Switching 100 chats, CPU | 34% | 28% | 27% | 14.7% | **10.6%** |
| RSS after all phases | 228 MB | 204 MB | 204 MB | 194 MB | 184 MB |
| Threads | 22 | 22 | 22 | 60 | 60 |
| Release binary | 40.9 MB | | | 53.3 MB | 52.2 MB |
| Catching up, 200k messages (CPU s) | 2.9 | 0.4 | 0.4 | 2.6 | 0.35 |

Frame pacing while scrolling is 62 fps with no gaps over 33 ms on both. An
earlier run showed about 33 ms gaps; it was put down to load or the display and
not isolated. A 2-minute soak grew Slint's RSS by 0.01 MB a minute and GPUI's by
0.36; the earlier 20-minute soak had Slint at 1.07 and GPUI at 0.48, so the
soaks do not compare.

## 6. Feature parity done alongside

`b33p-gpui/PARITY.md` holds the table, checked in both directions. Fixed in
GPUI: shortcuts did nothing until the first click; Esc went nowhere when the
focused view left; the settings page overflowed under about 850 px; long banner
errors did not wrap; about 25 elements had no screen-reader role; the "System"
theme only applied at startup. Tests: 13 end-to-end and 26 of Slint's 27 UI
tests ported (the 27th is a benchmark workload), plus about 8 new ones, all
without a display and now in CI.

Not closed: Windows-only session-end handling and screen-capture exclusion
(cannot be built here); in GPUI the reaction popup after a hover dwell, harness
`screenshot`, Wayland activation tokens after startup, the Qt widget style, and
the cosmetic fades.

## 7. Limits and open questions

- GPUI keeps 60 threads (one worker per core, no knob). About 85 MB of its
  memory at the chat list is GPU driver and shader compiler libraries.
- Switching chats in GPUI cost about 12% more in the worker's own before/after
  (about 1 ms a switch); the cause is not found.
- One Slint static round showed low activity and no caret blink after 10 s; it
  could not be reproduced in 8 runs and is left as an unexplained outlier.
- The Slint numbers that matter depend on the unpushed fork patches.
- `ChatMessage.quote` and `.preview` could be boxed once the other session's
  send work has merged.
- Untried: GPUI's `gpui-fast` feature, further binary-size cuts, Windows and
  macOS compile checks.

## 8. Next

1. Decide whether to push the two fork patches to `barafael/slint`, bump `rev`
   in the five workspace `Cargo.toml` entries and `Cargo.lock`, and open an
   upstream draft PR for 0001.
2. When the other session's PRs (#135 to #138: mention IME, quote jump, pins,
   read more) are merged, merge `main` into `gpui` and port quote jump
   (`Action::OpenQuote`), pins, relative times and receipts to `b33p-gpui`.
