# What a long-lived account has that the test box doesn't

The contact names (#149) were missing for a reason all the checks shared: the test box's accounts
are a day old. Everyone there has a profile name, everyone has written to everyone, and nobody
has an address book, nicknames, defaults set on the phone, or deletes made elsewhere. What only a
long-lived account has, b33p was never shown.

This plan closes the gaps found so far, and keeps the next ones from staying hidden. It was
written 2026-11-08, against `main` at `8ddd7d8`.

## The gaps, as checked in the code

| # | What | Found where | How much it matters |
|---|---|---|---|
| G1 | **Default timer for disappearing messages.** The account record's `universalExpireTimer` is never read, so a chat or group started in b33p doesn't get the timer the phone would give it | `storage.rs` reads the account record only for the receipts, typing and previews settings and the pins | Privacy: messages meant to disappear don't |
| G2 | **Profile keys from the storage service.** `ContactRecord.profileKey` is not read. b33p has a contact's key only from the history transfer, a message of theirs, or a group they share. Without it there is no profile picture, "About" text or profile name for that contact | `storage.rs`: no `profile_key` | Contacts show initials. It also decides sealed sender: without the key, b33p sends identified |
| G3 | **Deleting "for me" isn't shared with the phone**, though b33p's menu item already says "Delete for me" (`bubble.slint:805`), which in Signal's apps deletes on every device. In b33p it isn't sent to our other devices, and the phone's deletes reach presage's store but not b33p's window. Whole-chat deletes and attachments aren't handled at all (presage `registered.rs:3621`, a TODO) | `Command::Delete { for_everyone: false }` sends no sync message; b33p has no `DeleteForMe` anywhere | A message deleted on one device stays on the other. A chat deleted on the phone stays in b33p |
| G4 | **Group: "notify for mentions while muted."** `GroupV2Record.dontNotifyForMentionsIfMuted` is not read. It bears on #142's per-chat notification settings | `storage.rs` | Notifications differ from the phone's |
| G5 | **Smaller things the phone says:** view-once opened elsewhere, hidden or removed contacts, contacts no longer registered, preferred reaction emoji, "prefer contact photos" | `receive.rs` and `storage.rs` | Small differences, each visible |

Not gaps: call history (b33p has no calls), payments, backups and subscriptions.

## Step 1: an inventory that fails when something is new (first; it makes the rest checkable)

A test in b33p-core lists every field of `ContactRecord`, `GroupV2Record` and `AccountRecord`,
and every `SyncMessage` content type. Each one is marked:
- **taken**: with the function that takes it;
- **ignored**: with the reason, such as "b33p has no calls";
- **to do**: with this plan's number.

The test reads the field numbers from the prost-generated descriptors, or from the `.proto` files
in the libsignal-service checkout, so no list is kept by hand. It fails on any field without a
mark. Every fork bump then states what is new, the way `presage_migrations_only_ever_grow` does
for presage's tables. The inventory is also the honest answer to "what else is missing", in one
place.

Proof: the test, which fails until G1–G5 are marked "to do". Size: small.

## Step 2: the fixes, one branch each

### G1 Default timer
- Read `universalExpireTimer` with the account record, keep it beside the account preferences,
  and send it to the UI as part of `Preferences`.
- Apply it as Signal-Android does: a **new** 1:1 chat started here (`find_number`, or a first
  message to someone b33p has no history with) and a group created here start with that timer.
  The first message carries it, as a timer change does today (`commands.rs:1251`). Existing
  chats are left alone.
- Settings shows it, read only at first: "Default timer for new chats: 1 week (set on your
  phone)". Changing it from b33p is a write to the account record and can come later.
- Tests:
  - core tests: a new chat takes the default; an old chat doesn't; zero means off;
  - test box, storage phase: Android sets a default timer (Settings → Privacy → Default timer
    for new chats; new driver steps), b33p creates a group, and Desktop and Android show the
    group with that timer.

### G2 Profile keys from the storage service
- Read `ContactRecord.profileKey` (32 bytes) and hand it to presage's store when it holds none
  for that contact, or a different one. The phone's record is the authority: Signal-Android
  writes the key it holds.
- Then fetch the profile as `avatars.rs` does for new keys, for the picture and the about text,
  and the profile name as a fallback after the storage names of #149.
- presage's store has the setter (`Store::upsert_profile_key`, `store.rs:378`), so no presage
  change is needed.
- Tests:
  - a core test with a record carrying a key;
  - a test box check: b33p is linked without the history transfer after Android and the peer
    wrote, so the peer's key reaches b33p only through storage. The peer's profile picture must
    show in b33p before the peer writes again. This needs linking without transfer in the
    Android setup, which `setup.rs` may already choose; check.
- First, measure on the owner's account how many contacts lack a key: one debug log line at
  startup, "N contacts without a profile key". This sizes the problem before the fix.

### G3 Deleting for me, both ways
- **Out**: `Command::Delete { for_everyone: false }` also sends a `SyncMessage::DeleteForMe` with
  the message's author and time, as Signal-Desktop does (`DeleteForMe.preview.proto`). Deleting
  a chat here, if b33p offers it, sends a `ConversationDelete` with the chat's most recent
  messages, as Signal's apps do.
- **In**: `receive.rs` takes `DeleteForMe` itself, presage's handling being partial:
  - message deletes reach the UI as `Event::Deleted` (local only);
  - conversation deletes remove the chat's history and hide the chat until a new message;
  - attachment deletes drop the attachment;
  - local-only conversation deletes, the same.
- Tests:
  - core tests for each of the four kinds;
  - test box, Android box: Android deletes a message "for me" and b33p's window drops it; b33p
    deletes one "for me" and Android's chat drops it. New driver steps: long press, Delete,
    "Delete for me".

### G4 Mentions in muted groups
Read `dontNotifyForMentionsIfMuted` per group. With #142's per-chat settings, a muted group still
notifies for a mention unless this is set, as on the phone. Proof: a core test in `reading.rs`.

### G5 The smaller things
One branch, each with a core test:
- a view-once message opened elsewhere shows as opened (`ViewOnceOpen`);
- `ContactRecord.hidden` keeps the contact's chat out of the list until a new message;
- `unregisteredAtTimestamp` marks the contact as no longer on Signal, and b33p doesn't send to
  them;
- `preferredReactionEmoji` orders the reaction bar.

## Step 3: an old account in the test box

A fixture that ages the Android account before the phases run:
- address-book contacts on the emulator (`adb shell content insert`), with names other than the
  profiles', for the peer and a presage-cli member;
- a nickname, from #149;
- a default timer;
- a contact who never wrote;
- a deleted message;
- a muted group set to mention-only.

Then one check per gap that b33p shows what Desktop shows. Open question: whether
Signal-Android's contact discovery finds address-book contacts against the test box's server. If
it doesn't, the address-book names come through the storage service anyway (Android writes
`systemGivenName`), which is what b33p reads.

## Order, and what to decide

1. Step 1, the inventory: small, and it decides nothing.
2. G1, the default timer, and G3, deleting for me. Both are about what disappears, so both are
   about privacy.
3. G2, profile keys: measure first.
4. G4 and G5.
5. Step 3, alongside, as each fix needs its check.

To decide:
- **G1:** whether b33p only follows the phone's default, or can set it too (a write to the
  account record).
- **G3:** none really. The menu already promises "Delete for me", which in Signal's apps deletes
  on every device of yours; b33p should keep that promise. Say so if it should stay on this
  device only; the label would then have to change.
