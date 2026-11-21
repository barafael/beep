# What b33p can take from sigwin

[Othereum/sigwin](https://github.com/Othereum/sigwin) (inspected at `88ea8a5`, 2026-11-07) is a
Windows Signal client on the same stack: presage, libsignal-service, Slint. It is AGPL-3.0-only like
b33p, so code may be ported, not only ideas. A ported commit says so in its body
(`Ported from Othereum/sigwin <commit>`), and the About screen's notices name the project.

b33p is far ahead on the protocol: groups, stories, sender keys, history transfer, storage service.
sigwin was ahead on the **conversation's feel**, **memory** and **Windows integration**. Its fixes
live as `// sigwin patch` hunks under `vendor/` (listed in its `vendor/PATCHES.md`); where we take
one, it goes to our Slint fork's `slint/integration-branch` (a line in its `integration.sh`) and
upstream as a draft PR, not into a vendor directory.

Status as of 2026-11-21. Most of the plan of 2026-11-07 has landed (#134–#148, #182–#189); what follows is
what is left, then what was done and found, for the record.

## Open

### 1. The conversation's feel

Done on 2026-11-21 (see the table below), except **1.5 Time at the last line's right edge**:
built in #183 (a time shows once per run and minute; on a one-line message's line), a draft
until the owner has looked at the screenshots. Left over from 2.4: a testbox check that
Signal-Desktop sends an animated sticker (the box's sticker pack is still PNGs).

### 2. Memory of the open chat (phase 3.3)

The Windows runner measured idle private memory at 10.6 MiB on femtovg and 8.9 MiB on the software
renderer (#141), so the software renderer and sigwin's patches for it (phase 3.2) aren't needed.
What the open chat holds is still unexamined:

- b33p-core keeps every history page it has read; nothing trims the timeline. A cap on rows kept,
  or dropping old pages when scrolled away, as sigwin does.
- Pictures only for the open chat, decoded at the size shown, and dropped when the window has
  been hidden a while (sigwin `gui/window.rs`).

Measure first: the `mem:` line (`b33p-slint/src/memory.rs`) on a long chat scrolled through.

### 3. Owner's calls

| Decision | Unblocks |
|---|---|
| A Windows 11 evaluation VM in the testbox (`dockur/windows` on KVM; a ~6 GB image that expires after 90 days) | D.1, and through it the checks below |
| Code signing (SignPath's free plan for open source, or a paid certificate) | D.6, SmartScreen's warning |
| The minisign key in the repo's secrets (`B33P_MINISIGN_KEY`; without it `release.yml` skips signing) | D.5's updates actually offered |
| Enter in the middle of an IME syllable: today one press commits *and sends* (`@가` went out); Chromium and Signal-Desktop spend it on the syllable. Matching that needs to know the commit and the Enter were one key press, which Qt's fcitx plugin hides (it commits before delivering Return) | the composer's Enter |

### 4. Waiting for a Windows machine (D.1)

- **0.3 CJK with a Korean locale**: on en-US Windows femtovg shows 駅 鉄 国 and Hangul; sigwin's
  blanks came from a Korean locale picking Malgun Gothic for every ideograph.
- **D.7 the file dialog in a child process** (sigwin `picker.rs`): the shell, thumbnail and
  cloud-sync DLLs add 28 MB while open. Only if a measurement on real Windows shows it.
- **D.8 to check, not port**: pasting a picture from the Windows clipboard (`clipboard.rs`); a
  toast click opening the chat (sigwin `4e3bbee`); the window after restoring from minimized.
- femtovg's memory with a real GPU driver: sigwin measured 212 MB with the driver in the process;
  the CI runner has none.

## Done

| Item | Where |
|---|---|
| Global `SESSION_LOCK` in libsignal-service (`a2dc65d`) | lss #488 / presage #473, in the integration branches |
| Placeholder previews against presage's link fetch (`42327d6`) | presage #478 `set_automatic_link_previews` |
| 16 group sends at once | sender keys (fork) |
| Read syncs with a binary sender (`e9ebf2e`) | `receive.rs` reads `*_aci_binary` |
| Delivery receipts, jump-to-latest with a count, drafts, Esc cancels a reply, typing indicator, autostart, single instance, quote author | present before the plan |
| 0.4 Mentions while composing with an IME | #135 (checked with fcitx5 Hangul, B.4) |
| 1.1 Click a quote to jump to it | #136 |
| 2.1 Pinned messages, 1.7 the pin bar follows edits | #137, `state/pins.rs` |
| 1.4 Long messages fold behind "Read more" | #138 |
| 1.3, 1.6 receipts per recipient (core) | #139 |
| A.1–A.3 Windows CI, a smoke run with a screenshot, the `mem:` line | #141, `.github/workflows/windows.yml`, `memory.rs` |
| B.2 Notifications per chat: mentions only, no text, no sound | #142 |
| B.3 Image viewer, an overlay in the window | #145 |
| D.2 Installing per user from the zip | #146, `install.rs` |
| D.3 Unread count on the tray and the taskbar button | #144, `badge.rs` |
| D.4 Ctrl shortcuts during a Korean IME composition | #143, `keys.rs` |
| D.5 Signed self-update, off by default | #147, `update.rs`, `release.yml` |
| 1.2 Recent times as elapsed ("now", "5 min") | #182 |
| 1.3 Readers' pictures under a group message | #184 |
| 1.6 Message info, a dialog | #186 |
| 2.3 Albums as a grid | #187 |
| 2.4 Animated stickers | #189 |
| Screen-capture exclusion on Windows (found by #144: it asked before the window existed) | #148 |

## Findings worth keeping

- **0.1 Slint ListView at the end of its model** (sigwin `vendor/i-slint-core/model/repeater.rs:404`):
  not reproduced. Case tests (rows removed at the end, view grown, a jump past the end, a new
  model) pass on Slint master `6a4ef80` and v1.18.1. The `content_y_value += …` in that branch is
  dead code; sigwin's patch seems to make the layout converge in one pass, which its first-frame
  `seek-end` needs. No upstream PR without a failing test.
- **0.2 Caps Lock in Ctrl shortcuts**: fixed upstream (`InternalKeyEvent::shortcut` compares in
  lower case); b33p's own handlers use `eq_ignore_ascii_case`.
- **0.5 Preview `url`**: Signal iOS drops a whole message whose `Preview` has no `url`. Holds by
  construction: the only place building a `Preview` sets it (`send.rs`), and forwards and edits
  keep a preview only with its `url` in the text (`commands.rs`).
- **0.6 Attachments at rest**: downloads stay in memory; only Save (to Downloads) and Open (a
  `0600` copy in a private scratch directory, cleaned) write files.
- **The first Windows CI run found main broken there** (#141): `windows_registry::Error` had become
  private (no Windows build had compiled since 9b642ac), a migrations test compared CRLF text, and
  two Unix-only variants were unused. The job now treats warnings as errors.
- **Emoji on Linux** (black ❤, blurry ZWJ sequences, from the 6 Nov sweep) is a different path from
  sigwin's COLR renderer work; don't merge the two.
- **Code that links C (BoringSSL, aws-lc, ring) can't be checked for Windows from Linux** without
  Microsoft's SDK; the Windows CI job is the check.
