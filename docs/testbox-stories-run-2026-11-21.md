# Testbox: stories to My Story, 2026-11-21

The `stories` phase, new with posting to My Story, and the whole suite around it, on a box of
its own (`--instance stories`), debug builds, on branch `my-story`. b33p sends a story to My
Story with its sender key for the list, sealed for every recipient's devices in one request
(`PUT /v1/messages/multi_recipient?story=true`, presage `send_story_to_list`, on
libsignal-service's `pr/story-list-send`), and syncs the phone naming the list.

## Result

The last full run (`cargo testbox test --instance stories --screenshots`, 01:09, load average
1.4 at its start): **195 checks passed, none failed**, every phase on both boxes.

| Phase | Checks | Seconds |
|---|---|---|
| setup, linked box | 12/0 | 23 |
| desktop | 34/0 | 57 |
| attachments | 13/0 | 19 |
| network | 16/0 | 323 |
| core | 9/0 | 122 |
| setup, Android box | 16/0 | 83 |
| android | 18/0 | 61 |
| groups | 30/0 | 116 |
| storage | 14/0 | 56 |
| large-group | 11/0 | 33 |
| media | 9/0 | 35 |
| stories | 13/0 | 82 |

On the shared Android box, b33p's account has 22 connections by `stories` (the peer, the
member, and the twenty members `large-group` accepted). b33p's log:

```
INFO presage::manager::registered::groups: sent a story to a story list in one request, with its sender key recipients=22
INFO b33p_core::signal::stories: posted a story to a list reached=22 of=22
INFO presage::manager::registered::groups: sent a story to a story list in one request, with its sender key recipients=21
INFO b33p_core::signal::stories: posted a story to a list reached=21 of=21
```

The second is after Android left the peer out of My Story: 21, and the peer's Desktop didn't
show it within 30 s of the member having it.

## What the phase checks

1. b33p's My Story reaches both connections (the harness snapshot's `story_lists`).
2. A story b33p posts to My Story reaches the peer's Desktop and the presage-cli member, in one
   request (presage's log line above).
3. b33p files it under its "My Story" ring.
4. The phone files it under its own My Story, counting Desktop's view: Android's My Stories
   lists a story under My Story with "1 view". Its text can't be read there (below); it's the
   only story under My Story by then, and the view is the one Desktop sent opening it.
5. Android's own story to My Story shows on b33p under My Story (a sync naming the list), and
   on Desktop.
6. The peer's own story to My Story, posted on Desktop, shows on b33p under the peer.
7. Android leaves the peer out of My Story ("All except…"); b33p reads it from the storage
   service: My Story's recipients lose the peer.
8. b33p's next story reaches the member, and the peer's Desktop doesn't show it.
9. Android's My Story goes to everyone again, and b33p's with it.

## The runs before, and what each failure was

| Run | What | Result | Cause |
|---|---|---|---|
| stories 1 | the phase alone | 3 failed | the drivers (below) |
| stories 2 | the phase alone | 2 failed | the drivers |
| stories 3 | the phase alone | 1 failed | Android's text story (below) |
| stories 4 | the phase alone | setup failed | Android's first boot (below) |
| full 1 | every phase | 193/194 | the peer's name on the shared box |
| full 2 | every phase | 195/195 | |

None was b33p's. What they were:

- **Android's story viewer** shows a first-time explanation over the story ("Got it"), and two
  Back presses didn't reach the chats from it; the driver now goes back until the tab bar
  shows.
- **Desktop's first story to My Story** asks who may see it, on a modal page of its own
  (`SendStoryModal__my-stories-privacy`, outside `.SendStoryModal`, where the driver first
  looked for its Save); saving leaves My Story unchosen, so it is chosen again.
- **Android's My Stories row** opens the viewer by its thumbnail, not its texts; and the viewer
  draws a text story without a text node: in run 3 the viewer opened (logcat:
  `StoryViewerActivity` 23:39:53.5), played the story its 5 s and closed, and the agent never
  saw the text. Hence the views (check 4).
- **The peer's name on Android** after `storage` is "Nicky" (the nickname it gives), and after
  `large-group` the contact list is long: the phase leaves the peer out by the name b33p shows
  (taken from the storage service as Android's is), scrolled to.
- **Android's first boot, stories run 4**: the driver's `adb shell stop` (raising the ANR
  timeouts, right after `adb root` and `setprop`) exited 255 with nothing printed, a dropped
  transport. The container's restart (the pod's policy) booted, and the step passed there and
  in every run since, as in the four before. **Not explained**: the record has no logcat (the
  container restarted) and adb said nothing. The driver now prints adb's devices and the
  device's `init.svc.adbd`, `sys.boot_completed` and `service.adb.root` beside such a failure,
  so a next one says more.

## Seen, not b33p's

Desktop dropped one envelope from Android's phone (device 1) in stories run 1: "Dropping story
message - story=true on envelope, but message was not a story send or delete" (23:23:18.850),
beside Android's story, which it showed. b33p sends from its linked device; its stories showed
on Desktop each time.

## Limits

- Check 8 is bounded: the peer's Desktop is watched 30 s after the member had the story. The
  server only delivers to the recipients the request names, which b33p's log counts (21).
- Check 4 shows the phone has b33p's story under My Story with Desktop's view on it, not its
  text.
- Editing who is on a list from b33p isn't there yet (PLAN.md): it needs the storage write.
