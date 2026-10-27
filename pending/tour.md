# Tour: follow-ups, observations, notes

The testbox's `tour` phase runs several b33ps in one pod, each linked to a presage-cli primary of
its own. They write to each other through every feature b33p has, in an order drawn from a seed,
and after each step every b33p it reached is checked against a model of the chats. The work is on
branch `tour` (worktree `~/code/beep-tour`).

This file is kept as the runs go. Newest entries go at the end of each section.

## Findings in b33p and its forks

1. **presage fetches link previews whatever b33p's setting says.** This comes from upstream
   presage#359 (30ca664), which `langis/integration` carries and b33p main pins.
   - `Manager::send_message` and `send_message_to_group` call `fetch_link_previews` for every
     data message without a preview (presage/src/manager/registered.rs:1273, :2110). There is
     no way to turn it off.
   - b33p builds previews itself only with `link_previews` on, and that setting is off by
     default. With it off, presage still fetches the first https link of every message the
     user sends, from the user's machine.
   - merge-prs-full-run found a second case: with the setting on, when b33p's own preview finds
     nothing or fails, presage fetches the page again.
   - merge-prs-full-run took the fix:
     - whisperfish/presage#478 adds `Manager::set_automatic_link_previews(bool)`. It is on by
       default, and a manager's clones share it.
     - presage `langis/integration` is at bf7cfd0, with #478 on top.
     - b33p commit 1eacb7c on `forks-454` turns it off at the start of `run_linked`. It isn't
       pushed yet; it waits on #82.
   - **To do:** a tour step that sends an https link with `link_previews` off and checks that
     the sender's log shows no preview attempt. It should fail on main until 1eacb7c lands, and
     pass on `forks-454`. With `ca-certificates` in the image, presage's attempt now gets as
     far as DNS, which fails without egress, and logs "failed to build link preview".
2. **No HTTP client builds in the testbox.** The "builder error" on every send is
   "No CA certificates were loaded from the system" (rustls-platform-verifier 0.7.1, others.rs:104).
   - The b33p image had no `ca-certificates`, so neither b33p's preview client nor presage's
     ever built in the box, and link previews were untestable there.
   - Fixed on `tour`: the image installs `ca-certificates`. The fetch now gets as far as DNS,
     which fails without egress.
   - Small presage point: a client that failed to build isn't cached, so presage tries to build
     it again on every send.
3. **A group's details page doesn't update while open.** b33p-core's `Command::LoadDetails`
   reads the stored group once, when the page opens.
   - A member leaving, a removal or an admin change that arrives while the page is open isn't
     shown until it is opened again. Signal's apps update an open group page.
   - Small, and known in practice: `checks/groups.rs` `b33p_details` waits for the notice
     before opening. Worth an issue?
4. **b33p logs nothing when it edits a group.** No line marks a leave, removal or rename being
   sent or applied, at debug either. The box also logs presage at `info`, which hides
   `upsert_group`'s "fetching and saving group".
   - Finding out whether a group change was sent and fetched took the storage service's access
     log and envelope timestamps.
   - Suggestion: a debug line per group edit and per group update applied. The box could also
     log `presage=debug`.

5. **A message from someone just blocked can still show.** `ChatState::block` changes the UI's
   settings at once. The settings table that receiving checks (`receive.rs` `is_blocked`) is
   written only when the backend takes `Command::Block`, and that happens in turn after the
   non-sending commands ahead of it in `run_linked`'s loop.
   - A message that arrives in between is shown, although the chat already shows as blocked.
   - Seen in round 1 of run 4 (seed 1793097053, step 8): a message sent about 0.1 s after the
     block was shown.
   - Possible fixes:
     - the state drops an `Event::Message` for a chat it has as blocked;
     - or the table is written before the UI shows the block.
   - The tour now waits for the details to load after blocking. `LoadDetails` goes through the
     same loop, so its answer comes after the block is written.
6. **A request to join by link reaches no admin until they fetch the group.**
   `Manager::join_group` on the approval path (`AccessRequired::Administrator`) saves the
   request locally and sends nothing.
   - In run 4, round 3 (seed 1793097793), no one fetched the group after the request (storage
     log), and the admin never showed it.
   - Corrected by merge-prs-full-run from Signal-Desktop's code: for a **first-time**
     requester, this is what Desktop does too.
     - `requestToJoin` goes through `modifyGroupV2`, which sends the change to the members the
       requester has stored, and a first-time requester has none.
     - So admins learn at their next fetch, and the tour's case matches Desktop.
     - The groups phase shows the request at once only because the peer *rejoins*: Desktop
       still has the old member list and tells them.
   - **presage differs only on a rejoin.** It sends nothing, and it overwrites the stored group
     with `members: Vec::new()`.
     - Fixed by merge-prs-full-run: asking to join with approval, and cancelling, now send the
       signed change to the stored members and invitees, and keep them.
     - The fix: presage pr/groups 05bae67 (on whisperfish/presage#448), `langis/integration`
       76673f5, and b33p `forks-454` 940b6a9, which is local until #82's force-push.
     - **To do:** rebase `tour` onto `forks-454`. A rejoin with approval then has to reach the
       admin without a fetch, while a first-time request still reaches no one.
   - **Open:**
     - What should make an admin's b33p fetch the group, so that a first-time request shows up
       at all? Signal's apps fetch groups now and then. Does b33p ever do so without a message
       from the group?
     - The tour could check a rejoin with approval once presage's fix is in.
   - Until then, the tour joins links set to "anyone".
7. **A b33p admin never sees a first-time request to join** until some message from the group
   bumps its revision.
   - b33p fetches a group from the server only in two cases:
     - presage's `upsert_group`, on a message carrying a newer revision;
     - `groups::check_request`, for its own request.
   - `refresh_group` and `Command::LoadDetails` read only the store. Nothing fetches when a
     group's chat or details open, or now and then.
   - Suggestion: `LoadDetails` of a group where we are an admin fetches it first (`fetch_group`),
     as `check_request` does for a request of ours. That is when an admin looks for requests.
     Then the tour can check a first-time request with approval: the admin opens the details
     until it shows. To check against Signal-Android and Signal-Desktop: when do they fetch?

8. **An accepted message request can turn back into one.** This explains the "sent nothing"
   failures: run 2 step 16, and run 5 round 3 (seed 1793099053) step 3. Both times it was
   b33p 2's chat with b33p 1, the second request b33p 2 accepted.
   - **The sequence:**
     1. A new contact's first message makes a chat titled by their id.
     2. `conversations::retitle_once_saved` polls (0.25 s, 0.5 s, 1 s, …) until presage has
        saved the contact. Then `send_conversations` reloads every chat with settings read from
        the table.
     3. Accepting flips the UI at once (`ChatState::respond`). `accepted` is written to the
        table only in `send::respond`, in the in-order task, after the sends queued before it
        (here the first acceptance's profile-key update).
     4. A reload between the two reads `accepted: false`, and the UI takes it.
     5. `respond` then writes `true` without telling the UI. The chat stays a request, and
        nothing can be sent to it, until the next reload or a restart.
   - **Timeline, run 5 round 3:** the hello arrived at 11:04:11.6, the contact was saved at
     11:04:11.87, and the acceptances' sends ran from 11:04:11.6 to 11:04:12.3.
   - **For a user:** accepting a request soon after a new contact's first message can flip back.
     That is the usual moment to accept one.
   - **Possible fixes:**
     - `respond` sends the chat's settings once written, as `set_blocked` returns them;
     - or the table write happens in the main loop, and only the sends go in order;
     - or `send_conversations` keeps the settings of chats with a settings command still
       pending.
   - The tour's setup now waits for each chat's real title before accepting, which steps around
     it. A targeted check would accept right after a first message and check the request stays
     answered.
9. **Joining by a link that needs no approval fails at the server.** HTTP 400 `{"code":400,
   "message":"Invalid field set on action"}` comes back for presage's `join_group` on the
   `AccessRequired::Any` path (run 5, rounds 1 and 2, PATCH `/v2/groups/?inviteLinkPassword=…`).
   - Cause, confirmed by merge-prs-full-run: libsignal-service's
     `build_join_from_invite_link_action` (groups_v2/operations.rs:965) set
     `join_from_invite_link = true` on the `AddMemberAction`. The storage service
     (`GroupValidator.validateAddMember`) refuses an action that has it set, and sets it itself.
   - Fixed by merge-prs-full-run:
     - libsignal-service `groups-v2-changes` a725052, on #473, with a test that fails before
       the fix;
     - `langis/integration` d1d684e;
     - presage pr/groups 421083d;
     - b33p `forks-454` ac682da, a fast-forward of 940b6a9.
   - The tour, rebased onto ac682da, joins by link both ways: first-time by an open link, and as
     a rejoin with approval.

## Open questions

- **Answered (finding 8): step 16 of seed 1793096441 sent nothing once.** b33p 2's `Send` returned no command when it
  was to send a file in its chat with b33p 1.
  - The same seed passed that step in the next run.
  - b33p-core's `can_compose` was false. Its log shows no reconnect, so `synced` should have
    held. Storage can't undo an acceptance (`accepted: local || remote`). That leaves the
    selection, `left`, `trust` or `is_request`.
  - The record that would have said was overwritten (see the testbox notes). A failed send
    now reports the composer's view, and a failed step keeps every b33p's snapshot.
  - Root-caused in run 5: see finding 8.

## Testbox: observations and improvements

- **`cargo testbox test` reuses one record directory per box** (`target/testbox-record/mesh`),
  so the next `test` wipes the last one's logs. That cost the step 16 evidence. Use `run`,
  which records by start time, or let `test` keep the previous record.
- **The box lock doesn't span worktrees.** It is per worktree (`target/testbox.lock`), but
  the pod name `b33p-testbox` is global, so sessions in different worktrees still have to
  agree by message. The lock could live under `$XDG_RUNTIME_DIR`.
- **No harness snapshot was kept on a failure.** The tour now keeps every b33p's snapshot
  (`b33p-<k>.json`) when a step fails. The other phases could do the same per failed check.
  This is testbox-review's "Snapshots of the harness with the screenshots".
- **Metrics went only to stdout.** The tour writes `tour.json`: the seed, each step and its
  duration, per-kind median, p95 and max, how long a message took to show on the others,
  restart times, and memory, open files and snapshot latency per b33p every 30 s. A first step
  toward testbox-review's "A report per run".
- **There is no contact discovery in the box.** The mesh bootstraps by having each primary write
  first to every later one's account.
- **b33p previews its own messages as `You: <text>`.** The first mesh setup check missed that.
- **Things the harness can't show:**
  - In-chat search results (`SearchChat`/`ShowResult`) aren't in the snapshot, so only the
    chat-list search is checked.
  - Trust and safety-number state isn't in the snapshot, so a safety-number change can't be
    checked.
- **Steps not in the tour yet:**
  - Stickers: the pack has to be seeded per primary, then CDN0 has to be waited for.
  - Voice notes: b33p can't record one.
  - Invitations: everyone in the mesh knows everyone's profile key, so adds are direct.
  - Link previews: possible now that the image has CA certificates, but they need a local page
    to preview.
  - Editing a picture's caption.
- **Noise in every b33p log:** cage's "Cannot find Xwayland binary", since the image removes
  Xwayland on purpose, and Qt's "Detected locale C …" on every start.
- **`kill` and `start` print the container's name** (podman's output) into the run's stdout.
- **A stopped run leaves no record.** `Record::keep_logs` runs at the box's end, so a run that is
  interrupted (Ctrl-C, a livelock) keeps nothing unless the logs are saved by hand first.
- **`down --wipe` removes volumes by the prefix `b33p-testbox-`.** That would also take another
  instance's volumes. merge-prs-full-run's parallel-instances branch names instances
  `b33p-testbox.NAME` (with a dot) so the old filter misses them, and wipes by name.

- **A group's stories are filed under the group,** not under their author
  (`stories::on_story`). The first story step opened the author's chat, which was my mistake
  (run 4, round 2).

## Notes on the tour itself

- **Seeds and order.** Each kind of step comes up `TESTBOX_TOUR_REPEAT` times (2), in a
  shuffled order filtered by what can go. A seed replays the same steps: runs 2 and 3 drew the
  same 28.
- **The model.** Each b33p is held to a chat's messages from when it last joined, and notices
  aren't compared.
- **The first runs:**
  - Run 1 failed in setup on the `You:` preview, a mistake in my check.
  - Run 2 failed at step 16, the open question above.
  - Run 3 failed at step 28, the details race. That was a mistake in my check, now fixed by
    opening the page again.
  - Steps 1–27 passed in run 3: disappearing messages, kills, groups made, pictures, files,
    audio, forwards, reactions, blocking, search, deletes for oneself, a removal, a rename,
    read receipts, edits and replies.
- **Run 4** (`run --rounds 3`, records in `target/testbox-record/run/1793096978/`):
  - round 1 failed at step 8, the block race (finding 5);
  - round 2 failed at step 5, my story mistake;
  - round 3 failed at step 5, presage not telling the admin (finding 6).

  All three are fixed or worked around in the tour.
- **Run 5** (records in `target/testbox-record/run/1793098152/`):
  - rounds 1 and 2 failed at the join by link (finding 9);
  - round 3 failed at step 3, the accepted request turned back (finding 8).

  `link-without-preview` passed every time, falsely. Its link was `example.com`, which presage
  drops quietly as not previewable (`RESERVED_DOMAINS`). It now uses `b33p-tour.org`.
- **A new step, `link-without-preview`,** sends an https link with previews off. It is recorded
  apart from the step, so its failure doesn't end the tour.
  - It is expected to fail on main, where presage looks the link up, and to pass once
    `forks-454` (1eacb7c) is in.
- **Run 6** (rebased onto `forks-454` 940b6a9) livelocked in round 1, a bug in the tour.
  - After step 55, every kind left in the pool needed something no remaining step would make.
    A rejoin needs someone who left, and no Leave was left to draw.
  - So it drew filler texts for an hour: 4,000 steps, all passing.
  - Stopped by hand. Its container logs are kept in the scratchpad (`run6-logs/`), since a
    stopped run writes no record.
  - Up to step 55 everything passed, including a rejoin by link with approval (finding 6's
    fix) and the link sent with previews off (finding 1's fix, now with a link presage does
    try).
  - Fixed: when stuck, the tour draws a step that may unstick it (a leave, an add, a new group),
    and fails after eight in a row, naming the kinds that never could go.
