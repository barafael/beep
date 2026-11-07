# Follow-up plan: performance and parity leads

What to do next after the pass described in
[performance-gpui-vs-slint.md](performance-gpui-vs-slint.md), in an order that puts what
changes the numbers first. The questions behind each item, with what is known and guessed, are
in [performance-open-questions.md](performance-open-questions.md). Parity is tracked in
`b33p-gpui/PARITY.md`.

## Decided by the owner

- Patch `0001` (no animation from a value to itself) goes upstream to Slint as a draft pull
  request: https://github.com/slint-ui/slint/pull/14823 (branch `no-op-animation` of the
  `barafael/slint` fork, with a test; write-up in
  [upstream-slint-no-op-animation.md](upstream-slint-no-op-animation.md)). When it is merged
  upstream, bump the fork pin and delete the patch file.
- Patch `0002` (the text cursor stops blinking after 10 s) stays in this repository in
  `b33p-slint/fork-patches/`. It is a behaviour choice of b33p, not a bug fix.
- The Qt widget style (system or Fusion) is not wanted in the GPUI window.
- Every other parity gap is wanted.

## Step 0: finish the merge of `main` (in progress)

`main` (PRs #134 to #139: mention input method, quote jump, pins, Read more, receipts) is
merged into `gpui`. Two workers are porting it to GPUI (quote jump, pins, Read more,
receipt dialog, relative times) and closing the remaining parity gaps (reaction popup, harness
screenshot, activation tokens, animations, Windows session end and capture exclusion). When
both are merged: fmt, clippy with and without `harness`, `cargo test` of all crates, and the
final benchmark again, because new views can add idle redraws (check the "idle in a chat"
row stays at 0 wakeups).

## Step 1: make the benchmark trustworthy (about half a day)

Needed before any new number is believed.
1. Make `bench.py` verify, for each round, that the window got keyboard focus
   (`wl_keyboard.enter` in the `WAYLAND_DEBUG` log) and fail or retry the round otherwise.
   This is the leading explanation of the Slint outlier (Q1).
2. Run the static scenario 30 times for both clients to settle Q1; write the result into the
   questions file.
3. Record the machine load at the start and end of each round in the report, and refuse to
   publish a median when the load average is above a threshold.

## Step 2: the two cheap leads (about a day)

1. **Switching chats in GPUI (Q2).** Add a render counter around `MessagesView::render` and
   the image upload. If a switch renders twice, fix the observers; if images are re-uploaded,
   keep the atlas drop only for chats not visited recently. Bisect the worker's commits if
   the counters say nothing. Success: the switch scenario at or below the pre-pass CPU per
   switch, with the rest of the table unchanged.
2. **Box `ChatMessage.quote` and `.preview` (Q5).** Now that the other session's PRs are in.
   Success: RSS after the 200,000-message catch-up drops by at least a few MB, and `cargo
   test` stays green.

## Step 3: the real backend (needs the owner's go-ahead)

Q4. Count store calls and their time per received message with a counting layer around the
presage store, against a test account. Decision for the owner: is a staging test account
acceptable? If yes, produce the table, then batch the per-message calls in one transaction
where the table says it matters (a message's own receipt, trust and thread updates). Success:
catch-up of a large real backlog is dominated by decryption, not by SQLite.

## Step 4: untried options for the GPUI binary and memory (opportunistic)

1. `gpui-fast` and other gpui feature switches: build once, measure binary size, RSS and
   the whole table. Keep only if it improves something without a regression.
2. Binary size: the current 52.2 MB against Slint's 40.9 MB. Check `cargo bloat`-style
   numbers by crate (is it only `gpui-component`'s widgets we do not use?), strip and
   `opt-level="s"` for dependencies only, and dead-code elimination of `gpui-kit`'s unused
   components. Success: below 48 MB.
3. Thread count (Q3): check newer `gpui` releases for a setting; otherwise document it.
4. Other drivers (Q6): run the benchmark on llvmpipe and, if available, another GPU, so the
   comparison does not rest on one driver.

## Step 5: platforms that could not be built here

1. Windows: the GPUI session-end handling and capture exclusion are written without a
   Windows machine (see PARITY.md for what could be compile-checked). Run the GPUI window on
   Windows, check both for real (log off with b33p open; a screen capture of the window must
   be black), and compare the memory and CPU numbers with the Linux ones.
2. macOS: compile check and a short look. GPUI's macOS path (Metal) is its best-supported
   one, so a first look at the numbers there is worthwhile.

## Step 6: keep the two windows in step

The two windows share all non-view code, which has kept them close. To keep it that way:
1. After each merge of `main`, port the new view features the same day (this pass found six
   at once: quote jump, pins, Read more, receipts, relative times, mention input method).
2. Keep PARITY.md's mechanical check (every `ChatState` method and view field read by one
   window is read by the other) in CI; it caught gaps before.
3. When the Slint fork patch `0001` is released upstream, drop the patch file and bump the
   fork's revision; keep `0002` as a patch on top until the project decides to carry its own
   fork revision.
