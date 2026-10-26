# Branches for a green testbox

The work towards every testbox phase passing, across b33p and the forks it builds on. No pull
request is opened for these yet; each line says what the branch is for and where it stands.

## b33p (barafael/beep)

`testbox-green`, pushed, on `main` (since #69 merged: the forks' rebased integration branches with
the session lock). Its commits, oldest first:

| Commit | What | State |
|---|---|---|
| `testbox: presage-cli and the sticker tool take the session lock too` | #69 moved b33p to the locked presage and libsignal-service, but not the box's presage-cli and sticker tool; presage-cli's answers to Desktop's sync requests went out with a counter already used | Box: all phases passed (see below) |
| `testbox: with-display hands the stop signals on to its client` | `with-display` is PID 1 in the client containers and ignored SIGTERM, so `podman restart b33p` killed b33p after 10 s with its database open | In the box |
| `testbox: test --screenshots, and a record of every phase's logs` | `target/testbox-record/<phase>/`: every container's log, Desktop's logs, logcat; with `--screenshots` each display after each check (testbox/shot) | In the box |
| `testbox: reactions, replies and Desktop's deletes in the desktop phase` | Five new checks and the Desktop driver calls they need | In the box |
| `testbox: core checks a send while the server is down, and its retry` | TESTPLAN.md §1.2 item 3 | In the box |
| `xtask: a test that the box builds on b33p's presage and libsignal-service` | Fails when the Containerfile, presage-cli.lock or the sticker tool drift from Cargo.toml's pins | `cargo test` passes; fails on `main`'s Containerfile |
| `testbox: cargo testbox run, the phases on fresh boxes, in rounds` | The down/up/test loop with each phase's clients, and a summary; `down` with no pod played is a no-op | In the box |
| `testbox/README: run, the records, screenshots and the new checks` | | |
| `testbox: the failed-send check pauses the server instead of stopping it` | A stopped server closes b33p's connection, and b33p lets nobody write until caught up; TESTPLAN.md §1.2 item 3 ("send while offline … Not sent · Retry") describes what b33p prevents | In the box |
| `b33p-core: reactions, deletes, edits and quotes find an edited message` | Signal's apps name an edited message by its latest edit's time; b33p dropped reactions, deletes and second edits to edited messages, and its replies quoted the first text. Pins presage `fix/edited-targets` | Unit tests pass; in the box |
| `testbox: an emoji font in b33p's image` | Reactions showed as empty boxes in screenshots; it then exposed the Qt crash below | In the box |
| `testbox: each run keeps its records in a directory of its own` | `run` deleted the last run's records | |
| `testbox: the failed-send check waits for the keepalive to give up` | A send on a silent connection fails after ~2 min (keepalive), then presage's retry | In the box |
| `b33p-core: a new contact's chat takes their name once presage has it` | presage saves a new sender's contact in a background task and says nothing; the chat kept the UUID as its title when b33p was first | In the box |
| `Take the Slint fork's Qt backend drawing emoji itself` | b33p aborted ("double free or corruption") drawing 📎 with Noto Color Emoji on Debian 12's Qt 6.4 | Mock: no abort, valgrind 0 errors (5 invalid writes before); in the box |
| `A failed send's banner says it in words, and goes with the retry` | The banner showed the library's error and stayed after Retry | Unit test; core checks it |
| `Receipts and read syncs for edited messages, as Signal's apps send and take them` | Receipts for our edits found nothing, others' edits were never read; Signal-Desktop's behaviour at the box's revision | Unit tests; storage checks both ways |
| `TESTPLAN: as b33p is now, and what the test box checks` | §1.2 item 3 as b33p behaves; edited-message items; *(box: phase)* marks | |
| `testbox: b33p reads in the box, and Desktop reads an edit with what follows` | The harness's window focus held only until the next message; Desktop reads an edit with the next read | Box: storage |
| `b33p-core: a new contact's chat takes their name, however quick presage is` | Chats with someone new kept their id as title when presage's profile fetch beat b33p's look; desktop checks the title | Box: desktop |
| `testbox: Desktop reads b33p's edit once someone is at it` | Desktop reads only for 15 s after a click or key (`ActiveWindowService`) | Box: storage |
| `testbox agent: a swipe that changes nothing is looked past, not waited out` | Android's setup timed out scrolling to a row already shown: a swipe at a list's end sends no event | Box: android |

## presage (barafael/presage)

| Branch | What | State |
|---|---|---|
| `langis/integration` (`b664297`) | Upstream main `30ca664` (past #359, link previews) with the fork's PRs and fork-only commits, rebased 2026-10-26: our transfer-archive commit's `reqwest` line dropped for #359's, `session_lock` beside #359's `link_preview_client` | Pinned by `reconcile-upstream` |
| `pr/edited-targets` (`eea5a19`), pushed | The same commit on `pr/sender-owns-message` (whisperfish/presage#453, which it needs), both rebased on upstream main `291c7ee` past #471; store and manager tests pass | whisperfish/presage#475 (stacked on #453) |
| PR branches | Rebased on upstream main `30ca664` where they conflicted: `pr/session-lock` (#473, `d17f7d0`), `pr/receive-transaction` (`01ed8c3`, on it; PR not opened yet), `pr/groups` (#448, `d397146`, pinned to lss #473 rebased), `pr/transfer-archive` (#447, `a49f2e0`). The other ten build merged with upstream main as they are | |

## libsignal-service-rs (barafael/libsignal-service-rs)

| Branch | What | State |
|---|---|---|
| `langis/integration` (`a32d96b`) | Upstream main `72a0987` with the fork's PRs, #442 and #489, and #489's tests | Pinned by `reconcile-upstream` |
| `groups-v2-changes` (`93f9dc2`) | #473 rebased on upstream main `72a0987` (presage main needs its calling relays) | Pinned by presage `pr/groups` |

## Slint (barafael/slint)

| Branch | What | State |
|---|---|---|
| `langis/integration` (`2aa99c5`) | Upstream master `7e32802` (2026-10-26) with merges of #13985 (`eed8be0`) and #14449 (`1f053c6`) | Pinned by `reconcile-upstream` |
| `pr/qt-color-bitmap-glyphs` (`1f053c6`), pushed | The emoji commit, master merged in, and `CBDT` and `sbix` in the file's spell-check words (the PR's `lint_typecheck` failed on them) | slint-ui/slint#14449 |

## Testbox runs

Each phase on a fresh box (`down --wipe`, `up`, `test`).

| Revision | Rounds | core | desktop | attachments | android | storage | groups |
|---|---|---|---|---|---|---|---|
| `main` 954e604 | 2 | 2/2 | 0/2 | 0/2 | 0/2 | 0/2 | 1/2 |
| `testbox-green` 46ea502 (the lock only) | 1 | 1/1 | 1/1 | 1/1 | 1/1 | 1/1 | 1/1 |
| `testbox-green` 28dc6bc | 1 | 0/1 (the failed-send check stopped the server; fixed since) | 0/1 (reactions and quotes of edited messages; fixed since) | 1/1 | 1/1 | 1/1 | 1/1 |
| `testbox-green` 06061f7 (all fixes) | 2 | 2/2 | 2/2 | 2/2 | 2/2 | 2/2 | 2/2 |
| `testbox-green` 3261c5e…c0e2391 (banner, edited receipts, chat names, #71) | 1 | 1/1 (3261c5e) | 1/1 (3261c5e) | 1/1 (7182af0) | 1/1 (c0e2391; at 7182af0 the agent waited out a swipe at a list's end) | 1/1 (7182af0; at 3261c5e the edit check clicked nothing in Desktop) | 1/1 (7182af0) |
| `main` 8ff6797 (#72, #70 merged) | 1 | 1/1 (on rerun; first under host overload) | 1/1 (on rerun) | 1/1 (on rerun) | 1/1 | 1/1 | 1/1 |
| `forks-moved` 85e2ed0 (#74: Slint and presage rebuilt on upstream) | 1 | 1/1 | 1/1 | 1/1 | 1/1 | 1/1 | 1/1 |

## Open

- (Explained) The peer titled by its UUID: presage's background contact save racing b33p;
  fixed in b33p-core.
- (Fixed) Receipts and read syncs for an edited message name its edit's time (Signal-Desktop
  `MessageReceipts`); b33p kept deliveries by the first time. Now as Desktop, and checked.
