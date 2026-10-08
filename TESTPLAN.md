# Test plan

Manual checks against a real Signal account, for what automated tests can't cover: the
Signal servers, the phone, and the desktop around langis. Each check says what to do and what
should happen. Note anything that differs, with the log (`RUST_LOG=langis=debug` helps).

## Before you start

- Build and run: `cargo run --release -p langis-slint`.
- Have your phone at hand, and ideally a second person (or a second Signal account) who can
  write to you, react, edit and delete. A small test group with them helps for the group checks.
- Back up first if you care about the local history: langis's data lives in
  `~/.local/share/langis` (the database is encrypted; keep its key or password).
- Turn **Settings → Account → Sync chat settings to your phone** off until section 2.13.

Mark each check: ✅ works, ❌ broken (say how), ➖ couldn't test.

---

## 1. M1

### 1.1 Read state and receipts

1. Have someone send you a message while langis is open but another chat is selected.
   - The chat shows an unread badge; the tray icon gets its red dot.
2. Open the chat.
   - The badge goes away. On **their** phone the message shows as read (two filled ticks),
     if read receipts are on for both of you.
   - On **your** phone, the chat is no longer unread.
3. Have them send another message, and read it on your **phone** first.
   - In langis the chat's unread count goes away without opening it.
4. Quit langis with an unread chat, start it again.
   - The unread count is still there.
5. With a chat open but langis's window **not** focused (another window in front), have them
   write.
   - It counts as unread and notifies; focusing langis reads it.
6. Right-click a read chat → **Mark as unread**: the chat closes and shows a dot. Open it again:
   the dot goes.
7. Open a chat with unread messages from before: a **New messages** line sits above the first
   unread one.

### 1.2 Delivery ticks

1. Send a message to someone whose phone is on.
   - Under it: ✓ when sent, ✓✓ when delivered, bold ✓✓ when they read it (if they send read
     receipts).
2. Restart langis; the ticks of older messages stay.

### 1.3 Typing indicators

1. Have someone type to you (1:1).
   - The chat list shows "<name> is typing…" instead of the preview, and so does the chat
     header. It goes away when they stop or after about 15 seconds.
2. The same in a group: "<name> is typing…" appears in the right group.
3. Type in langis: on their phone, you show as typing. Clear the composer: it stops.
4. Turn **Settings → Privacy → Typing indicators** off: they no longer see you typing.

### 1.4 Edits and deletions

1. Right-click one of your messages → **Edit**, change the text, Enter.
   - It shows the new text and "edited"; on their phone too.
2. Press ↑ in an empty composer: your last message goes into editing. Esc cancels.
3. Have them edit a message: it changes in langis and shows "edited".
4. Right-click your message → **Delete for everyone**.
   - It reads "This message was deleted." here and on their phone.
5. **Delete for me** on any message: it goes away here only.
6. Have them delete a message for everyone: it shows as deleted here, also after a restart.

### 1.5 Disappearing messages

1. Chat ⋮ → **Disappearing messages → 30 seconds** (1:1 chat).
   - A notice "You set disappearing messages to 30 seconds." On their phone the timer is set.
2. Send a message: it shows ⏱ with the time left and disappears after 30 seconds, here and
   on their phone.
3. Have them send one: its timer starts when you read it.
4. Change the timer on the phone: langis shows the notice and the new timer in the header.
5. Group chats: see 3.1.

### 1.6 Formatting

1. Send `*bold* _italic_ ~strike~ ` `` `mono` `` ` ||spoiler||`.
   - Shown formatted here and on their phone; the spoiler is hidden until clicked.
2. Have them send formatted text from their phone: shown formatted.

### 1.7 Mentions

1. In a group, type `@` and part of a name: members are offered. Pick one with a click, Tab
   or Enter, and send.
   - On their phone it is a real mention (highlighted, notifies them).
2. Mentions they send show as "@Name".

### 1.8 Pinned, muted, archived chats

1. Right-click chats → **Pin** a few: they come first, in order. **Move up/down** reorders.
2. Alt+1 … Alt+9 open the pinned chats.
3. **Mute → For 1 hour**: 🔕 in the list; new messages don't notify and don't count in the
   tray. **Unmute** undoes it.
4. **Archive**: the chat moves to "Archived chats". A new message in it brings it back, unless
   it is muted.
5. Restart: pins, mutes and archive are kept.

### 1.9 History

1. Open a long chat: the newest messages show at once. Scroll up: older ones load, and what you
   were looking at stays in place.
2. Scroll all the way to the beginning of a long chat: no gaps, no duplicates.

### 1.10 Settings from the phone (reading the storage service)

1. On the phone, pin a chat, mute another, archive a third. Restart langis.
   - langis shows them pinned, muted, archived.
2. With langis running, change one of them on the phone.
   - langis follows within a few seconds.
3. Pin a chat only in langis, then pin a different one on the phone.
   - The phone's pins come first; langis's own pin stays after them.

### 1.11 Note to Self

1. Note to Self is in the list even before you wrote there; messages to it arrive on the
   phone.

---

## 2. M2

### 2.1 Forwarding

1. Right-click a message with a photo → **Forward…**, search for a chat, pick it.
   - The copy arrives there with the photo, here and on the recipient's phone.
2. Forward a long message after opening it (its full text shows): the full text arrives.

### 2.2 Search in a chat

1. In a long chat, Ctrl+F (or 🔍), type a word from an old message.
   - Results list sender, date and the text around the word.
2. Click an old result.
   - langis loads back to it, scrolls there and outlines the message.
3. A word that isn't there: "No messages found."

### 2.3 Media, files and links

1. Chat ⋮ → **Media, files and links**.
   - Media: photos as a grid (they load as you scroll), videos with ▶.
   - Files: Open and Save work.
   - Links: clicking opens the browser.

### 2.4 Attaching by paste and drop

1. Drag a file from the file manager onto an open chat: "Drop to attach", then it is staged.
2. Copy a picture (e.g. a screenshot) and press Ctrl+V in the composer: it is staged as a PNG.
3. Copy files in the file manager and Ctrl+V: they are staged.
4. Plain text still pastes as text.

### 2.5 Keyboard shortcuts

Check each from the list in Settings: Ctrl+K, Ctrl+F, Alt+↑/↓, Alt+Shift+↓ (next unread),
Alt+1…9, ↑ to edit, Esc closing things innermost first (forward dialog, details, media, search,
edit, reply, settings).

### 2.6 Details and safety numbers

1. Click a contact chat's picture or name.
   - Their about line (if they set one), phone number, and a safety number.
2. Compare the safety number with the one on your phone (Signal → the chat → View safety
   number): **all 60 digits must be the same.**
3. A group's details: description, members, admins first.

### 2.7 Message requests

1. Have someone who isn't in your contacts and has never been written to write to you.
   - The chat says "Message request", there is no composer but Block / Delete / Accept, and they
     don't get a read receipt.
2. **Accept**: the composer appears; they can see your name and picture; your phone no longer
   shows it as a request.
3. With another request: **Delete** removes the chat, also on the phone. **Block** blocks them.
4. Answer a request on the phone: langis follows.

### 2.8 Blocking

1. Chat menu → **Block**: an "Unblock" bar replaces the composer; new messages from them don't
   show or notify. On the phone, they show as blocked.
2. Block someone on the phone: langis blocks them too.
3. **Unblock**, both ways.

### 2.9 Media

1. Receive a photo: a blurred preview shows until it has downloaded.
2. Receive a video and a GIF: tiles with ▶ / GIF; clicking opens the system player.
3. Photos with a caption: the caption shows under them.

### 2.10 Link previews

1. Receive a link with a preview: a card with picture, title, description and site; clicking it
   opens the link.
2. Turn **Settings → Privacy → Link previews** on and send a link (https).
   - The message goes out with a preview, on their phone too. (Off: no preview.)

### 2.11 Stickers

1. Install a sticker pack on the phone; restart langis.
2. ▦ next to the emoji button: the pack shows as a tab with its stickers.
3. Click one: it is sent and shows as a sticker here and on their phone.

### 2.12 Chat folders

1. Make a folder on the phone (Settings → Chats → Chat folders), e.g. with two chats, or "all
   groups". Restart langis.
   - Its tab shows above the chat list and filters it as on the phone; the "only unread" and
     "muted" options are respected.
2. Delete the folder on the phone: the tab goes after the next sync.

### 2.13 Writing settings to the phone — carefully

This writes to your account's shared settings. Do it one step at a time and check the phone
after each.

1. Turn **Settings → Account → Sync chat settings to your phone** on.
2. Pin **one** chat in langis. Within a few seconds the phone shows it pinned.
   - Check that nothing else changed on the phone: other pins, mutes, archived chats,
     folders, your profile, privacy settings.
3. Unpin it again: unpinned on the phone.
4. Mute, archive, block, mark unread one chat each: the phone follows each time.
5. Make a change on the phone and, within a few seconds, a different one in langis: both
   survive (langis reads the phone's change first, then writes its own).
6. If anything on the phone changed that shouldn't have: turn the setting off and send me the
   log (`wrote to the storage service` lines).

### 2.14 Accessibility

1. With a screen reader (Orca): moving through the chat list reads each chat as one item
   (name, unread count, last message); buttons say what they do; Enter or the reader's default
   action opens a chat.

---

## 3. M3

Groups are best tested in a new group made for it, with one or two people who know they are
part of a test. Every change below reaches all members' phones.

### 3.1 Groups

1. **+** above the chat list → pick two people → name the group → **Create**.
   - The group opens; it reads "You created the group." On their phones the group appears with
     both of you as members, and they can write to it; their messages arrive in langis.
   - On **your** phone the group appears too.
2. Group details (click its name) → change the **name**, then the **description**.
   - Notices "You changed the group name to …" here, and the change on every phone.
3. Chat ⋮ → **Disappearing messages → 1 minute** in the group.
   - A notice here and on every phone; messages then disappear for everyone.
4. Details → **Add members** → pick someone.
   - "You added …"; the new member sees the group and its name, and can write.
5. Member ⋮ → **Make admin**, then **Remove as admin**: their role changes on the phones.
6. Member ⋮ → **Remove from group**: they are gone from the member list, here and on the
   phones; they can no longer write to it.
7. Changes by others: have someone rename the group, add or remove someone, change the timer
   on their phone.
   - langis shows a notice for each ("Alice changed the group name to …") and the new state.
8. Restart langis: the notices of 1–7 are still there.
9. A group where you are not an admin and only admins may edit (set on the phone): langis says
   the change failed (the error shows), and nothing changes.
10. **Leave group** while you are its only admin: langis asks you to make someone else an admin
    first. Do that (5), then **Leave group** (details, confirm with "Really leave?").
    - A banner says you aren't a member; the composer is gone. On the others' phones you left
      the group.
11. Have someone remove you from a group.
    - The notice "… removed You." and the same banner; you can no longer write to it.
12. Have someone who doesn't have your profile key yet add you to a group (it invites you).
    - The group says you're invited and to accept on your phone; accept there, and langis
      lets you write once the next group message arrives.
13. With one member's phone off or unreachable, rename a group: the change still goes through
    here and for the others (no error), and that member sees it later.

### 3.2 Voice messages and audio

1. Have someone send a voice message from their phone (hold the mic button).
   - It shows as a row with ▶ and its length. Click ▶: it plays, the bar fills, the time counts
     up. ⏸ pauses, ▶ goes on.
2. Start another voice message while one plays: the first stops.
3. An audio file sent as an attachment (MP3, M4A, Ogg): plays the same way.
4. No sound device (e.g. pulled headphones with nothing else): a short error, no crash.

### 3.3 Stories

1. Have someone post a story on their phone (text, then a photo), visible to you.
   - ◎ above the chat list gets a dot. Click it: their ring is listed with the time.
2. Click the ring: the story shows full size; ‹ › move between their stories, Esc
   closes the story, Esc again the list.
   - On **their** phone your view is counted (if you send read receipts / view receipts are on).
   - On **your** phone the story counts as viewed too.
3. A story posted to a group you are in shows under the group's name.
4. Your own stories posted on the phone show in the list too, without sending yourself receipts.
5. **New story** (bottom of the list): pick a test group, type text, **Post**.
   - It appears in the group's stories on the members' phones, with the text on a blue
     background, and on your phone under "My stories".
6. **Photo…** → pick a picture → it is posted the same way, as a photo story.
7. 24 hours after posting, stories are gone from the list (also after restarting).
8. Stories from someone whose chat is an unanswered message request, or whom you blocked, don't
   show (watching would tell them you're active). Accept the request: their stories appear.

## 4. Review fixes (October 2026)

What the third code review changed; each is a manual check because it needs the servers, the
phone or a second person.

1. Edit a message you sent **with a photo** (↑, change the caption): the photo stays, here and
   on their phone. Edit a message with a link preview so the link is gone: the card goes too.
2. Receive a file called e.g. `test.exe` or `test.sh`: **Open** refuses with a message saying
   to save it instead; **Save** still works.
3. Freshly linked, with the phone's storage key arriving late (phone offline while linking,
   then online): within five minutes message requests are told apart from accepted chats
   (Settings from the phone are read; see 1.10).
4. A message request's notification: typing a reply in the notification popup does nothing
   (it would accept the request). Accept it in langis, then replying from a notification works.
5. Block someone, then have them write to you from a fresh install (an undecryptable
   message): nothing shows in their chat, and the log has no "reset the session" for them.
6. In a group, have a member change the timer or remove someone: the notice shows (their
   change is signed by the server). No notice ever appears for a change that didn't happen.

