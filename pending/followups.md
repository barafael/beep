# Follow-ups left from the UI review

What was left of docs/review-plan.md when its worktree went (2026-11-06); the full note is in
`../beep-records/tidy-2026-11-06/remaining-work.md` (not in the repository).

## Batched saves for the history import (presage)

The thread index went another way: b33p makes its own, `b33p_thread_messages_thread_ts_sender`
(`LocalStore::index_messages`), and presage's fork finds a thread's messages through it (no PR
yet, `Cargo.toml`); upstream #502 replaced our paged reads. What is left is
`ContentsStore::save_messages` (branch `pr/batch-save-messages` of barafael/presage, 2026-11-06,
not in `langis/integration`): importing 5,000 messages took 7.9 s one by one and 0.19 s
batched. To take it: rebase it onto the integration branch, append its migration (if any) to
`b33p-core/tests/presage-migrations.txt`, and switch the history import (`import.rs`,
`transfer.rs`) to `save_messages`, about 1,000 at a time.

## The real backend while catching up

Carried over from docs/performance-open-questions.md (Q4, removed with the GPUI client in #178).
The catch-up measurements used the mock. Reading `signal/receive.rs` (`handle_content`) suggests
6 to 8 small store calls per received message; only the trust refresh that read a whole table per
message was removed. Count them: a counting layer around the store in a debug build, while an
account receives a backlog of a few thousand messages (in the testbox), giving calls per message
and time per call. That says whether one transaction per batch is worth it.

## b33p

- The chat pane is still inline in `app.slint`; it could be a component like `ChatListPane`.
- A keystroke with 5,000 messages selected takes about 1.4 ms against a 1 ms target: the three
  selection queries each look every picked message up. Keeping counts on pick and unpick fixes it.
- `storage_sync` could retry an `Unreachable` write later rather than wait for the next change.
- `RateLimitExceeded` carries `retry_after`, which could tell the user when to try.
- An upload copies its data before the first try, retry or not (`attachments::upload_retrying`,
  #164: `upload_attachment(first, data.clone())`), so every upload holds its file twice for its
  length, files sent together each twice at once. Only the retry, on the CDN closing the
  connection under the request, needs the second copy. Avoidable if presage's
  `upload_attachment` took shared bytes (`Bytes` or `Arc<[u8]>`) instead of a `Vec<u8>`, or for
  files by reading the file again for the retry. Pictures arriving together once took b33p past
  2 GiB (pending/stress.md finding 4), so the peak matters.
- `ChatMessage.quote` and `.preview` are unboxed `Option`s most messages leave empty
  (`b33p-core/src/lib.rs`). Boxing `LinkPreview.image` made a message about 25% smaller (a
  200,000-message backlog 330 to 290 MB). Box both, run the mock's catch-up with
  `B33P_MOCK_SCALE` backlog 200000 and keep it if it saves more than a few MB (Q5 of the removed
  performance-open-questions.md).
- Regenerate the README screenshots and the demo GIF on a real desktop
  (`docs/screenshots/shoot.py`).
