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
