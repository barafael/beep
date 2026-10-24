# Test plan

Manual checks against a real Signal account, for what automated tests can't cover: the
Signal servers, the phone, and the desktop around b33p. Each check says what to do and what
should happen. Note anything that differs, with the log (`RUST_LOG=b33p=debug` helps).

## Before you start

- Build and run: `cargo run --release -p b33p-slint`.
- Have your phone at hand, and ideally a second person (or a second Signal account) who can
  write to you, react, edit and delete. A small test group with them helps for the group checks.
- Back up first if you care about the local history: b33p's data lives in
  `~/.local/share/b33p` (the database is encrypted; keep its key or password).
- Turn **Settings → Account → Sync chat settings to your phone** off until section 2.13.

Mark each check: ✅ works, ❌ broken (say how), ➖ couldn't test.

---

## 1. M1

### 1.0 Starting up

1. Start b33p: one screen with a spinner says "Opening your messages…", "Connecting to
   Signal…", then "Catching up on your messages…" (with a count if any came while it was
   closed), and the chats appear once that is done. Nothing about syncing shows afterwards.
2. Start it with the network off: the chats appear anyway (nothing can be sent until it is
   back); turn the network on: messages arrive, with no banner or bar.
3. Link a fresh install with the history transfer: the same screen walks through waiting for
   the phone (with **Skip**), downloading and importing, then the catch-up, then the chats.
4. Leave b33p closed for a few days, with messages coming in meanwhile, then start it: while
   it catches up, a **Start** button appears after a moment. Clicking it shows the chats at
   once; the list says "Catching up…" until the rest is in, and the messages that are still
   coming appear in their chats as they come. Without clicking, the chats come as before.
5. The first start after this version, on a large account: "Opening your messages…" may
   take a few seconds longer once (b33p indexes the messages); later starts are faster than
   before, the chat list included.

### 1.1 Read state and receipts

1. Have someone send you a message while b33p is open but another chat is selected.
   - The chat shows an unread badge; the tray icon gets its red dot.
2. Open the chat.
   - The badge goes away. On **their** phone the message shows as read (two filled ticks),
     if read receipts are on for both of you.
   - On **your** phone, the chat is no longer unread.
3. Have them send another message, and read it on your **phone** first.
   - In b33p the chat's unread count goes away without opening it.
4. Quit b33p with an unread chat, start it again.
   - The unread count is still there.
5. With a chat open but b33p's window **not** focused (another window in front), have them
   write.
   - It counts as unread and notifies; focusing b33p reads it.
6. Right-click a read chat → **Mark as unread**: the chat closes and shows a dot. Open it again:
   the dot goes.
7. Open a chat with unread messages from before: a **New messages** line sits above the first
   unread one.

### 1.2 Delivery ticks

1. Send a message to someone whose phone is on.
   - Under it: ✓ when sent, ✓✓ when delivered, ✓✓ on a filled pill when they read it (if they
     send read receipts).
2. Restart b33p; the ticks of older messages stay.
3. Send while offline (network off, or right as b33p reconnects): the bubble shows
   **Not sent · Retry**. Once online, click it: "Sending…", then the ticks as in 1.

### 1.3 Typing indicators

1. Have someone type to you (1:1).
   - The chat list shows "<name> is typing…" instead of the preview, and so does the chat
     header. It goes away when they stop or after about 15 seconds.
2. The same in a group: "<name> is typing…" appears in the right group.
3. Type in b33p: on their phone, you show as typing. Clear the composer: it stops.
4. Turn **Settings → Privacy → Typing indicators** off: they no longer see you typing.

### 1.4 Edits and deletions

1. Right-click one of your messages → **Edit**, change the text, Enter.
   - It shows the new text and "edited"; on their phone too.
2. Press ↑ in an empty composer: your last message goes into editing. Esc cancels.
3. Have them edit a message: it changes in b33p and shows "edited".
4. Right-click your message → **Delete for everyone**.
   - It reads "This message was deleted." here and on their phone.
5. **Delete for me** on any message: it goes away here only.
6. Have them delete a message for everyone: it shows as deleted here, also after a restart.
7. Ctrl+click two of your recent messages, then the bin in the bar over the composer →
   **Delete for everyone**: both read as deleted here and on their phone. (**Delete for
   everyone** is only offered while every selected message is yours and recent.)

### 1.5 Disappearing messages

1. Chat ⋮ → **Disappearing messages → 30 seconds** (1:1 chat).
   - A notice "You set disappearing messages to 30 seconds.", with a timer icon before it. On
     their phone the timer is set. In the chat list, the preview has the timer icon too.
2. Send a message: it shows a timer icon with the time left and disappears after 30 seconds,
   here and on their phone.
3. Have them send one: its timer starts when you read it.
4. Change the timer on the phone: b33p shows the notice and the new timer in the header.
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

1. On the phone, pin a chat, mute another, archive a third. Restart b33p.
   - b33p shows them pinned, muted, archived.
2. With b33p running, change one of them on the phone.
   - b33p follows within a few seconds.
3. Pin a chat only in b33p, then pin a different one on the phone.
   - The phone's pins come first; b33p's own pin stays after them.

### 1.11 Note to Self

1. Note to Self is in the list even before you wrote there; messages to it arrive on the
   phone.

---

## 2. M2

### 2.1 Forwarding

1. Right-click a message with a photo → **Forward…**, search for a chat, pick it.
   - The copy arrives there with the photo, here and on the recipient's phone.
2. Forward a long message after opening it (its full text shows): the full text arrives.
3. Ctrl+click one message, Shift+click another a few below, then the arrow in the bar over
   the composer: the dialog says "N messages"; after picking a chat, all of them arrive
   there, in order, and the selection is gone.

### 2.2 Search in a chat

1. In a long chat, Ctrl+F (or 🔍), type a word from an old message.
   - Results list sender, date and the text around the word.
2. Click an old result.
   - b33p loads back to it, scrolls there and outlines the message.
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
Alt+1…9, ↑ to edit, Esc closing things innermost first (forward dialog, selection, details,
media, search, edit, reply, settings). In the composer with two lines: ↓ on the last line goes
to the end, ↑ on the first to the start; Ctrl+Shift+←/→ selects by word. In a message, drag
to select text (a double click a word) and Ctrl+C: it pastes elsewhere; selecting in another
message drops the first; a click on a link still opens it; Esc or writing in the composer
drops the selection.

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
4. Answer a request on the phone: b33p follows.

### 2.8 Blocking

1. Chat menu → **Block**: an "Unblock" bar replaces the composer; new messages from them don't
   show or notify. On the phone, they show as blocked.
2. Block someone on the phone: b33p blocks them too.
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

1. Install a sticker pack on the phone; restart b33p.
2. ▦ next to the emoji button: the pack shows as a tab with its stickers.
3. Click one: it is sent and shows as a sticker here and on their phone.

### 2.12 Chat folders

1. Make a folder on the phone (Settings → Chats → Chat folders), e.g. with two chats, or "all
   groups". Restart b33p.
   - Its tab shows above the chat list and filters it as on the phone; the "only unread" and
     "muted" options are respected.
2. Delete the folder on the phone: the tab goes after the next sync.

### 2.13 Writing settings to the phone — carefully

This writes to your account's shared settings. Do it one step at a time and check the phone
after each.

1. Turn **Settings → Account → Sync chat settings to your phone** on.
2. Pin **one** chat in b33p. Within a few seconds the phone shows it pinned.
   - Check that nothing else changed on the phone: other pins, mutes, archived chats,
     folders, your profile, privacy settings.
3. Unpin it again: unpinned on the phone.
4. Mute, archive, block, mark unread one chat each: the phone follows each time.
5. Make a change on the phone and, within a few seconds, a different one in b33p: both
   survive (b33p reads the phone's change first, then writes its own).
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
     both of you as members, and they can write to it; their messages arrive in b33p.
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
   - b33p shows a notice for each ("Alice changed the group name to …") and the new state.
8. Restart b33p: the notices of 1–7 are still there.
9. A group where you are not an admin and only admins may edit (set on the phone): b33p says
   the change failed (the error shows), and nothing changes.
10. **Leave group** while you are its only admin: b33p asks you to make someone else an admin
    first. Do that (5), then **Leave group** (details, confirm with "Really leave?").
    - A banner says you aren't a member; the composer is gone. On the others' phones you left
      the group.
11. Have someone remove you from a group.
    - The notice "… removed You." and the same banner; you can no longer write to it.
12. Have someone who doesn't have your profile key yet add you to a group (it invites you).
    - The group says you're invited and to accept on your phone; accept there, and b33p
      lets you write once the next group message arrives.
13. With one member's phone off or unreachable, rename a group: the change still goes through
    here and for the others (no error), and that member sees it later.
14. Group details → **Change picture…** → pick a photo: the group's picture changes here, on
    every phone, and the chat shows "You changed the group's picture." **Remove picture**
    takes it away everywhere. Have someone else change it from their phone: it changes here.
15. Group details (as admin) → Invite link → **Anyone**: a link appears, **Copy** puts it on
    the clipboard, and the chat says "You turned the invite link on." Open the link on a phone
    that isn't in the group: it joins at once, and the chat here says they joined. A member
    who isn't an admin sees and copies the link, without the buttons.
16. **Admins approve**: the notice says "…with admin approval". Someone opening the link on
    their phone asks to join: open the details again, and their request is listed. **Let in**
    makes them a member (everyone sees "You accepted …'s request to join."); **Turn down**
    drops the request, and tells nobody.
17. **Reset**: a new link, and the old one no longer works on a phone. **Off**: the link goes;
    the old one tells a phone that it isn't active.
18. Chat list → link icon → paste a link from a phone (group settings → Group link → Share):
    the group's name and size show. **Join**: the group opens with "You joined the group."
    and the phones see you join. With admin approval, **Ask to join** opens the group saying
    you asked, with **Take request back**; the admin's phone shows the request, and letting
    you in makes the chat writable here with "… accepted your request to join." Turning you
    down leaves it waiting until you open its details, which then say you aren't a member.
    A `signal.group` link in a message opens this dialog, not the browser.

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
   (it would accept the request). Accept it in b33p, then replying from a notification works.
5. Block someone, then have them write to you from a fresh install (an undecryptable
   message): nothing shows in their chat, and the log has no "reset the session" for them.
6. In a group, have a member change the timer or remove someone: the notice shows (their
   change is signed by the server). No notice ever appears for a change that didn't happen.
7. Have someone invite you to a group (12 in 3.1), then **decline on your phone** (or have
   them withdraw it): "You're invited to this group." goes away here, and stays away after a
   restart. The same when you decline here.
8. Have someone reinstall Signal (or switch phones) after you wrote to them, so their first
   message back can't be read: at most one "couldn't be decrypted" notice, then their next
   messages arrive. No notice appears for them when the connection drops and comes back.
9. Write to someone whose *other* device just reinstalled Signal (they'll see your message on
   the phone but not there): no notice here, and the message turns up on that device too
   within a minute (the log says "sent a message again").
10. Accept a group you were invited to (the ⋯ menu): no second "You created the group." or
    "added you to the group." line appears.

## 4b. The UI review (October 2026)

What the review of snappiness and looks changed. Most of it is covered by tests; these need
a real account, a desktop or a person.

1. Open a chat with many unread messages: it opens at "New messages", not at its end. With
   only a few unread, it opens at the end as before.
2. Scroll up in a busy chat and have someone write: a round ↓ button appears at the bottom
   right with how many came; clicking it goes to the newest message.
3. In the chat list's search, type part of a name: ↑ and ↓ move a frame through what is
   found, Enter opens it. A phone number and Enter offers to message it.
4. Point at an icon button (header, composer, sidebar): its name shows as a tooltip. Point at
   a message inside a run: its time shows.
5. Point at a message: beside Reply, **⋯** opens the same menu as right-clicking. The
   reaction bar has **+**, which opens every emoji; picking one reacts.
6. The composer's emoji button opens a picker with groups, search and recently used ones
   (kept after a restart). An emoji goes where the caret is, not at the end.
7. Play a voice message, then click or drag along its bar: it plays from there.
8. Leave a group, block or delete a message request, unlink, start over, or delete a message
   for everyone: each asks first. Enter and Esc take Cancel.
9. Pull the network cable: the list says "Not connected" with a crossed-out Wi-Fi icon and no
   spinner, and the window stops redrawing (CPU idle).
10. With the window in the background, receive a few hundred messages in a burst: the window
    stays responsive while they come in.
11. Look through the window in Breeze light, Breeze dark and Fusion dark: colours, text
    sizes, corners and spacing are alike everywhere (`python3 docs/screenshots/shoot.py`
    takes the pictures; `compare.py` compares two runs).
12. With Orca: a message reads its text, then who wrote it, when, and how far it got.
13. On a slow connection, send a large file, and while it uploads: type in another chat (they
    see you typing), open a third chat (its messages load), react to something and send a
    short message in the first chat. The reaction and the message go out after the file, in
    that order, on their phone too.
14. Delete a message for everyone while it is still sending: it doesn't come back once sent.
15. Receive a photo, a voice message, a file and a sticker without text in different chats:
    each preview in the list has an icon before its word, and the notification still starts
    with an emoji. Change a chat's disappearing-message timer: the notice and the preview have
    a timer icon.
16. With hundreds of chats, receive messages in a few of them while scrolled down the list:
    the list doesn't jump, and each chat moves to the top with its new preview.
17. Pull the network cable and react to a message, or open a group link: the error banner
    offers **Try again**, which works once the cable is back. Send a message to a number that
    has left Signal: it says "Not sent", with no Retry.


## 5. When something goes wrong (0.2.0)

Back up `~/.local/share/b33p` before these; some delete it on purpose.

1. Quit b33p, stop the keyring (on KDE: `kwalletd6` locked or closed; GNOME: lock the
   keyring), start b33p: "b33p can't go on" says the keyring isn't available yet and that it
   keeps trying. Unlock the keyring: within a minute the chats appear without clicking.
2. With autostart on, log out and in: b33p comes up in the tray even if the keyring unlocks
   late, and no failure window stays behind.
3. On a fresh data directory with the keyring locked: the screen offers **Use a password
   instead**; choosing it asks for a new password, and the next start asks for it.
4. Unlock screen → **Forgot your password?** → the dialog says the messages here are deleted;
   **Cancel** changes nothing; **Delete and start over** shows a new linking code, and the data
   directory holds no database. The phone still has its messages.
5. Turn the network off, start linking: after the code fails, "Linking this device didn't
   work" with **Try again**; turn the network on, **Try again**: a new code.
6. Change the device name under the code, press Enter: a new code; link it: the phone's
   Linked devices shows that name. Settings → Device name shows it too.
7. Start a second b33p while one runs, once with `DBUS_SESSION_BUS_ADDRESS=` empty (so the
   single-instance check can't tell): the second says b33p is already running, and the first
   keeps working.
8. Remove this device on the phone (Linked devices → b33p → Unlink). b33p says so and shows a
   code; link again **with** the history transfer, and quit b33p while it downloads. Start
   it: it asks to link again, and the chats that were there before are still there.
9. Settings → **Unlink this device…** → **Unlink and delete**: the phone no longer lists it,
   b33p shows a linking code, and the chats are gone here.
10. Take the network away while chatting: the chat list says "Not connected · trying again";
    back: "Catching up…", then nothing.
11. Settings → About → **Open log folder**: `b33p.log` is there. Search it for a contact's
    number, name, or a word you sent: none of them appear.
12. Start the 0.2.0 build on the data of a newer test build whose migrations it doesn't know
    (or on 0.1.0's data, which must just open): it says the data was saved by a newer version,
    and never "Wrong password."

## 6. The packages, on a clean system

1. **AppImage** on GNOME (Wayland): it starts, has a title bar, follows the GTK theme (dark
   too). Notifications show b33p's name and icon.
2. **.deb** on a fresh Debian 12: `sudo apt install ./b33p-*.deb` pulls in Qt and starts from
   the menu with its icon; `apt remove b33p` removes it.
3. **Windows zip** on a fresh Windows 11: SmartScreen as the README says; the exe shows the
   icon in Explorer and its version under Properties → Details; a toast shows the icon; the
   log appears where the README says. `b33p.exe --unregister` turns off starting with Windows.
4. **macOS dmg**: the Gatekeeper steps in the README open it; Settings has no notification
   switches and shows ⌘ in the shortcuts; closing the window and clicking the Dock icon (note
   what happens: PLAN.md lists it as unchecked).
