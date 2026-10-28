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
   - **Correction (run 7):** the observed failures don't show this. Run 7, round 1 (seed
     1793102501, step 7) failed the same way with that wait in place.
     - The block step unblocked as soon as the server had taken the blocked message, and `by`
       can receive it after that. A message taken after the unblock shows, rightly.
     - The race in the code is real, but no failure so far needs it. It is unconfirmed until a
       check shows it.
     - The step now unblocks only once a third b33p's later message has reached `by`. The server
       hands a device its messages in turn, and a blocked sender gets no receipt to wait for.
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
   - A rejoin isn't enough either. Run 18, round 2 (seed 1793157181, step 51): b33p 2 asked to
     rejoin, and b33p 1 was to approve. b33p 2 had left at step 32, when b33p 1 wasn't a
     member, so it told only b33p 0 (`send_message_to_group` with 1 recipient). b33p 1 never
     saw the request. The tour's rule that approval joins are rejoins assumes the requester
     knows the admin; it doesn't hold after the member list changed.

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
   - **A second trigger, for groups** (run 10, round 3, seed 1793106393, step 11).
     - b33p 1 was added to a group and accepted it; the tour saw it accepted.
     - Group updates then came in. Each calls `groups::refresh_group`, which rebuilds the chat
       with `first_settings`, read from the table. `respond` hadn't written the table yet.
     - The group turned back into a request (`accepted: false`), and its stories were hidden
       (`story_shown` hides a request's). The story step failed, with nothing in the log.
     - Any reload that reads the table can do this, not only `retitle_once_saved`.
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

10. **A rejoined member keeps a group picture removed while it was away.** Seen in run 7,
    round 2 (seed 1793102699, step 42).
    - presage `join_group`'s request path stores the group from the link's info, with
      `avatar: info.avatar`, which is empty once the picture is gone. It touches the cached
      picture only when `info.avatar` isn't empty.
    - Every later `fetch_group` then sees the stored path `""` equal to the server's and keeps
      the old bytes.
    - Fix: clear the cached picture when `info.avatar` is empty, as `fetch_group` does for "none
      any more".
    - Fixed by merge-prs-full-run: presage pr/groups 1873f8b (on #448), `langis/integration`
      45b2365. b33p's pin moves in a small PR after #82.
    - Replayed seed 1793102699 on `tour` with #85 (presage 45b2365). It drew the same steps,
      and step 42 failed the same way.
      - What the check sees is b33p's UI, finding 11. b33p 0 restarted on that box then showed
        no picture, so presage's store is right with #85.
      - Without #85 the restart wasn't tried. That presage's store kept the old picture rests on
        reading the code.
      - So there are two layers: presage's store (fixed in #85) and b33p's UI (finding 11, not
        fixed).

11. **A member added back doesn't show a group picture set while it was away**, until b33p
    restarts. Seen in run 8, round 3 (seed 1793104113, step 14): b33p 0 left, the admin set
    a picture, then added b33p 0 back.
    - presage fetched the group and downloaded the picture (CDN0 GET at 12:29:02).
    - b33p sends a group's picture to the UI only when the change it receives has a picture
      change in it (`receive.rs:633-643`), and being added has none.
    - Fix: send the picture when the stored group's picture differs from what the UI has, or
      whenever we join or are added.
    - It covers a rejoin by link too, and a picture removed as well as set: the UI keeps what
      it showed before (run 7 round 2, and its replay).
    - The tour checks pictures strictly, except for a member that joined after the picture last
      changed. That case is a check of its own, "who came back shows the group's picture", which
      fails without ending the tour.

12. **After a reload, a reply's quote shows text the quoted message got only later.** Run 10,
    round 2 (seed 1793106083, step 29): b33p 2 replied to its own message, edited that message
    later, and was restarted.
    - Live, the quote shows the text it quoted. After the restart it shows the edit.
    - `history.rs` (around :506) looks the quoted message up by `quote.id` with
      `message_sent_or_edited_at`. presage keeps an edit in place under the original timestamp,
      so a lookup by the original timestamp returns the latest version.
    - The comment there means "the version it quotes".
    - Fix in b33p: use the stored body only if its version is the one quoted (`edited_at()`, or
      its own timestamp if never edited, equals `quote.id`). Otherwise use `quote.text`, which
      the quoting sender wrote.
13. **One failed group fetch leaves a group stale until its next message.** Run 10, round 1
    (seed 1793105499, step 56).
    - b33p 1 got b33p 2's leave. presage's `upsert_group` then failed to fetch the group: "HTTP
      reqwest error: error sending request for url".
    - It only warns, keeps the old revision, and nothing fetches again. The admin listed the
      leaver for good.
    - The transport error happened once in all the runs. Its cause is unknown, because the log
      line (`%error`) drops the error's source.
      - A guess: reqwest reused a pooled connection (kept 90 s) that the storage service had
        closed after its idle timeout. Jetty's default is 30 s, and the box sets none.
    - Suggestions:
      - presage retries a failed group fetch, or keeps the group marked to fetch again;
      - libsignal-service retries an idempotent GET once on a reused connection;
      - logs print an error's whole chain.

14. **Under heavy disk writes, presage-cli's send died with "database is locked".** Both tour
    rounds of the log experiment failed this way.
    - Under that load each `INSERT INTO pre_keys` took 1–2 s, about 200 of them, each its own
      transaction with an fsync. The other connections waited out sqlx's default 5 s busy
      timeout.
    - Fixed by merge-prs-full-run in #92 (merged): libsignal-service#491 and presage#483 save a
      replenishment in one transaction, with a 30 s busy timeout. Two rounds under the same load
      then passed (73 and 70 checks).
15. **A group from an accepted contact was a message request in b33p.** Signal-Desktop's
    `isConversationAccepted` accepts a group whose `addedBy` is a trusted contact (system
    contact or profile sharing), so it shows no request.
    - The tour's `converge_group` had been answering those requests itself, which hid this.
    - Fixed in item 2's PR: who brought us in is the editor of the change that lets us in, or the
      sender of a new group's announcement. When that person's chat is accepted, so is the group.
    - With it, a chat written to from another device of ours is accepted in the table too, as
      writing from b33p does (`conversations::wrote_to`). Before, it was accepted only in the
      window (`written_to`), so the backend withheld receipts from it as from a request.
16. **presage-cli's send waited 30 s for the database, then failed with "database is locked",
    with no load** (item 2's run, round 3, setup, at 2026-10-27 23:16:21).
    - The background receive logged "saved contact on first sight". Then nothing happened until
      a pool connection was acquired after 29.97 s (sqlx's acquire timeout), and the send's
      `UPDATE pinned_identities SET offered = NULL …` got `SQLITE_BUSY`.
    - So something held the write lock and the store's three connections for 30 s: a lock-order
      deadlock between the receive transaction (session step, then transaction) and the send
      path, rather than a slow disk.
    - The 5 s timeout before #92 would have ended the same deadlock sooner; the two "database
      is locked" failures under load may have been this one too.
    - It is once in about 40 rounds without load. Investigation: next.
    - Not seen in 15 tour rounds with presage-cli at debug under 8 fsync writers (f16 run,
      2026-10-28): no "database is locked" at all. The logs show what held the connections:
      two writes each waited the full 30 s busy timeout on SQLite's write lock, and a third
      waited 30 s for a connection. So the pool's third connection held the write lock over
      30 s without committing. Every store transaction read so far commits before awaiting anything else;
      still open.
17. **The box's server gave one one-time pre-key to two senders** (a 40-round setup run for
    finding 16, round 23). b33p 0 failed to decrypt a sealed-sender pre-key message ("no rows
    returned").
    - b33p 1 and b33p 2 fetched b33p 0's bundle 0.35 ms apart, and both got one-time pre-key 2.
      The first session used it up, so the second one's message found no key.
    - Fetching one account's bundle from two accounts at once got the same one-time key in 3 of
      6 trials.
    - Signal-Server's `SingleUseECPreKeyStore.take` queries for the first key, then deletes it
      with `ReturnValues: ALL_OLD`; whoever gets the old item back has the key. That relies on
      an atomic DeleteItem. In DynamoDB Local, of 8 simultaneous deletes of one item, more than
      one got it back for 40 of 40 items. With `ConditionExpression: attribute_exists(k)` it was
      0 of 40. The KEM store's conditional UpdateItem counter
      (`PagedSingleUseKEMPreKeyStore.take`) gave no index twice either, 0 of 40.
    - Real DynamoDB is atomic here, so this is the box's. Fixed in the box: the server is built
      with testbox/patches/signal-server-atomic-take.patch, a conditional delete in `take`.
    - With the patch (run 18): two accounts fetching b33p 0's bundle at once got different
      one-time keys in 6 of 6 trials (before, the same key in 3 of 6). The server's rate limit
      ends each such test after about 6 trials. Three tour rounds: two passed, and one failed
      on finding 7.
18. **b33p doesn't ask for a message it couldn't decrypt to be sent again.** Seen through
    finding 17: b33p 0 lost b33p 2's first message for good.
    - presage hands b33p `Received::DecryptionError`. b33p resets the session, at most hourly,
      and only with someone it has a chat with, and it shows a notice
      (b33p-core receive.rs, `Received::DecryptionError`).
    - It never sends a retry receipt (`DecryptionErrorMessage`). The Signal apps send one, and
      the sender resends the message on a fresh session. b33p already answers retry receipts
      (`on_retry_receipt`), but it doesn't send them.
    - So a first message that fails, from someone b33p has no chat with yet, is gone with no
      reset and no resend.
19. **An upload that got no answer counted as refused.** f16 run of 2026-10-28, round 1,
    step 39, under 8 fsync writers. b33p 0's upload to the box's tus server got no answer within
    the client's wait (77 s after the upload form), and b33p marked the photo `Refused`, which
    says sending it again won't work. libsignal-service's `ServiceError::Http` wasn't among the
    transient errors. Fixed in #97: connect, request and timeout errors are `Unreachable`, so
    the message shows as failed, to send again. Under that load the tus stand-in took seconds per
    upload (its last logged POST 12.5 s); without the load it doesn't come up.
20. **A first request to join with approval failed on the requester**: "couldn't join the group:
    … FOREIGN KEY constraint failed". presage's `join_group` saved the group's picture before the
    group, and `group_avatars` refers to `groups`; only a rejoin had the group stored already.
    The tour drew only rejoins until finding 7's branch. Fixed in presage pr/groups 59d958b,
    langis/integration 881688c, taken with finding 7's PR.
21. **A forwarded sticker arrived as a picture.** b33p forwarded a sticker's picture as an image
    attachment, with no pack (`Command::Forward` sent `sticker: None`). Signal-Desktop forwards
    the original's sticker (`maybeForwardMessages`). Found by the tour's sticker step (#102),
    seed 1793169709, step 30; fixed in #101, and the seed passes with it.
22. **A forwarded link lost its preview.** b33p's forward sent `preview: None`; Signal-Desktop
    forwards the original's previews (`maybeForwardMessages`, `draft.previews`). Fixed in #105,
    from the code: no tour run has drawn a forward of a message with a preview yet.

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

- **Images could hold another worktree's build** (fixed on main, f4976cd). The cargo target
  directories in the Containerfiles are cache mounts, which every worktree and every box share.
  - cargo goes by modification times: a tree whose sources are older than another tree's last
    build into the cache got that tree's artifacts. b33p-testbox.f4-b33p had none of f4's log
    lines in its binary.
  - Two builds at once wrote one target directory and each copied out what was there last: the
    f3 and f4 images, built together, held the same b33p.
  - Now the mounts are `sharing=locked` (two concurrent builds take turns, tested), and each build
    touches the sources it copied. Built at once again, with f4's files all dated back, f4's image
    held its own code, and f3's the same binary as before (dev builds are reproducible here).
  - Earlier runs from different worktrees may have tested another tree's b33p. The PRs of
    2026-10-28 (#94, #95, findings 3 and 7) were run again after the fix.

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
- **Things the harness can't show** (in #99: trust, safety numbers and in-chat search, with the
  tour's ChatSearch and Verify steps):
  - In-chat search results (`SearchChat`/`ShowResult`) aren't in the snapshot, so only the
    chat-list search is checked.
  - Trust and safety-number state isn't in the snapshot, so a safety-number change can't be
    checked.
- **Steps not in the tour yet** (caption edits in #100, stickers in #102; a nightly run on main
  in testbox/nightly):
  - Stickers: the pack has to be seeded per primary, then CDN0 has to be waited for.
  - Voice notes: b33p can't record one.
  - Invitations: everyone in the mesh knows everyone's profile key, so adds are direct.
  - Link previews: in #105, with a page of the box's own (testbox/tools `preview`) at a public
    address on the pod's loopback, which b33p's previews fetch from unchanged.
  - Editing a picture's caption.
- **Noise in every b33p log:** cage's "Cannot find Xwayland binary", since the image removes
  Xwayland on purpose, and Qt's "Detected locale C …" on every start.
- **Fixed: `kill` and `start` printed the container's name** (podman's output) into the run's
  stdout. `stop`, `start`, `kill`, `pause`, `unpause` and `update` now log it at debug, as
  `restart` did.
- **Memory samples counted the page cache.** The soak's and the tour's samples read cgroup
  `memory.current`, which includes the page cache: the files read and written, and what a
  restarted b33p reads again. So "memory grew" could mean nothing.
  - Run 13, round 3: b33p 1's 168 MiB was 67 MiB anon and 93 MiB file.
  - Now the samples are `memory.stat`'s `anon` (what the processes hold, what a leak grows),
    with `file` recorded beside it (`cache_mib` in `tour.json`).
  - Then b33p's own `RssAnon` (/proc/<pid>/status): the cgroup's `anon` counts wayvnc too,
    some 150 MiB while the screenshots connect over VNC, which failed the tour's "nothing keeps
    growing" with `--screenshots`.
  - The soak's earlier numbers (53 to 68/77 MiB) were `memory.current`.
  - The soak again on anon (2026-10-28, seed 1793160337, optimized, 30 minutes, beside other
    boxes and disk-write load): 55 to 82 MiB anon, with 19 to 40 MiB of page cache beside it,
    47 open files, snapshots within 106 ms. Most of the rise is in the first ten minutes; the
    last twenty stayed between 68 and 82 MiB. The check found nothing that keeps growing.
- **Signal-Server can start before the Redis cluster has formed, and then fail requests.**
  - Run 17, round 1: the redis nodes came up 35 s apart on a loaded host
    (`redis-1` 16:05:51, `redis-0` 16:06:22, `redis-2` 16:06:25).
  - The server's last start (16:06:23) found the cluster still forming. It said it served all
    the same, and its cluster client stayed "Currently not connected" well past 16:07:20.
  - A registration at 16:07:17 got HTTP 500 (error ID 2ffbc094e2798511, `RedisException`).
  - `provision` assumed a missing store fails the server, so the pod would start it again. For
    the cluster it doesn't.
  - The fix (branch `testbox-redis-cluster-ready`): `provision` asks `redis-0` for `CLUSTER
    INFO` and fails unless it says `cluster_state:ok`, as it does for the pre-key bucket.
- **A box's record can miss the end of a container's log.** In run 17, the server's log reached
  journald (the containers' log driver) up to 41 s late on a loaded host: the 500's ERROR line,
  logged at 16:07:20, arrived at 16:08:01. The record, taken at the box's end, stopped at
  16:06:48.
  - The whole log was in `journalctl --user` (by container name).
  - `Record::keep_logs` could wait for the log to catch up, or read journald directly.
- **Every build needs the network for Desktop's `ADD https://registry.npmjs.org/pnpm/…`.**
  podman fetches an `ADD <url>` again on each build, checksum or not, cached layer or not.
  - A Wi-Fi roam at 17:29:32 (DHCP restarted) failed a build with "Temporary failure in name
    resolution".
  - The tour builds Desktop's image too, as `checks::run` builds the linked box's images
    whatever the phases.
  - Improvements: keep the tarball in the repository or a build cache by its checksum, and build
    only the images the chosen phases use.
- **`podman run --pod <box>` inherits the pod's `restartPolicy: Always`.** A one-off container
  started that way is restarted after every exit unless given `--restart=no`. merge-prs-full-run
  drew a wrong conclusion from this before noticing.
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
- **Run 7**, as `--instance tour` from local `tour-on-main` (tour merged with main, for #84),
  beside #82's run on the default box. Records are in
  `target/testbox-tour-record/run/1793102422/`.
  - Round 1 failed at the block step: the delivery race in the step (see the correction to
    finding 5).
  - Round 2 failed at step 42 (finding 10).
  - **Round 3 passed: the first tour with every step green.**

- **Run 8**, as `--instance tour`, records in `target/testbox-tour-record/run/1793103275/`:
  - **rounds 1 and 2 passed;**
  - round 3 failed at step 14 (finding 11).
- **Run 10**: `tour` with #85, three rounds, records in
  `target/testbox-tour-record/run/1793105144/`. All three rounds failed, on findings 13, 12 and
  8 (groups).
- **Where the tour stands:** three rounds fully green so far (run 7 round 3, run 8 rounds 1 and
  2). The rounds that fail now fail on b33p and presage bugs (8, 11, 12, 13), not on the tour.
  Finding 8 is the most frequent, and b33p's own to fix.
- **Fixes, by the user's decision:**
  - finding 8 is barrafael/beep#86, merged;
  - finding 12 is #87, merged;
  - findings 11 and 13 are merge-prs-full-run's: 11 in #85, 13 to be reproduced as
    `--instance repro`, then the error chain logged in full, the pool idle timeout set below
    the server's, and the group fetch retried.
- **Finding 13's timing:** b33p 1 last fetched the group at about 12:54:09, and the failing fetch
  was at 12:54:40.18. The connection was idle about 31 s, at Jetty's 30 s default. That fits a
  pooled connection reused after the server closed it.
- **Run 11**: `tour` plus #86, without the setup's wait for titles (finding 8's trigger back on).
  3 of 3 rounds passed, 68 checks each; records in `target/testbox-tour-record/run/1793107440/`.
  - A message showed on the other b33ps in 0.2–0.3 s median, 2 s at most.
  - A killed b33p was back with its chats in 2–3 s. Linking took 2–4 s.
  - Each b33p used 90–145 MiB with about 50 open files, and no snapshot took over 30 ms.
  - Slowest steps (median): disappearing 10.4 s (the 10 s timer), join-by-link 7.9 s,
    story 5.3 s, make-admin 5.0 s, create-group 4.6 s, leave 4.2 s.
- **A new step, `quote-then-edit`:** a reply to one's own message, that message edited, then a
  restart. It is #87's check: it should fail on main, where finding 12 shows, and pass with #87.
- **Run 12**, `quote-then-edit` without and with the fix:
  - On #83's branch alone (seed 1793108413), step 22 failed as finding 12 says: after the
    restart, the quote showed "tour 15: edited by 0 …" instead of "tour 9: from 0 …".
  - On #83 plus #86 and #87, 3 rounds, 70 checks each: every `quote-then-edit` passed.
  - Round 2 failed only the recorded check "who came back shows the group's picture" (finding
    11). That branch lacked #85, where it is fixed.
- **All merged into main** on the user's word: #85 (17e8a9e), #83 (03f4461), #86 (8705be1),
  #87 (6ef429d). Main's CI passed on 03f4461.
- **Run 13, on main at 03f4461** (all of it merged), 3 rounds, records in
  `target/testbox-tour-record/run/1793110205/`:
  - **round 1 passed**, 70 checks;
  - round 2 failed at step 47, a description b33p 1 never showed: finding 13 for the second
    time, a group fetch failing on "error sending request" at 14:21:47.9;
  - round 3 failed "who came back shows the group's picture" four times. That is finding 11's
    fix missing a case, call it 11b: b33p 1 joined a group for the first time by an open
    link, after its picture was set. #85 sends the picture when a received change lets us
    in, but a join by an open link is the joiner's own change (`Command::JoinGroup`, which
    only calls `refresh_group`). Reported to merge-prs-full-run.
  - Round 3 also failed "b33p 1's memory … didn't keep growing" (95 to 168 MiB). That was the
    page cache, not a leak: 67 MiB anon and 93 MiB file, after b33p 1 was killed and started
    again. A measurement flaw (see the testbox notes).
- **Finding 13 reproduced and fixed by merge-prs-full-run.**
  - The box's storage service closes an idle connection after 30.03 s, and reqwest keeps one
    90 s.
  - Requests sent at about 30 s idle: 5 of 1210 failed with "connection closed before message
    completed". With a 20 s pool idle timeout, 0 failed; with one GET retry, 0 failed.
  - The fixes:
    - libsignal-service c0cb9bf: idle connections are dropped after 20 s, and a GET is retried
      once;
    - presage fcbdf70: a failed group fetch is tried once more, and its causes are logged;
    - beep branch `stale-connections` pins them.
- **Finding 13 a third time** (run 14, on #88, seed 1793111805): join_group's
  `GET /v1/groups/join/…` sent on a connection the storage service had closed, so the join
  failed.
- **11b fixed in #89** (merge-prs-full-run): `Command::JoinGroup` sends the group's picture
  after a join or a request. Run 15 replayed seed 1793111125 on #89: 70 of 70 passed, step 46
  included, with no "who came back" failure (4 on main).
- **Finding 13 fixed in #90** (merge-prs-full-run): presage a1f75aa, libsignal-service 33f271d.
- **Run 16: main with #88, #89 and #90 merged**, 3 rounds, all passed (70, 71, 70 checks).
  Records are in `target/testbox-tour-record/run/1793112508/`.
  - There is no transport error left. The 12 group-fetch warnings are all "Authorization
    failed", for a b33p no longer in the group, as they should be. #90 retries those once,
    pointlessly but harmlessly.
  - A message showed on the other b33ps in 0.20–0.32 s median and 0.41–0.63 s at the 95th
    percentile; the most was 5.5 s, once.
  - Memory, now as `anon`: each b33p from 62 MiB to 75–97 MiB over a round, with 18–27 MiB of
    page cache apart.
- **Run 17**, main with #88, #89 and #90 at their final heads:
  - first attempt: round 1 failed in setup (the Redis race above), rounds 2 and 3 passed;
  - rerun: 3 of 3 rounds passed (70, 71 and 70 checks), records in
    `target/testbox-tour-record/run/1793117809/`.
- **Merged on the user's word:** #90 (7b53abd), #89 (6ca0563), #88 (6dde8ac). Main's tree is
  byte-identical to the tested one.
- **#91 merged** (d43fca6): `provision` waits for `cluster_state:ok`. On its branch, a tour round
  passed (73 checks) with 18 refusals logged before the cluster formed.
- **The remaining testbox weaknesses, fixed on main:**
  - Desktop's pnpm is fetched in a `RUN`, taken from the cache when unchanged. The `desktop`
    phase passed on the rebuilt image (setup 11/11, desktop 17/17). The first build fetched and
    checked it (`pnpm-11.24.0.tgz: OK`, layer d86b05792b47); the next one reused that layer
    without running the step and built Desktop's image in 7 s.
  - Desktop's image is built only for a box that runs Desktop. A tour round built eight images,
    with no Desktop, and passed 70/70.
  - Containers log to `k8s-file`, not journald. In that tour round the record's server log
    ended at 18:39:21, a second before the record was written (18:39:22).
- **Item 2: the tour without workarounds.**
  - The setup accepts without waiting for titles; pictures are checked strictly for everyone;
    the link-without-preview step fails itself; no group request is answered.
  - Negative control, the strict tour on main without finding 15's fix: round 1 failed at the
    first group, "b33p 0 shows the group as a message request". The check bites.
  - With the fixes: rounds 1 and 2 passed (69 checks each). Round 3 failed in the primaries'
    setup (finding 16), before any b33p step.
  - The replay of seed 1793111125 passed strictly (68 checks).
  - Harness tests 52/12/2; b33p-core 267.
- **Run 18: the server with signal-server-atomic-take.patch** (finding 17), on main at 87d90ae,
  3 rounds, records in target/testbox-tour-record/run/1793156891.
  - Rounds 1 and 3 passed (68 checks each).
  - Round 2 failed at step 51 on finding 7: b33p 2's request to rejoin reached only b33p 0,
    and b33p 1 was to approve it.
