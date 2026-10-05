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

- **Storage service, writing**, so pins (the first four), mutes, archive and blocks follow to
  the phone; then langis-only changes no longer need protecting from the next read.
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
