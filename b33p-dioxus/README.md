# b33p-dioxus

b33p for Android: the same Signal client as `b33p-slint` and `b33p-gpui`, with its window
drawn by [Dioxus](https://dioxuslabs.com) in the system's WebView. Everything that isn't the
window is `b33p-core` (the chat state and the backends); this crate is the screens, written
for a phone: the chat list, then the chat opened from it, and Back to return.

It builds as a library that Android loads (`libmain.so`), packaged into an APK by `dx`, which
also writes the small Kotlin shell around it.

## Status

Runs on the Android 15 emulator (x86_64) against both backends:

- **The offline mock** (build without `signal`): chat list, conversations with replies,
  reactions, attachments as labels, delivery ticks, a message request, sending, the keyboard.
- **The real backend** (`signal`): opens the encrypted store, asks for a password (there is no
  keyring on Android, see below), connects to Signal and shows the code to link with.

The arm64 build for phones links and has the right shape; it has not run on a phone yet.
Linking an account, and everything after it on this platform, is unchecked: PLAN.md has the list.

## Building

You need Rust with the Android targets, `dx` (the Dioxus CLI, 0.7), a JDK 17 or newer, the
Android SDK with its platform tools, and the NDK (r29 was used):

```sh
rustup target add aarch64-linux-android x86_64-linux-android   # phones; the emulator
cargo install dioxus-cli --locked
sdkmanager "platform-tools" "ndk;29.0.14206865"                 # Gradle fetches the rest
export JAVA_HOME=… ANDROID_HOME=… ANDROID_NDK_HOME=$ANDROID_HOME/ndk/29.0.14206865
```

`android/android.sh` does all of this, and undoes it, without root: `setup` installs what is
missing (JDK, SDK and NDK, emulator image and device, Rust targets, `dx`), `up` and `down` start
and stop the emulator (and Gradle's daemons, and the adb server if idle), `status` shows both,
`build`, `install`, `run`, `logs`, `shot` and `reset` drive the app, and `clean` and `purge` remove
the build output and the whole toolchain. `android.sh help` lists them.

`dx doctor` says what it finds. Then, from the repository's root (the `android` group of
`just --list` has the same as recipes):

```sh
# the offline mock, for the emulator: builds in seconds after the first time
dx build --android --target x86_64-linux-android -p b33p-dioxus --features mobile
# the real client; arm64 for a phone
dx build --android --target aarch64-linux-android -p b33p-dioxus --features mobile,signal
adb install -r target/dx/b33p-dioxus/debug/android/app/app/build/outputs/apk/debug/app-debug.apk
```

The first real build compiles libsignal, BoringSSL and SQLCipher for Android, which needs `cmake`,
`clang` and `protoc` on the host as the desktop builds do; the debug APK is large (the libraries
carry their debug information).

Without `--features mobile` the crate builds no renderer, so `cargo build --workspace` on a desktop
needs no WebView libraries. `--features desktop` draws the same screens in a desktop window
(`webkit2gtk` on Linux), which is quicker for working on the look:
`cargo run -p b33p-dioxus --features desktop`.

## What is different on Android

- **Where things are kept.** The app's own directory, `/data/data/<package>/files`, which no other
  app can read. `android.rs` finds it from the process's name and hands it to
  `b33p_core::settings::use_app_dir`; `directories` knows no such place.
- **No keyring.** The credential store b33p uses on the desktop is the Secret Service, which
  Android has not; the core falls back to what it does when the keyring is locked, a key derived
  from a password. The Android Keystore would be the right place for the key, and is not used yet.
- **No backup.** The manifest sets `allowBackup="false"`: Android's backup would copy the database
  and its key material to Google Drive and restore them on another device, which would put one
  Signal identity on two. Linking a device is how b33p gets onto a new phone.
- **Static C++ runtime.** `.cargo/config.toml` makes BoringSSL's `libc++` static. Left alone,
  `libmain.so` needs `libc++_shared.so`, the APK doesn't carry it, and the app dies at start with
  `UnsatisfiedLinkError … libc++_shared.so not found`.
- **Back** is the WebView's history: opening a chat pushes an entry, Back closes the chat, and
  from the list it leaves the app.
- **Inputs are uncontrolled.** A value written back into a text field races with the keystrokes
  typed meanwhile and drops them, so the fields only read theirs.
- **Logs** go to logcat (`adb logcat -s RustStdoutStderr`), scrubbed of messages, names and
  numbers as the desktop's file log is, but stricter: `info` and above, no `RUST_LOG`.
- `Dioxus.toml` names the package (`io.github.barafael.b33p`); `android/AndroidManifest.xml` is the
  manifest `dx` merges its own parts into.

## Not done yet

What the desktop clients have and this doesn't, by what it costs most not to have:

- Notifications, and what happens when Android stops the app: today the backend lives and dies
  with the process, so nothing arrives while the app is closed.
- The Keystore for the database key; attachments (viewing, saving, sending: the file picker and
  the camera); contacts' pictures (raw pixels now, a letter badge shown).
- Groups, search, details, forwarding, stories, stickers, settings: the screens, not the core.
- Text styling in messages (the state has the ranges), links, mentions.
- Older messages as one scrolls up.
- A shared logger with the desktop's (`b33p-platform` pulls in desktop-only crates).
- CI: no job builds this, as it needs the NDK.
