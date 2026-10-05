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

## M1: remaining

- **History paging.** `LoadHistory` reads a whole conversation. Add a `LIMIT`ed query to the
  presage fork (newest first, before a timestamp) and load older messages as the list scrolls
  up.
- **@-mention picker.** Group members per conversation, a picker on `@` in the composer, and
  `Mention`s passed with `Command::Send` (the wire encoding is done in `format::encode`).
- **Pinned chats, the rest.** Drag to reorder (`ChatState::move_pin` exists); sync the first
  four pins through the storage service once it is written (M2).
- **Note to Self** always listed, not only once it has messages.
- **Storage service, reading.** Pull the account record and contact/group records: pins,
  mutes, archive, blocked, profile sharing. Merge into langis's tables, local changes winning
  until writing exists.
- **UI tests** with Slint's testing backend: context menus, archive list, editing, the
  composer's shortcuts.
- **Live verification** against a real account: receipts, read sync, typing in groups, our
  edits and deletions as seen on the phone.

Known limits to revisit: the earlier text of someone else's edit is lost after a restart
(presage overwrites the stored message before langis sees it); a group counts as read once any
member read a message.

## M2

- **Storage service, writing**, so pins, mutes, archive and blocks follow to the phone.
- **Message requests**: accept, delete, block for chats with unknown senders.
- **Blocking** contacts and groups.
- **Chat folders** (Signal's chat folders, from the storage service).
- **Contact details**: profile, safety number and verification, a notice when a safety number
  changes (needs the presage fork to report identity changes).
- **Group details**: members, description, invite link (read only until M3).
- **Forwarding** messages and attachments.
- **Search in a chat**, jumping to results.
- **Media overview** per chat: photos, files, links.
- **Stickers**: show installed packs, send them.
- **Link previews**, opt-in.
- **Paste and drop** files and images into the composer.
- **Media polish**: blurhash placeholders, animated GIFs, video thumbnails, captions.
- **Keyboard shortcuts** beyond Alt+1…9 (search, next unread, close chat).
- **Accessibility**: labels and focus order for screen readers.

## M3

- **Group management**: create groups, add and remove members, change name, picture,
  description and the disappearing-messages timer. Needs a fork of libsignal-service-rs for
  group changes.
- **Stories**: receive and view first, then send.
- **Voice messages**: playback (symphonia + cpal); recording later, as it depends on audio
  input across desktops.
