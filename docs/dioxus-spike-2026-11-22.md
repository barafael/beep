# The Dioxus spike: b33p in a WebView, against the Slint client

> Recorded 2026-11-22, when `b33p-dioxus` was removed. Its last state is the tag
> `archive/2026-11-22/b33p-dioxus`.

`b33p-dioxus` was b33p for Android: the screens drawn by [Dioxus](https://dioxuslabs.com) 0.7
in the system's WebView, over the same `b33p-core` as the Slint client, packaged by `dx`. It
also ran on the desktop (`--features desktop`, WebKitGTK), which made the two comparable there.

It was removed because neither place needs it: on a phone, Signal's own app is a good client
already, and on the desktop Slint is the better product by every measure that matters at run
time (below). What it showed about the core stays true: a second client over `b33p-core` took
1,300 lines of Rust, and the helpers it shared with Slint moved into the core
(`list_time_formatter`, `count_reactions`, `read_only`, `avatar_color`, `initials` and the like).

Nothing of the crate was taken into the core or into Slint: Slint has its own of everything the
spike had (the clock, the start screen's texts, event batching, the backend dying), its
accessible names are fuller (a chat row says unread, request, pinned, muted and typing), and the
desktop's log scrubs every line as the spike's did.

## How it was measured

On the development machine (Linux, KDE Plasma on Wayland), `testbox` builds (release with debug
assertions, which the mock's scale needs) on the mock with `B33P_MOCK_SCALE=chats=200,messages=20000`,
and `incoming=5` (five messages a second) for the load. A sampler ran each client and read every
0.5 s, from `/proc`, the CPU time and the memory (PSS and RSS, `smaps_rollup`) of its whole
process tree, the WebKit helper processes included: [dioxus-spike-2026-11-22-measure.py](dioxus-spike-2026-11-22-measure.py).
Opening a chat happened 20 s after the start: Slint through its agent socket (from the request to
the first redraw that shows at least 60 messages), Dioxus through a temporary hook (from the
selection to two animation frames after the page changed, which counts ~33 ms more by itself).
"Steady" CPU is from 25 s after the start to the end.

A locked screen stops WebKit painting: one run made that way was thrown away (`loginctl`'s
`LockedHint` stayed `yes` after unlocking; the screensaver's `GetActive` is the one to ask).

## Desktop

Second run (16:30, three rounds, medians):

| | Slint | Dioxus (WebKitGTK) |
|---|---|---|
| Chat list drawn after start | 2.01 s | 2.30 s |
| Opening a 60-message chat, idle | 12 ms | 95 ms |
| The same while messages arrive | 3 ms | 98 ms |
| CPU to start / to open the chat | ≈0.3 s / ≈0.2 s | ≈3.2 s / ≈2.5 s |
| CPU idle, chat open | 0.4 % of a core | 0.9 % |
| CPU with 5 messages a second | 3.9 % | 42 % (⅔ the app, ⅓ WebKit's renderer) |
| PSS idle / under load | 86 MB / 81 MB | 428 MB / 495 MB (peak 579) |
| RSS | 117–122 MB | 820–940 MB |
| Processes | 1 | 3 |

The first run (12:13, two rounds, the same binaries and settings) had Slint the same (3.5–4.9 %
under load, 72–89 MB PSS) but Dioxus much lighter: 13–19 % under load, 239–284 MB PSS, 452–532
MB RSS, 48–77 ms to open the chat. The spike's last code change (accessible names, a live region
on the message list) is not why: the first run's binary, measured again at 16:30, gave the
second run's figures (46 %, 516 MB). What else differed (most likely the window's size, or its
being covered) was not found; take Dioxus's cost as somewhere in that range, and 3 to 10 times
Slint's CPU and 3 to 6 times its memory.

Under load every message makes Dioxus diff the whole screen in the app's process, then patch the
DOM and lay out the page in WebKit's renderer; Slint changes its model and redraws one region.

## Size and build

| | Slint | Dioxus |
|---|---|---|
| Desktop release, with Signal | 53.2 MB, 15 shared libraries | 25.6 MB, plus ~130 MB of the system's WebKitGTK; 146 shared libraries |
| UI only (the mock) | 35.5 MB | 4.9 MB |
| Android APK, arm64 with Signal | – | 17.0 MB (21.5 MB native library uncompressed, 11.5 MB of `dx`'s Java shell) |
| Android APK, x86_64 mock | – | 8.6 MB |
| Clean release build | 411 s (UI only 299 s) | 230 s (UI only 50 s) |
| Crates in the build | 681 (UI only 436) | 626 (UI only 356) |

The release profile's `opt-level = "s"`, measured on Slint at the same time: 53.2 MB; `3`: 73.2
MB; `"z"`: 51.7 MB.

## Code

| | Slint | Dioxus |
|---|---|---|
| Lines (comments included; the core's 36k not counted) | 17.8k (12.5k Rust, 7.9k `.slint`) | 1.85k (1.3k Rust, 205 CSS) |
| Files | 49 | 10 |
| Tests | 109 | 6 |
| Core methods called | 185 | 35 |
| Framework | a fork of Slint (7 open upstream PRs and text work) | Dioxus 0.7 from crates.io |

Dioxus was small because it drew the whole page from the state each time, and had a fraction of
the screens: no attachments beyond labels, groups, search, details, stories, stickers, settings,
pictures or icons (emoji stood in). Slint is large where it is fast (redrawing only what changed),
and where it is a desktop app (tray, badge, updates, about 30 timers and threads).

## Android (emulator, Android 15 x86_64, release mock)

- Memory: about 172 MB PSS, the app's process 110 MB and the WebView's renderer 62 MB.
- Ran against Signal as far as the code to link with (SQLCipher, BoringSSL and TLS all worked);
  never on a phone, never linked.
- TalkBack read every label (delivery ticks, reactions, pinned, muted, unread counts, the message
  log); the window's name came from the page's title.

## What Android asked of it, if b33p goes there again

- **Static C++ runtime.** `boring-sys` links `libc++` as a shared library the APK doesn't carry:
  the app died at start (`UnsatisfiedLinkError … libc++_shared.so`). `.cargo/config.toml`'s
  `BORING_BSSL_RUST_CPPLIB_<target> = "c++_static"` fixed it; a changed `[env]` does not rebuild
  `boring-sys` by itself.
- **No places for files, no keyring.** `directories` knows nothing on Android: the app's
  directory (`/data/data/<package>/files`) came from `/proc/self/cmdline` and was handed to the
  core (`settings::use_app_dir`, removed with the spike). With no Secret Service the core fell
  back to a password; the Keystore would be the place for the key.
- **No backup.** Android's default `allowBackup` would copy the database and keys to Google
  Drive and restore them elsewhere: one Signal identity on two devices.
- **The activity made anew.** Android destroys and recreates an activity on a configuration
  change unless the manifest claims it; tao (under Dioxus) ends its event loop with its only
  window, and the process exited. `configChanges` can claim all of them but one: the theme
  overlays Android applies when the wallpaper's colours change, which still closed the app.
- **Dark mode.** The WebView's `prefers-color-scheme` follows the activity's theme: `dx`'s is
  always light, so `Theme.AppCompat.DayNight` was needed.
- **Inputs uncontrolled.** A value written back into a text field races with the keystrokes and
  the keyboard's composition and drops them; fields only read theirs, and were emptied from
  JavaScript. A `key` outside a list does nothing in Dioxus, so re-keying can't reset one.
- **Back** is the WebView's history: opening a chat pushed an entry, and `popstate` closed it.
- **`dx`'s Gradle project** never removes a library built for another target (an x86_64 "mock"
  APK carried a stale arm64 real client), and Gradle packs into the old APK, leaving what was
  removed as dead space (a phone APK grew from 17.0 to 27.6 MB). Clearing `jniLibs` and the APK
  before each build fixed both.
- **The APK `dx` makes is debug-signed and debuggable** (`adb` can read the app's files): a
  build for others needs its own signing.
- **Checking it:** `uiautomator dump` shows the WebView's accessibility tree only with TalkBack
  on (and TalkBack's notification permission granted, or its dialog hides the app).
