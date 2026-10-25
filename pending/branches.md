# Branches for a green testbox

The work towards every testbox phase passing, across b33p and the forks it builds on. No pull
request is opened for these yet; each line says what the branch is for and where it stands.

## b33p (barafael/beep)

`testbox-green`, local, on `integration-rebase` (#69, which pins the forks' rebased integration
branches with the session lock). Its commits, oldest first:

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
| (uncommitted) Slint pin at `fix/qt-color-bitmap-glyphs` | b33p aborted ("double free or corruption") drawing 📎 with Noto Color Emoji on Debian 12's Qt 6.4 | Mock repro, then the box |

## presage (barafael/presage)

| Branch | What | State |
|---|---|---|
| `langis/integration-next` (`7e868e4`) | Upstream main with the fork's PRs and the session lock (whisperfish/presage#473); b33p #69 pins it | Base of the next |
| `fix/edited-targets` (`ce28171`), pushed | `integration-next` plus one commit: edits and deletes for everyone find an edited message by its latest edit's time (`ContentsStore::message_sent_or_edited_at`), and a stored edit names its original's time; a store test. For a PR, rebase the one commit on upstream main | Pinned by `testbox-green` (b33p and presage-cli) |

## libsignal-service-rs (barafael/libsignal-service-rs)

| Branch | What | State |
|---|---|---|
| `langis/integration-next` (`3b76dca`) | Upstream main with the fork's PRs and the `SessionLock` (whisperfish/libsignal-service-rs#488) | Pinned by `testbox-green` for presage-cli too; no change needed |

## Slint (barafael/slint)

| Branch | What | State |
|---|---|---|
| `fix/qt-color-bitmap-glyphs` (`6b8eef3`), pushed | `langis/integration-new` (`e38356d`, what b33p pins) plus one commit: the Qt backend renders glyphs of colour bitmap fonts (CBDT, sbix) with swash and draws them as images. Qt 6.4.2's `QFontEngineFT::loadGlyph` writes past a glyph buffer for them from `drawGlyphRun` (valgrind, b33p's mock on Debian 12); Qt 6.8.2 doesn't. For a PR, rebase the one commit on upstream master | Built into b33p's image |

## Testbox runs

Each phase on a fresh box (`down --wipe`, `up`, `test`).

| Revision | Rounds | core | desktop | attachments | android | storage | groups |
|---|---|---|---|---|---|---|---|
| `main` 954e604 | 2 | 2/2 | 0/2 | 0/2 | 0/2 | 0/2 | 1/2 |
| `testbox-green` 46ea502 (the lock only) | 1 | 1/1 | 1/1 | 1/1 | 1/1 | 1/1 | 1/1 |
| `testbox-green` 28dc6bc | 1 | 0/1 (the failed-send check stopped the server; fixed since) | 0/1 (reactions and quotes of edited messages; fixed since) | 1/1 | 1/1 | 1/1 | 1/1 |

## Open

- (Explained) The peer titled by its UUID: presage's background contact save racing b33p;
  fixed in b33p-core.
- Receipts and read syncs for an edited message name its edit's time too (Signal-Desktop
  `MessageReceipts`); b33p keeps deliveries by the first time. Not checked yet.
