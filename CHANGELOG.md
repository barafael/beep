# Changelog

The release workflow takes a version's notes from here (the section headed with its number).

## Unreleased

### Faster

- Starting and reading are faster on large accounts: b33p indexes the stored messages by chat
  (once, on the first start), so unread counts, the newest message of each chat and history
  pages no longer read through every chat's messages.
- Typing, reactions, receipts and downloads redraw only what they change, not every message
  of the open chat; long selections, catching up with a chat open and loading older pages no
  longer slow down with the size of the chat.
- Searching a chat, gathering its media and loading sticker packs no longer hold up
  everything else, and a new search stops the one before it.
- Sending a message with a large attachment no longer holds up everything else: messages,
  edits, reactions and deletions still go out in order, one after another, while read receipts,
  typing and older pages carry on.
- A new message, someone typing or a reaction redraws its chat's row in the list, not the whole
  list; a page of older messages goes in above in one go.
- Pictures take less memory (stickers and link previews are decoded at the size they show),
  and downloaded pictures beyond about 160 MB are let go of, oldest first, and downloaded
  again if they show again. The window no longer redraws all the time while offline.

### New

- **Start** on the start-up screen while catching up: the chats show at once, and the rest
  comes in as they do.
- A chat with many unread messages opens at the first of them; a ↓ button goes back to the
  newest message, with how many came since.
- Tooltips on icon buttons, and the time of a message inside a run when pointed at.
- A **⋯** button beside each message opens its menu; **+** in the reaction bar reacts with any
  emoji.
- A full emoji picker, with search and the ones used lately; emoji go where the caret is.
- Seeking in audio and voice messages.
- ↑, ↓ and Enter in the chat list's search.
- Leaving a group, blocking or deleting a message request and deleting for everyone ask
  first, in the same dialog as unlinking.
- Failures say whether trying again may help: the error offers **Try again** when it may;
  messages Signal refused and attachments gone from the server aren't offered again.

### Looks

- One look throughout: the same colours, text sizes, corners, spacing, shadows and motion
  everywhere, from one place (`ui/theme.slint`), following the desktop's colours as before.
- Icons instead of text symbols (play, pause, timer, verified, files), panels with a back
  arrow, banners with an icon, a b33p mark on the start-up screens.
- Chat previews of a photo, video, voice message, file or sticker, and changes of the
  disappearing-message timer, have an icon instead of an emoji (notifications keep theirs).
- The chat list's title row is as tall as the chat's header beside it.

### Fixed

- A message added to a run no longer leaves the one before it showing a time and ticks.
- Link-preview pictures show as soon as they have downloaded.
- The sender pictures in a group's messages appear when they arrive.
- Actions on a message can no longer hit a neighbouring one when messages come in meanwhile.
- Changing a chat's settings no longer goes back on another change made at the same moment.
- Esc also closes the join dialog, the calendar and the dialogs that ask first.
- Two messages sent within the same millisecond no longer take each other's place.
- Reactions, replies, deletes and further edits to an edited message work; they were dropped,
  and replies quoted the text before the edit.
- b33p no longer crashes drawing emoji on Debian 12 with Noto Color Emoji installed.
- A chat with someone new is titled with their name as soon as it is known, not with a code.
- A message that couldn't be sent for want of a connection says so in words, and the banner
  goes once Retry sends it.
- Receipts for edited messages work as in Signal's apps: an edit of yours shows its own ticks,
  delivered and read, and an edit someone else makes is read, and they're told so, like a new
  message.

## 0.2.0

The first release for others to try. Downloads for Linux (AppImage, .deb, tarball), macOS
(.dmg) and Windows (.zip), each for x86_64 and arm64; see the README for what each needs.

### When something goes wrong

- A failure at the start has a screen of its own that says what happened and what helps:
  **Try again**, **Start over** (deletes the messages stored here and links anew, after asking),
  or, when the system keyring is locked on the first start, **Use a password instead**. A
  keyring that isn't unlocked yet at login is tried again by itself.
- "Forgot your password?" on the unlock screen, and **Unlink this device** in Settings.
- Only a wrong password reads as one: data saved by a newer b33p, or one this build can't read,
  says so, and nothing suggests deleting it.
- Linking again after the phone removed this device keeps the messages here, also when the
  history transfer fails or is cut off.
- A second b33p can't open the same data (it would break decryption for both).
- Error messages say what to do; the banner shows them whole.
- The chat list says when Signal is out of reach, and while it catches up.

### New

- The device links as "b33p on <computer name>", and the name can be changed on the link
  screen and in Settings (it is used the next time this device is linked).
- Settings → About: the version, the source code, the log folder and where to report a problem.
  `b33p --version`, `--help`.
- A log file (`logs/b33p.log` in b33p's local data folder), with every line scrubbed of
  messages, names, numbers and IDs, so it can go with a bug report.
- Select text in messages, and pick several messages to copy, forward or delete together.
- Send a failed message again from its bubble.
- Groups: set and remove the picture; invite links (show, copy, turn on or off, reset, approve
  requests); join a group from a link.
- One start-up screen until the messages that came meanwhile are in.

### Safer

- Photos are sent without their EXIF and XMP data.
- What senders send is treated as untrusted: only what Signal's own apps accept is taken, sync
  messages only from our own devices, and everything from blocked people is dropped, in groups
  too. Only a message's author can delete or edit it, and deleting for everyone erases its
  content.
- Link previews only over https, to the internet.
- Names, numbers and IDs stay out of the log.

### Platforms

- Linux builds run on Debian 12, Ubuntu 23.04, Fedora 38 and newer (they needed Ubuntu 24.04's
  libraries before). The AppImage follows the GTK theme on GNOME and Xfce.
- Windows: the program has its icon and version; notifications show the icon;
  `b33p.exe --unregister` removes its registry entries before deleting it. The zip explains
  SmartScreen.
- macOS: the shortcuts are labelled with ⌘, ⌥ and ⇧, and the notification settings, which
  don't work there yet, are gone.

### Known limits

- macOS has no notifications yet, and its builds aren't notarized (see the README).
- Renaming a device that is already linked waits for the next link.
- Calls and payments are out of scope.
- See [PLAN.md](PLAN.md) for what is still missing.

## 0.1.0

The first build: linking with the message history, reading and sending, groups, attachments,
stories, notifications and the tray.
