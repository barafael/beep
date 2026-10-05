# Plan

What is left to make langis a full Signal client on the desktop. Payments and calls are out
of scope, and so is upstreaming the presage fork. Milestones go roughly in order; within one,
items are independent unless noted.

## Done

- Linking with message history transfer (45 days of attachments), relinking when the device
  is removed, encrypted store (keyring or password), tray, single instance, notifications,
  several widget styles and themes.
- **M1 core and UI:** read state (receipts sent, read synced with the phone both ways, unread
  counts that survive restarts, "New messages" line, mark read or unread), delivery and read
  ticks, typing indicators both ways, edits and deletions for everyone (sent and received),
  disappearing messages (timers, sweeper, 1:1 timer changes), formatting (shortcuts in the
  composer, Signal's body ranges both ways, spoilers), pinned chats (Alt+1…9), mute, archive,
  privacy preferences, message and chat context menus.
- **M1 rest:** history loaded a page at a time (fork: `messages_before`), @-mentions of group
  members, moving pinned chats up and down, Note to Self always listed, chat settings read from
  the storage service (fork: `Manager::storage_service`; a setting is taken over only if it
  changed on the phone), headless UI tests.

## M1: remaining

- **Live verification** against a real account: receipts, read sync, typing in groups, our
  edits and deletions as seen on the phone, older pages while scrolled up, mentions as seen on
  the phone, the storage service read (pins, mutes, archive from the phone).

Known limits to revisit: the earlier text of someone else's edit is lost after a restart
(presage overwrites the stored message before langis sees it); a group counts as read once any
member read a message; pinned chats move by menu, not by dragging (a Slint `ListView` gives no
drag feedback); the storage service's read-receipt and typing settings and blocked contacts
aren't taken over yet.

## M2

Done: message requests (Accept / Delete / Block, synced with other devices), blocking (block list
synced both ways), chat folders read from the phone as tabs, contact details with safety
numbers, group details, forwarding, search in a chat (whole history, jumps to the result),
media overview (media, files, links), sending stickers from installed packs, link previews
(shown; sending opt-in), pasting and dropping files, blurred previews, video tiles and captions,
keyboard shortcuts, screen-reader labels.

Also done: writing chat settings back to the storage service (opt-in, Settings → Account):
records and manifest changed in their bytes so unknown fields survive, written only on top of
the version last read, conflicts read and retried.

Remaining:

- **Live verification** of all of M2 against a real account, the storage write above all: try
  it with one change (e.g. pin a chat) and check the phone before relying on it.
- **Not done within the items above:** marking a safety number verified; a notice when a
  contact's safety number changes (needs the presage fork to report identity changes);
  creating and editing folders (folder records aren't written yet); new storage records for
  chats the phone has none for (only existing records are changed); animated GIFs and real video
  thumbnails (need a video decoder; they open in the desktop's player); a sticker picker
  test that sees the stickers (the testing backend doesn't lay out popups).

## M3

Done: group management (create a group, rename it, description, disappearing-messages timer,
add and remove members, make and unmake admins, leave; group changes from others show as
notices), playing audio and voice messages in the window (AAC, MP4, MP3, Ogg, FLAC, WAV via
rodio), stories (received ones viewed for 24 hours with viewed receipts; text and photo stories
posted to groups).

Group changes need calls libsignal-service-rs lacks: a local clone with them (branch
`langis/groups`) is patched in from `../libsignal-service-rs`, and the presage fork uses them.

Remaining:

- **Live verification** of all of M3 against a real account (TESTPLAN.md §3).
- **Not done:** the group picture; accepting a group invitation here (it is done on the
  phone); adding people known only by phone number; group invite links and join requests; recording voice
  messages (audio input across desktops); posting to My Story or custom story lists (the
  audience needs care: distribution lists live in the storage service); replying to and
  reacting to stories; a seek bar for audio.
- **Upstream** the libsignal-service-rs changes, then drop the `[patch]`.
