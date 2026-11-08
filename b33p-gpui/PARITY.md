# Parity of the two windows

`b33p-slint` (the original) and `b33p-gpui` are two windows on the same client: everything that
is not the window is shared (`b33p-core`, `b33p-platform`, `b33p-notify`, `b33p-harness`). This
says, feature by feature, what each window does, how that was checked, and what differs.

How a row was checked: **code** is a reading of the Slint code (`b33p-slint/ui/*.slint`,
`src/`) against the GPUI code; **test** is a test of the GPUI window that checks it (and the Slint
one that checks it, where there is one: see [Tests](#tests)); **display** is the window driven
in a private display (`testbox/gpui-view`, a cage compositor in a container, never the desktop).
One mechanical check backs all of the data rows: every field of `ChatState`'s views
and of the `b33p-core` types that the Slint window reads is read by the GPUI window too, and every
`ChatState` method one calls the other calls (`comm` over the two sources; the only
difference is `ChatState::conversation`/`reveal`, which Slint reaches by another path).

Legend: yes = same behaviour; differs = same purpose, other form (see notes); no = not there.

## The start, and what goes wrong

| Feature | Slint | GPUI | Notes |
|---|---|---|---|
| Syncing screen: status per stage, spinner, "Skip" for the history transfer, "Start" once catching up takes over 1.5 s | yes | yes | code: `start.rs` `syncing`, `screens.slint` |
| Password screen: new password with confirmation, wrong password, "Forgot your password?" asks before starting over | yes | yes | code |
| Link screen: QR code, the steps to scan it | yes | yes | code. The GPUI code had no screen-reader name; it has one now ("The code to scan", test `things_say_what_they_are_to_screen_readers`) |
| Failure screen: message, "Try again" (when it may work), "Use a password instead" (locked keyring only), "Start over…", "Open data folder" | yes | yes | test `a_failure_offers_a_way_out`; e2e `a_failure_can_be_tried_again` |
| Asking before what can't be undone (unlink, start over, leave group, block or delete a request, delete for everyone); Cancel is the default, Esc cancels | yes | yes | test `a_failure_offers_a_way_out`; code `confirm.rs` |
| Error banner: the message, "Try again" only when that may work, "Dismiss" | yes | yes | tests `the_error_offers_to_try_again_when_that_may_work`, `a_long_error_wraps`. A long message did not wrap in GPUI (the text ran out of the window): fixed |
| Error banner slides open and closed | yes | yes | `app.rs` `banner`, kept while it slides away; to a guessed height (a line, plus a half for each hundred characters), then its real one. Tests `the_error_banner_slides`, `the_error_banner_does_not_slide_with_motion_off`; see [Animations](#animations) |
| Error banner is announced at once by a screen reader | text role | alert role | GPUI: `Role::Alert` |
| "Not connected" / "Catching up…" line over the chat list | yes | yes | code |
| The backend ending without a word becomes the failure screen | yes | yes | code `main.rs` |
| Unlinking and starting over go back to linking | yes | yes | e2e `unlinking_starts_again_with_linking` |
| The mock says so in the window title | "b33p — MOCK (nothing is sent)" | "b33p · MOCK" | the words differ |

## The window, the tray, the desktop

| Feature | Slint | GPUI | Notes |
|---|---|---|---|
| Size: 1000 x 680, at least 360 x 480; one pane at a time under 640 wide | yes | yes | code |
| The settings page next to the chat list shrinks with the window | yes | yes | test `settings_fit_a_narrow_window`. It did not in GPUI below about 850 wide (controls ran out of the window): fixed |
| Sidebar is 300 wide and can be dragged between 220 and 480 | yes | yes | code |
| Closing the window leaves b33p in the tray (setting "Keep running in the tray"), or ends it where there is no tray | yes (hides it) | yes (closes it) | GPUI windows can't be hidden: "in the tray" is no window and the process running, and opening it again opens a window. Everything shown is in `Shared`, so nothing is lost |
| Tray icon: "Open b33p" and "Quit", the unread dot and the tooltip with the count, shown only with the setting | yes | yes | code `tray.rs` (a StatusNotifierItem through `ksni`; `tray-icon` on Windows and macOS) against `tray.slint`; unit tests of the icon and the tooltip |
| `--hidden` starts in the tray, and opens the window if no tray host appears in 10 s or the user is needed | yes | yes | code `window.rs` |
| One copy of b33p; starting it again shows the first | yes | yes | code. A Wayland activation token handed over by a second start or a notification is given to the compositor: GPUI takes one only when it connects, so with a token and no active window the window closes, GPUI goes headless and windowed again with the token (`GraphicalEnvironment::activation_token`, public API made for this) and a window opens (`window.rs` `reconnect_with`). Without a token the window is closed and opened anew, as before. Display: the window comes back, draws and the agent answers; the private display's compositor (cage) has no `xdg_activation_v1`, so that the compositor took the token is not seen. The new connection drops a selection copied to the clipboard from b33p |
| Notifications: setting, content (name and message, name only, nothing), click opens the chat, inline reply, withdrawn once read | yes | yes | the same `b33p-notify` and `ChatState` notices |
| A chat's own notification settings: mentions only, no text, no sound | yes | applied, not offered | the notices follow them (`shared.rs`, `b33p-core` `reading.rs`) and the agent sets and reports them (`SetChatNotify`); the chat's details have no place to change them yet |
| Starting with the system | yes | yes | `b33p-platform` |
| Quitting cleanly on SIGTERM, SIGINT, SIGHUP | yes | yes | code `shutdown.rs` |
| Quitting cleanly at Windows' session end (`WM_ENDSESSION`) | yes | yes, not run | Windows only: `shutdown.rs` (`windows_session`) is Slint's window procedure on a thread of its own, telling the app to quit and waiting up to 4 s for the backend. Compile-checked (the module alone, `cargo check --target x86_64-pc-windows-msvc`: the whole crate can't be checked here, its C dependencies need the Windows SDK); never run on Windows |
| The window kept out of screenshots, screen sharing and Recall (`SetWindowDisplayAffinity`) | yes (Windows) | yes, not run | Windows only: `capture.rs`, through the window's raw handle (`HasWindowHandle` on GPUI's `Window`) as the window opens. Compile-checked as above, never run on Windows |
| Reduced motion: "Animations" = As the desktop says / On / Off, following the desktop live | yes | yes | see [Animations](#animations) |
| Theme System / Light / Dark; "System" follows the desktop as it changes | yes | yes | GPUI took the scheme only when b33p started or the setting changed: it follows `observe_window_appearance` now (not testable: the test platform has no way to change the appearance) |
| Text size small / default / large | yes | yes | code |
| Qt widget style (system or Fusion) | yes | no | Slint on Qt only |
| Window title bar, resize, drag | server side | server side | |

## Chats

| Feature | Slint | GPUI | Notes |
|---|---|---|---|
| Chat list rows: avatar or initials, title, preview with its kind icon, time, unread count, pinned, muted, marked unread, "Message request ·", typing, "Blocked" | yes | yes | tests `chat_rows_speak_for_themselves`, `the_chat_list_follows_events` |
| A chat's row is one item for a screen reader: "Bob, 2 unread, muted, See you", selected, opens with the default action | yes | yes | same label; GPUI gave the label without a role, so no screen reader heard it: fixed. Test `chat_rows_speak_for_themselves` |
| Row menu: pin, move up and down, mark read or unread, mute for 1 hour, 8 hours, 1 week or always, unmute, details, media, block, archive | yes | yes | code `chat_list.rs`, `conversation.rs` |
| Folders as tabs with unread counts, "All"; archived chats entry with count | yes | yes | code. Screen-reader names ("All chats", "Work, 2 unread", "Archived chats, 1") and the selected state were missing in GPUI: added. Test `things_say_what_they_are_to_screen_readers` |
| Search chats; arrows and Enter go through and open the results; Esc clears | yes | yes | code; test `a_typed_number_can_be_messaged` |
| A phone number in the search offers "Message +49 …", which looks it up | yes | yes | test `a_typed_number_can_be_messaged` |
| Ctrl+K searches, Ctrl+F searches the chat, Alt+Up/Down previous and next, Alt+Shift+Down next unread, Alt+1 to 9 pinned, Esc closes the innermost open thing | yes | yes | test `the_shortcuts_go_through_chats_and_close_what_is_open`, `shortcuts_work_from_the_composer`. Two real faults found: in a fresh window nothing had the focus, so no shortcut worked until the first click; and once the focused view left the window (the composer, when settings open in its place) Esc went nowhere. Both fixed in `app.rs` |
| The shortcuts listed in settings | 12 rows | the same 12 | code |
| Empty state: "Pick a chat on the left …" with the keys | yes | yes | code |
| New group, add members, join through a link, story button | yes | yes | code; e2e `a_group_is_joined_through_its_link` |

## The open chat

| Feature | Slint | GPUI | Notes |
|---|---|---|---|
| Header: avatar, title, subtitle (number, size, "Messages to yourself"), who is typing, disappearing-messages timer, details, search, activity calendar, menu | yes | yes | the timer says "Disappearing messages: 1 hour" to a screen reader in both (GPUI: added) |
| Messages: bubbles with runs (tight corners), day separators, "New messages" line, sender name and avatar in groups, time on the last of a run | yes | yes | code |
| Notices (safety number changed, timer changed) as a centred line | yes | yes | test `a_notice_is_drawn_as_a_line_not_a_message` |
| Markdown (bold, italic, strike, monospace), spoilers revealed on click, links, mentions | yes | yes | code `text.rs` against `markdown.rs`; the unit tests of each |
| Selecting text in one message and Ctrl+C; selecting in another drops the first | yes | yes | test `ctrl_c_copies_the_text_selected_in_one_message` |
| Quote of the replied-to message; a click on it shows the quoted message (scrolls there, loading older history first if needed, and marks it); the quote has a hover state and is named "Show the quoted message from Ann" | yes | yes | tests `a_click_on_a_quote_shows_the_quoted_message`, `a_quote_of_an_older_message_loads_it_first`; e2e `a_quote_shows_the_message_it_quotes`; display (the marked message after the jump). The mock's quote now says which message it quotes |
| Link preview card (picture, title, description, site), click opens | yes | yes | code; named "Open link …" to a screen reader (GPUI: role added) |
| Attachments: pictures (blurhash placeholder, fetch, retry), videos, GIF label, audio and voice messages with play, seek and time, files with Open and Save, stickers, long text | yes | yes | e2e `attachments_are_fetched_and_saved`; code. The audio position is a slider with its value to a screen reader in both (GPUI: added) |
| Reactions: pills with counts, yours marked, click toggles; picker with every emoji | yes | yes | code |
| Reacting: a row of quick reactions (six emoji and "+" for every emoji) over the bubble's top edge after the pointer has rested on the message 350 ms, kept while the pointer is on the message or the bar, gone with it | yes | yes, and "React" among the buttons that show on hover | `reaction_bar.rs`. Display: real pointer moves over the VNC socket (bar after the dwell, pointer on to it keeps it, "+" opens the picker above, an emoji reacts and closes both); tests `the_reaction_bar_comes_after_a_dwell`, `the_reaction_bar_goes_with_the_pointer`. While the pointer is on the bar the message's own buttons are hidden (they are drawn for the hovered row only) |
| Delivery ticks (sending, sent, delivered, read), "Sending…", "edited", the time a message has left | yes | yes | named for screen readers in both ("Sent", "Disappears in 50m"); GPUI: roles added; test `things_say_what_they_are_to_screen_readers` |
| A failed message says so on every bubble that failed and offers "Send again"; one Signal refused doesn't | yes | yes | tests `a_failed_message_offers_a_retry`, `the_window_shows_the_chat_state`; e2e `failures_offer_to_try_again_when_that_may_work` |
| Message menu: select, reply, react, copy selection, copy text, forward, edit, delete for me, delete for everyone (asks) | yes | yes, and also "Save attachment", "Send again", "Info" | GPUI's has three more entries; the pin entries are in the rows below |
| Picking messages (Ctrl+click, Shift+click up to one, "Select") gets a bar: count, copy, forward (only when all can go), delete, clear | yes | yes | test `picked_messages_get_a_bar_to_act_on_them` |
| Forwarding: pick a chat, then confirm "Forward to X?" | yes | yes | test `forwarding_goes_to_the_clicked_chat_once_confirmed` |
| Older messages load as the top is neared, and the view stays where it was | yes | yes | tests `scrolling_to_the_top_asks_for_older_messages`, `older_messages_put_in_above_leave_the_view_in_place` |
| Jumping to a message (search result, quote) scrolls it into view | yes | yes | tests `jumping_brings_the_row_into_view`, `clicking_a_search_result_shows_it` |
| The jumped-to message flashes, then settles into its highlight | yes | yes | added in GPUI (`bubble.rs`), with motion off it is only the highlight: test `a_message_jumped_to_flashes_unless_motion_is_off` |
| Pins: the bar above the messages (one pin at a time, newest first, a mark per pin when there are several, the pin's author and first line or kind, a click shows it in the chat and turns to the one pinned before, a button to unpin where messages can be pinned, "Pinned message 2 of 3: Ann: text" for a screen reader) | yes | yes | tests `the_pin_bar_shows_one_pin_at_a_time`, `the_pin_bar_unpins_for_those_who_may`; e2e `messages_are_pinned_and_unpinned`; display with 1 and 3 pins. One difference: the bar's unpin goes by the pin itself, so it works for a pin whose message is in older history that isn't loaded (Slint finds the message's row and does nothing then) |
| Pins in the message menu: "Pin" with "For 24 hours", "For 7 days", "For 30 days", "Until unpinned"; "Pin, replacing the oldest pin" when three are pinned; "Unpin"; not for notices, deleted or unsent messages, nor where one may not pin | yes | yes | test `the_menu_offers_pinning_where_it_is_allowed` (the offer, and the command each choice sends); display of the menu. A bubble draws nothing for being pinned, as in Slint |
| A pin that runs out leaves the bar at that moment | looks every 30 s | at the moment: a timer set from `ChatState::next_pin_expiry` after each draw, none while no pin runs out | test `a_pin_leaves_the_bar_when_it_runs_out`. `Update::pins` redraws the window like any update |
| Messages over 800 characters fold to ten lines with "Read more"; the rest comes on a click, for that message | yes | yes | test `a_long_message_folds_until_read_more`; display folded and expanded. Slint's row is reused as it scrolls, so a message folds again when scrolled away; GPUI keeps it open until the chat is shown anew (`Shared::expanded`). A folded message's text can't be selected, as in Slint |
| Message info: "Info" in the menu of our own sent messages opens a dialog (who got it, when each got and read it; "Finding out who received it…" while gathering; Esc, a click outside or "Close" closes it) | no (core only) | yes | designed here, plain and in Signal's style; tests `the_info_of_a_message_of_ours_lists_who_got_it`, `a_recipient_is_read_delivered_or_not_reached`; e2e `a_message_s_info_lists_who_got_it`; display with one and four recipients. Slint gets its own later: the texts are in `message_info.rs` |
| Readers: under our newest message that group members have read, their small pictures (six, then "+n"), named "Read by Ann, Bob" | no (core only) | yes | test `readers_show_under_the_newest_message_they_read`; display. The mock's group now has an own message read by two members |
| Button to the newest messages, counting those that came meanwhile | yes | yes | code |
| Typing indicator under the title and in the list | yes | yes | code |
| Disappearing messages: timer in the details, per-message countdown, counted down every minute | yes | yes | code `shared.rs` `expiry_timer` |
| Times that change as time passes: the chat list's "14:05" becomes "Mon" and the day separators move on at midnight; the countdown of a disappearing message once a minute while the open chat has one; nothing else (a bubble's time is the time of day it was sent; Slint's `sent_minute` field is carried but no `.slint` file reads it) | yes | yes | code: the same two timers (`midnight_timer`, `expiry_timer`), and nothing runs while there is nothing to change, so an idle window costs nothing. No relative label ("2 min ago") exists in either window, so no further tick was added |

## Writing

| Feature | Slint | GPUI | Notes |
|---|---|---|---|
| Composer: Enter sends, Shift+Enter a new line, Up in an empty one edits the last message, Esc cancels reply or edit | yes | yes | e2e `a_sent_message_is_edited`; code |
| Reply and edit bars; staged files with remove | yes | yes | code |
| Mentions: "@" offers the group's members, Tab or Enter or a click picks | yes | yes | test `clicking_a_mention_candidate_picks_it` |
| Emoji picker with groups, recents and a search; an emoji goes where the caret is, in place of the selection | yes | yes | test `an_emoji_goes_where_the_caret_is` |
| Sticker picker: packs, stickers, send | yes | yes | test `the_sticker_button_asks_for_the_packs` |
| Attach files; paste a picture, files or text; drop files on the chat | yes | yes | code `composer.rs` (`ask_pick`, `ask_paste`, `accepting_files`) against `desktop.rs` |
| Composer's own menu (cut, copy, paste, select all) | yes | yes | the component library's input has it |
| A chat opens ready to write in, wherever it was opened from; and again after a dialog, a popup or Esc | yes | yes | code `composer.rs` `render` |
| Bars instead of the field: request (Block, Delete, Accept), invitation (Decline, Accept), asked to join (Take request back), left, changed safety number (Show safety number, Continue), blocked (Unblock) | yes | yes | tests `an_invitation_is_answered_in_the_chat`, `a_changed_safety_number_holds_the_composer` |
| Send, edit, answer: round trip through the mock | yes | yes | e2e `a_sent_message_is_answered` |

## Panels and dialogs

| Feature | Slint | GPUI | Notes |
|---|---|---|---|
| Details: about, phone, safety number with Copy and "Mark as verified" or "Clear verification", the changed-number warning, group description, members with admin, remove, picture, rename, invite link (off, anyone, with approval), join requests, leave | yes | yes | tests `the_safety_number_can_be_marked_verified`; e2e `a_safety_number_is_verified_and_a_chat_searched`, `the_device_can_be_renamed`; code |
| Chat search panel: results with sender, time and the words around the match, a click shows it | yes | yes | tests `clicking_a_search_result_shows_it`; e2e |
| Media, files and links of a chat | yes | yes | code `media.rs` |
| Activity strip and calendar (settings: off by default) | yes | yes | code `activity.rs` |
| Stories: rings, viewer with arrows and position, text stories, post a text or picture story to a group | yes | yes | test `a_text_story_can_be_clicked_through`; e2e `stories_are_shown_in_turn` |
| New group and add members: name, filter, choose people, a number looks someone up | yes | yes | code `group_draft.rs` |
| Join a group by link: look up, ask or join, take the request back | yes | yes | e2e `a_group_is_joined_through_its_link` |
| Settings: Appearance (theme, style, text size, animations, profile pictures), Chats (strip, calendar), Notifications, Privacy (read receipts, typing, link previews), Background (tray, autostart), shortcuts, Account (sync to phone, device name, unlink), About (source, report a problem, log folder) | yes | yes, but no "Style" | choices are segmented buttons in GPUI, a combo box in Slint. Tests `settings_say_where_to_report_a_problem`; e2e `settings_can_be_changed`, `the_device_can_be_renamed` |
| Phone changes to the account's settings show up on their own | yes | yes | code `take_preferences` |
| Tooltips on icon buttons | yes | yes | test `icon_buttons_have_tooltips` |

## Animations

The window's "Animations" setting (As the desktop says / On / Off) is applied. In Slint it scales
`Motion`'s durations. In GPUI it sets the app's reduced-motion flag (`window::apply_motion`),
which GPUI and the component library read before they animate anything: the spinners, the
tooltips, the scrolling and the caret blink; the desktop's own wish comes through `b33p_platform`'s
watcher. It was applied already (the first port's note that it was only stored was out of date);
test `the_animations_setting_decides_whether_things_move` checks every combination, and the
flash below is checked to stop moving with it off.

What moves in Slint, and in GPUI:

| Animation | Slint | GPUI | Notes |
|---|---|---|---|
| A message jumped to flashes, then settles into its highlight | yes | yes | carries meaning (it is how the eye finds it): added |
| Spinners, tooltips, caret blink | yes | yes | the component library's, which obeys the flag |
| Hover and press colour changes | fades | at once | cosmetic |
| A photo opens in b33p's own viewer, over the window: next and previous, zoom, the download's state | yes | no | GPUI opens it in the system's viewer, as before; the agent refuses the viewer's actions and reports no viewer. The viewer's state is `b33p-core`'s (`state/viewer.rs`), so only the drawing is missing |
| A new message fades in | yes | no | the row's fresh state is not in the view model GPUI reads; cosmetic |
| Error banner slides open and closed | yes | yes | `app.rs`; the closing one is kept for the length of the slide, with no alert role |
| Dialogs and panels fade in | yes (dialogs also grow from 97%) | fade only | `widgets::modal`, `panels.rs`; GPUI has no element transform, so no growing |
| The selection bar and files staged for sending fade in | yes | yes | `message_menu.rs`, `composer.rs` |
| A new message fades in | yes | yes, but not those in a stream | `bubble::row`, from `Shared::arrived` (what the list was told). Not for one that came less than a second after another: a fade draws a frame at a time, and five a second would have the window drawing all the time |
| Reply and edit bars slide, the typing line and the unread badge fade or grow, the audio bar glides, pictures fade over their placeholder | yes | no | cosmetic; the first two would need the closing kept (as the banner's is), the audio bar the last position |

Idle: every one of these is GPUI's animation element, which asks for a frame at a time while it runs and not after; with motion off it draws the end state at once. Checked in the private display by counting `wl_surface.commit` requests (`WAYLAND_DEBUG=1`): 4 commits in the half second after a panel opens, then 0 in the next four seconds (the others in a window that is not typing show 0 to 2 in four seconds, the same as before). Test `dialogs_and_new_messages_fade_in_unless_motion_is_off` checks that the setting turns them all off.

## Accessibility

GPUI hands an element to a screen reader only when it has an id and a role: a label alone says
nothing. The first port gave ~25 elements a label and no role, so none was heard. They have roles
now, through `widgets::Speaks` (`.speaks(role, label)`, which also names the element for the
tests): buttons, links, list items with their selected state, tabs, the audio slider with its
value, the QR code as an image, the error banner as an alert, timers and delivery marks as text.
Not checked by a screen reader: the test platform can't turn the accessibility tree on, so the
tests find an element by its name, not its role.

## Not the same, and why

* **Settings choices** are segmented buttons (they look at home in GPUI); Slint's are combo boxes.
* **The animations in the last table** that are not done: hover colour fades, pictures and the
  audio bar, the reply and edit bars, the typing line and the badge. Cosmetic.
* **Windows and macOS** are untested for GPUI. The two Windows-only features above (session end,
  capture exclusion) are written after Slint's and compile-checked, not run. macOS (`cargo check
  --target aarch64-apple-darwin`) can't be checked here either: the target is installed, but the
  C dependencies need the macOS SDK.
* **Harness `screenshot`** is unsupported: the Wayland window can't give the frame back. GPUI's
  `Window::render_to_image` (feature `test-support`) is only implemented by its test window,
  which draws with a headless wgpu renderer when the app runs in `HeadlessAppContext`; the
  Wayland window's renderer (`WgpuRenderer`) has no read-back, and the scene it would draw is
  private to GPUI. What would make it work: a patch to `gpui-pre-linux`/`gpui-pre-wgpu` (the
  surface texture with `COPY_SRC` and `render_to_image` on the Wayland window), or running the
  harness build entirely offscreen. The display's screenshot (`testbox/gpui-view/view.sh shot`)
  is the way, as before. The error says so.
* **The Qt style** setting exists only with Qt.

In GPUI and not in Slint: the "Send again" and "Save attachment" entries of the message menu, and the message info and readers (core has them; Slint has no window for them yet).

## Tests

Slint's tests and where GPUI has the same:

| Slint | GPUI |
|---|---|
| `b33p-slint/tests/harness.rs` (13 tests, the real binary on Qt's offscreen platform) | `src/e2e_tests.rs`: the same 13, through the same protocol |
| `b33p-slint/src/ui_tests.rs` (27 tests, Slint's testing backend) | `src/ui_tests.rs`: the same 26 (the 27th is a callgrind workload, not a test: `bench.py` measures GPUI), and the ones named in the tables above |
| `b33p-slint/tests/theme.rs` (the look comes from `theme.slint`) | `theme.rs`'s `the_look_comes_from_the_theme`: no colour, text size or corner of a view's own outside `theme.rs` |
| `markdown.rs` property tests | `text.rs`'s own tests |

A window needs a display; CI has none and no GPU. Slint answers with Qt's offscreen platform.
GPUI has a test platform (`gpui`'s `test-support`: windows, layout, input, a clock you move) that
needs neither, so both kinds of test run on it, in the test binary, with no display:

* `src/testing.rs` brings the app up as `main` does, on that platform. The UI tests give the views
  a `ChatState` built from events and a recorder in place of the backend (`Commands::recorded`,
  added to `b33p-core`), find elements by name (`debug_selector`, which is a no-op outside tests;
  `widgets::Speaks` sets it with the screen-reader name), and click, type, scroll and drag with
  `simulate_*`.
* `src/e2e_tests.rs` runs the real agent on a Unix socket with the mock backend in the same
  process; the client is Slint's, on a thread of its own, while the test thread runs the app. The
  one test that needs a switch of the mock (`B33P_MOCK_FAIL`) runs in a process of its own, as the
  environment is shared by all tests of a process. These need `--features harness` (as Slint's
  do), which CI now passes for GPUI in its own step.

`gpui-kit`'s own `test-support` feature can't be used: it names `gpui-fast`, which needs a `taffy`
that Slint's fork rules out; the tests depend on `gpui-pre` with `test-support` directly (a few
dev-only packages come with it: `Cargo.lock`).

What the tests can't see: pixels (the test platform draws nothing), real fonts (text is laid out
at a fixed width per letter), the compositor, the accessibility tree, and the desktop's appearance
changes.
