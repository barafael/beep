# Why the Slint client redrew 60 frames a second while messages arrived

Found 2026-11-09, from the multi-way benchmark (`performance-multiway.html`), which showed the
FemtoVG and Skia builds using 3 to 4 times GPUI's CPU with messages arriving (15 to 16 % against
4.7 %), and 330 wakeups a second against 24, for 5 messages a second.

## What it was

Slint's own counter (`SLINT_DEBUG_PERFORMANCE=refresh_lazy,console`) says it: with no messages
arriving the window draws 2 frames a second (the caret), with 5 messages a second it draws
**61, without a break**, whether the list follows the newest message or not, with a 200-message
chat as with a 20,000-message one, and with the app's own animations switched off.

Every frame is a whole-window render in FemtoVG and Skia, so the CPU cost follows the number of
frames. Two independent things kept the animation driver "active" the whole time (found by
logging the stack of every `set_has_active_animations` call, and every animated value, in a
copy of Slint):

1. **Animations from a value to itself** (Slint patch 0001, draft PR #14823). A row made for a
   message gets its data after it was made and again when its neighbours change; each time, its
   `animate background` / `animate opacity` ran for its full 120 to 240 ms from a value to the
   same value, and redrew the window all the while. Logged: `f32 from->to delta 0.0 duration
   240 ms`, `Brush ... delta 0.0 duration 160 ms`.
2. **The scrollbar thumb.** Fluent's `ScrollBar` has `animate width, height { duration: 150ms }`
   on its thumb, and the thumb's length is `track * page / (content + page)`. Every message
   changes the content height, by a hundredth of a pixel in a long chat, and starts a new
   150 ms animation: 5 a second is continuous. It does not follow the app's reduced-motion
   setting, which only scales our own `Motion` durations.

Either one alone keeps the window redrawing, which is why patch 0001 showed no effect on the
FemtoVG and Skia builds in the benchmark (only on Qt, which has no Fluent bar), and why switching
the bar off alone changed nothing either. Both gone: 10 to 20 frames a second, about 3 per
message (the new row, the row before it losing its time and ticks, the follow-scroll).

## The change

Both are fixed in Slint, and the app has nothing of its own for it:

1. #14823 (patch 0001) and
2. slint-ui/slint#14841: the thumb of the Fluent, Cupertino and Cosmic scrollbars no longer animates
   its width and height. Fluent and Cupertino animate the thickness through `size` / `pad` already.

Both are merged into our fork's `langis/integration` (`2a802138d`), which the workspace pins.
(First attempt: a scrollbar drawn by the app over a `ListView` with `always-off`, see commit
`0c81f02`; reverted once the Slint change measured as well.)

## Measured (medians of 2 rounds, release builds with debug assertions, `bench.py`)

| | FemtoVG | FemtoVG + 0001 | FemtoVG + 0001 + bar | Skia | Skia + 0001 + bar | GPUI |
|---|---|---|---|---|---|---|
| CPU %, messages arriving | 14.1 | 15.5 | **4.9** | 16.6 | **5.5** | 4.9 |
| wakeups/s, messages arriving | 317 | 320 | **86** | 213 | **57** | 25 |
| CPU %, 60 s soak | 14.8 | 15.9 | **4.8** | 15.7 | **5.1** | 4.7 |
| CPU %, scrolling | 19.4 | 20.1 | 20.4 | 21.9 | 18.2 | 21.7 |
| CPU %, switching 100 chats | 21.8 | 21.2 | 20.3 | 20.6 | 18.8 | 11.9 |
| CPU %, typing 80 keys | 9.8 | 9.7 | 8.0 | 9.9 | 9.9 | 6.8 |
| scrolling frame gaps over 33 ms | 0 | 0 | 0 | 0 | 0 | 0 |

Messages arriving, the case that mattered, is now level with GPUI. What is left of the
difference: switching chats (about 20 % against 12 %), typing, and the panels. Those are not
overdraw (no continuous frames in them); they want a real profile of the render path.

## Measured with the Slint change instead (FemtoVG, release, stock app scrollbar)

Messages arriving: 3.7 % and 4.0 % CPU in two runs (stock 14.1 %, own bar 4.9 %, GPUI 4.9 %), on a
machine that was busy compiling, so rough.

## What is left: a profile of chat switching and typing (2026-11-09)

`samply` works at `perf_event_paranoid` 1 and `perf_event_mlock_kb` 65536 (both reset at reboot; the
default mlock of 516 KB gives "mmap failed" with 32 CPUs). Two things had to be done for usable
stacks: build with `CARGO_PROFILE_TESTBOX_STRIP=false`, `CARGO_PROFILE_TESTBOX_DEBUG=line-tables-only`
and `RUSTFLAGS="-C force-frame-pointers=yes"` (Arch's libc and Mesa have frame pointers), and
symbolise `b33p` addresses myself (samply cannot read `/usr/local/bin/b33p`, which only exists in
the container; its addresses are file offsets, add 0x1000 for the text segment).

FemtoVG, fixed build, main thread, 100 chat switches and 80 keystrokes (about the same picture):

| | share of the main thread |
|---|---|
| rendering (`FemtoVGRenderer::render`) | 69 to 73 % |
| walking the item tree and drawing the rows | 45 to 50 % |
| of that: drawing text (`draw_text`, glyph runs) | 24 % |
| GL command submission (`flush_to_output`, Mesa) | 10 to 12 % |
| instantiating the rows of the list (`ensure_updated`) | 6 to 10 % (switching only) |
| measuring text (`text_layout_info`, `text_size`, paragraphs) | 6 to 8 % |
| `glGetError` after every GL call (`check_error`) | 7.6 to 9.5 % |
| dependency-list validation (`debug_assert_valid`) | 2.5 to 5 % |

The last two exist only with debug assertions on (femtovg: `debug = cfg!(debug_assertions)`). They
cost less than they look: the same build with debug assertions off for all dependencies (b33p's own
crates keep them, the mock needs them: `--config 'profile.testbox.package."*".debug-assertions=false'`)
is 5 to 8 % cheaper, and GPUI does not change:

| CPU % (median of 2) | switching 100 chats | scrolling | typing 80 keys |
|---|---|---|---|
| Slint FemtoVG, debug assertions | 18.5 | 19.0 | 6.9 |
| Slint FemtoVG, none in dependencies | 17.2 | 17.5 | 6.6 |
| GPUI, debug assertions | 11.5 | 21.5 | 4.8 |
| GPUI, none in dependencies | 11.1 | 20 to 27 | 4.6 |

So the remaining 1.5 times (switching) and 1.4 times (typing) are not the profile and not overdraw:
every frame is a whole-window render, and the text of every visible message is drawn glyph by
glyph again (hash lookups per glyph in femtovg's `draw_glyph_run`, `GlyphRunIter::next`), with no
retained batches. A keystroke repaints the lot. There is no single hot function; the cost is
spread over the renderer. The ways down are renderer work (partial rendering or retained text
batches in FemtoVG), not app work.

## Left open

- Both Slint PRs are drafts upstream; until they are merged and released, the fork's
  `langis/integration` carries them.
- Other lists with the Fluent bar (chat list, media panels, emoji and sticker pickers) had the same
  animation but did not grow while the window was idle.
- Panels were not profiled separately (they are full-window renders like the rest).
