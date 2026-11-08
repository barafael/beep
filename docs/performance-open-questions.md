# Open questions after the GPUI and Slint performance pass

What the pass in [performance-gpui-vs-slint.md](performance-gpui-vs-slint.md) did not
explain or did not touch. Each entry says what is known, what is only a guess, and the
cheapest experiment that would settle it. The order of work is in
[performance-followup-plan.md](performance-followup-plan.md).

How things were measured, so that an experiment can be compared with the old numbers:
`testbox/gpui-view/bench.py` (medians of interleaved rounds, a private display that draws on
the host GPU, real wheel and key events through the display's VNC `interact` socket, the
`testbox` cargo profile, `B33P_MOCK_SCALE`). The machine is shared and was loaded (load
average 5 to 7) during the final run, so differences under a few percent are noise.

## Q1. Why was one Slint "static" round quiet?

**Known.** In the first of three rounds of the "static" scenario (idle in an open chat, the
caret blinking for its first seconds) the Slint client showed low activity and no caret
blink after 10 s, unlike every other round. It is in `target/bench-final/report.md` as the
outlier. Eight further runs did not reproduce it. The median is not affected (three
rounds), but a first-run effect would be.

**Guesses, most likely first.**
1. The window was not active in that run. On Wayland a window is active once
   `wl_keyboard.enter` arrives, and the benchmark makes a keyboard exist by sending a
   Shift key to the `interact` socket. If the key press raced the window's mapping, the
   text input never got focus, so there was no caret to blink. This fits "no blink" and
   "low activity" and does not need Slint to misbehave.
2. A first-run cost (cold font or shader caches in the container's fresh `HOME`) that
   shifted the timing of the first 10 s.
3. The cursor timer of the patched fork (`0002`) starting from a stale state. Not checked
   which of the two Slint binaries the outlier came from; the report of that run says.

**Experiment.** Run the static scenario 30 times with a fresh container each time and log
per run whether `wl_keyboard.enter` was seen (`WAYLAND_DEBUG=client`) and the wakeups in
seconds 0 to 10. Count how many runs are quiet. If the quiet runs are exactly the runs
without a keyboard enter, guess 1 is confirmed and the fix is in `bench.py` (send the key
only after the window's first commit, and fail the round if no enter was seen).

**Why it matters.** If it is guess 1, the benchmark can produce wrong numbers silently for
either client. A "window active" check in `bench.py` would make every future run
trustworthy. If it is a real Slint behaviour, a client can end up with no caret.

## Q2. Why is switching chats about 12% slower in GPUI after the pass?

**Known.** The GPUI worker's own before/after (same session, interleaved) measured the
"switch 100 chats" scenario at about 12% more CPU time per switch after its changes, roughly
1 ms a switch. In the final comparison against the GPUI baseline the scenario is still
faster overall (14.7% to 10.6% CPU), because the cached messages view and the hover-only
toolbars saved much more elsewhere. So this is a regression inside an improvement, not a
net loss. The cause was not found; there was no profiler (`perf_event_paranoid=2`, no
valgrind).

**Guesses.**
1. `MessagesView` is a cached view (`Entity::cached`). A chat switch invalidates it from
   several observers (selection, history, highlights), and it renders more than once per
   switch instead of once.
2. Dropping images from the GPU atlas on a switch (`images.rs` and `shared.rs`) means the
   images of the chat you return to must be decoded and uploaded again. The benchmark
   returns to chats it has visited.
3. The icon set embedding (133 icons instead of the full Lucide set) is not a candidate for
   CPU, only for size.
4. More `notify` calls from the observer wiring added to keep the cached view up to date.

**Experiment.** Bisect on the performance worker's commits (the merge `6c13d92` brought them in; `git log
6c13d92^1..6c13d92^2`), building each with the `testbox` profile and running only the switch scenario with
3 rounds. Where the profile has no tools, count instead: add a `tracing` span or an
`Instant` counter around `MessagesView::render` and the image upload, and log renders per
switch. Two renders per switch would confirm guess 1 without any profiler. Use CPU seconds
and wakeups, not wall time, since both are less sensitive to load.

## Q3. Can GPUI be made to use fewer than 60 threads?

**Known.** GPUI starts 60 threads (Slint 22) regardless of what the client does. The names
are in `bench.py`'s thread report: most are the executor's pool. In `gpui-pre-0.3.11` the
background executor is sized from the number of CPUs with no setting. The machine has 32
logical cores.

**What it costs.** Little resident memory (stacks are mostly untouched address space), and
the idle threads are parked, so no CPU. It shows up in `ps` and in sandboxes with a thread
limit, and it is a risk on machines with few cores only in the sense that it scales the
other way. This is cosmetic more than a cost, which is why it was left.

**Options.** (a) Check whether a newer `gpui` release or `gpui-kit` exposes a thread count.
(b) A `[patch]` of `gpui-pre` for one constant, which the project would then carry.
(c) Leave it and document it. Recommendation: (a), then (c).

## Q4. What does the real Signal backend cost while catching up?

**Known.** The catch-up benchmark uses the mock, whose backlog is now oldest-first. After
that fix, 200,000 messages cost about 0.4 CPU seconds in the clients, and the wall time is
at the mock's own 2 s link-wait floor. The real backend (`b33p-core/src/signal/receive.rs`,
`handle_content`, and the store calls it makes through presage's SQLite store) was not
exercised: it needs a Signal account and a real server. While reading it the sync worker
found that each received message causes a handful of small database calls (its estimate was
6 to 8). Only one was removed: the trust refresh that read a whole table per message.

**Unknown.** The exact list and what each costs. The estimate came from reading, not from
counting.

**Experiment.** Wrap the store in a counting layer for a debug build: count calls and time
per kind of call while a test account receives a large backlog (a few thousand messages
sent to it from a second account on Signal's staging environment, if available, or the
replayed envelopes of a recorded session if the project has any). The result is a table of
calls per message and microseconds per call, which says whether batching them in a
transaction is worth it. The decision for the owner is whether a test account on staging
is acceptable.

## Q5. Should `ChatMessage.quote` and `.preview` be boxed?

**Known.** Making `LinkPreview.image` boxed made a `ChatMessage` about 25% smaller, and the
whole backlog 330 to 290 MB. `quote` and `preview` are the next large optional fields.
They are `Option`s that most messages leave empty, so boxing them would shrink messages
further. They were not touched because the other session was editing the same structs
(quote jump, `Preview` urls in `signal/send.rs`).

**Now.** Those PRs (#136 and the later ones) are merged into the `gpui` branch. It is safe
to try. **Experiment.** Box both, run the catch-up scenario with `backlog=200000` and look at
the "RSS after sync" column; keep it only if it saves at least a few MB, since boxing costs
an allocation for the messages that do have a quote.

## Q6. Where is the GPUI memory that is not ours?

**Known.** About 85 MB of GPUI's resident memory at the chat list is GPU driver and shader
compiler libraries (the Vulkan driver and shader compiler libraries mapped into the process), not Rust allocations.
RSS after all phases is 184 MB for GPUI against 204 MB for Slint, but Slint's number is CPU
rendering, so a machine without a good GPU driver would show different numbers for both.

**Open.** What the same client costs with another driver (NVIDIA proprietary, Intel,
llvmpipe) and with the `gpui-fast` feature set. Only the AMD driver was measured. The
fair claim in the comparison is therefore "on this machine".

## Q7. Is the 0.36 MB a minute of GPUI memory growth real?

**Known.** A 2-minute soak with arriving messages grew GPUI by 0.36 MB a minute and Slint by
0.01. The earlier 20-minute soak gave 0.48 and 1.07, so the two soaks do not compare, and a
2-minute window is dominated by warm-up. Messages themselves accumulate in the history (that
is growth by design), so a non-zero slope is expected.

**Experiment.** A 20-minute soak of the patched Slint and the current GPUI, subtracting the
expected message growth (messages received times the per-message size), looking at the
remainder.

## Added after the multi-way run and the extended testbox runs (2026-11-08)

The multi-way comparison ([performance-multiway.html](performance-multiway.html), raw tables in
[performance-multiway-raw.md](performance-multiway-raw.md)) measured seven builds on the mock
backend at f9c0db9 (Slint on Qt, FemtoVG and Skia, each with and without the fork patches, and
GPUI). The testbox runs are in [testbox-extended-runs.md](testbox-extended-runs.md). They left
these open.

### Q8. Do the merges since the benchmark change the numbers?

**Known.** The benchmark ran on a tree based on 8ddd7d8; the testbox runs on 31083b5. Main is
now past both: #142 (per-chat notification settings through the agent), #145 (no picture viewer
in GPUI), #146 (`window.rs` takes `instance::Request`), #152 (the GPUI window review) and #153
(non-Qt Slint on Linux). New views can add idle redraws.

**Experiment.** Run `bench.py run --ways-dir <binaries> --rounds 3 --sync-rounds 3
--frames-rounds 2 --backlog 200000 --soak 120 --max-load 6` again for the same seven ways, and
check that "idle in a chat after the caret stops" is still 0 wakeups for GPUI and the patched
Slint builds. About 2 hours on a calm machine. The testbox phases should run on the new main too,
with `--ui gpui` and the default.

### Q9. Should the Wayland activation-token reconnect stay?

**Known.** GPUI accepts an activation token only when it connects, so the window reconnects to
take a token handed over later. The reconnect drops a clipboard selection the window owns.
Written and merged, not verified at a compositor.

**Decision for the owner.** Keep it (a lost selection is rare, a window that cannot raise itself
is a visible bug), or drop it and document that a second launch cannot raise the window on
Wayland. A check that copies, triggers the token and pastes would give the cost.

### Q10. What do the non-Qt Slint windows lack that Qt has?

**Known.** On Linux Slint can now be built with FemtoVG or Skia and no Qt (`gpu-femtovg`,
`gpu-skia`; Qt stays the default; Fluent is the widget style without Qt). GPU use was verified
and the benchmark ran on them. Not exercised: input methods (IME), the tray icon, file dialogs,
the clipboard, drag and drop, accessibility, and the XDG activation token. The testbox has only
ever run the Qt build.

**Experiment.** Run each of these by hand in a Wayland session with each of the three builds,
then add `--features gpu-skia` (or whichever is chosen) as a testbox build option. Until then
the report's claim is "for rendering and the benchmark's scenarios".

### Q11. Why do the fork patches help only on Qt?

**Known.** Patch `0001` (no animation from a value to itself) cut CPU with messages arriving
from 22.1% to 8% on Qt, and changed nothing measurable on FemtoVG and Skia (10.1 to 11.8 and
9.8 to 9.5; the first is inside the noise of 3 rounds). Patch `0002` (the caret stops blinking
after 10 s) takes the idle chat to 0 wakeups on all three.

**Guess, not checked.** The Qt backend's timers wake the loop for each animation tick even when
nothing changes, where the winit backends' frames are only requested when something is drawn.
Settle it by counting animation ticks and frames per second in the soak for both. It matters for
the upstream pull request (#14823): its description says what the patch does in Slint's core, so
it should say that the effect depends on the backend. The fork's revision in the workspace
`Cargo.toml` is bumped, and patch `0001` deleted, only when it is released.

### Q12. Which Slint renderer should Linux ship?

**Known.** Against Qt, FemtoVG and Skia used about 30% less CPU scrolling (15.5 and 15.4 against
22.8, % of a core) and switching chats (16.3 and 15.5 against 25.6), and made a quarter to a
sixth of the wakeups while scrolling, but cost memory: RSS after all phases 314 MB (FemtoVG) and 283 MB (Skia)
against 204 MB (Qt), and 20 to 27 threads against 22. Typing used more CPU on both (7.1 and 6.9
against 5.5). The binary is 45.8 and 52.7 MB against 40.8. GPUI (185 MB, 3.9% with messages
arriving) is lowest on most rows but has 60 threads. All of it on one AMD GPU and driver.

**Decision for the owner.** Qt stays the default for the system's look (the owner said the Qt
widget style is not important, but the system's fonts and colors are what Qt gives); which of
the GPU builds, if any, ships as a second package, and whether the .deb should list Qt as
optional. Needs Q10 first.

### Q13. Does the soak's memory growth come from history or from a leak?

**Known.** The 120 s soak in the multi-way run: Qt and FemtoVG flat (0 to 0.02 MB a minute),
Skia 0.25, GPUI 0.33. The 30-minute testbox soak went from 35 MiB to 68 MiB, with 47 to 48 open
files throughout, and its "nothing keeps growing" check passed; the samples of the last minutes
moved between 48 and 56 MiB, so the end point is a single high one. Messages accumulate in the
history, so some growth is expected.

**Experiment.** This extends Q7: a 20-minute soak per build in `bench.py`, subtracting the
messages received times the per-message size, and in the testbox a longer soak sampled more often
(the memory at each sample is in the record), then look at whether the remainder has a slope.
