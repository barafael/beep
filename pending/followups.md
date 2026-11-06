# Follow-ups left from the UI review

What was left of docs/review-plan.md when its worktree went (2026-11-06); the full note is in
`beep-records/tidy-2026-11-06/remaining-work.md`.

## The thread index and batched saves (presage)

Branches `pr/thread-messages-index`, `pr/batch-save-messages` and `langis/integration-next` of
barafael/presage, rebased 2026-11-06. Measured on 300k messages in 300 threads: latest per thread
1.16 s to 0.01 s, unread counts 2.35 s to 0.01 s; importing 5,000 messages 7.9 s to 0.19 s.

Once they land, or to take them from the fork first:

1. Bump the presage rev (`presage` and `presage-store-sqlite`).
2. Append the migration to `b33p-core/tests/presage-migrations.txt`.
3. Drop b33p's own `b33p_thread_messages_thread_ts` (`LocalStore::index_messages`).
4. Switch the history import (`import.rs`, `transfer.rs`) to `save_messages`, about 1,000 at a
   time. This is what makes the import fast.

## b33p

- The chat pane is still inline in `app.slint`; it could be a component like `ChatListPane`.
- A keystroke with 5,000 messages selected takes about 1.4 ms against a 1 ms target: the three
  selection queries each look every picked message up. Keeping counts on pick and unpick fixes it.
- `storage_sync` could retry an `Unreachable` write later rather than wait for the next change.
- `RateLimitExceeded` carries `retry_after`, which could tell the user when to try.
- Regenerate the README screenshots and the demo GIF on a real desktop
  (`docs/screenshots/shoot.py`).
