# A web demo of the mock

Whether the mock backend (`B33P_MOCK=1`) can run in the browser, as a demo on GitHub Pages. Not
done yet; this records what a trial build for `wasm32-unknown-unknown` found (October 2026, at
0.2.0).

## What already builds

- `b33p-core` without `signal` builds for wasm once `gethostname` is left out there (it pulls
  `rustix`, whose `errno` has no wasm support). `default_device_name()` needs a stand-in, and
  `jiff` wants its `js` feature for the clock.
- All other dependencies of `b33p-slint` build too: the Slint fork with `backend-winit` and
  `renderer-femtovg` (what macOS and Windows use already), `i-slint-core`, `b33p-notify`,
  `tracing-subscriber`, `async-channel`, `png`, `blurhash`.
- Five don't, and the demo can do without them: `arboard` (clipboard), `open` (links),
  `mundy` (reduced motion), `rfd` (file picker) and `rodio` (audio).
- Without those five, 22 errors remain, all in our code that uses them: `audio.rs`,
  `clipboard.rs`, `motion.rs`, and a few lines of `ui.rs` and `main.rs`. Gating them with `cfg`
  is enough.

## What needs work

1. **The mock runs on a thread of its own.** `Backend::spawn` (`b33p-core/src/lib.rs`) starts a
   thread, and `mock.rs` blocks on it: `thread::sleep` in about six places and `recv_timeout` on
   the crossbeam `Inbox`. On `wasm32-unknown-unknown` `std::thread::spawn` panics. The mock has
   to become an async task on Slint's event loop (`slint::spawn_local`, a sleep made from a
   `slint::Timer`, an async command channel). This is most of the work.
2. **Starting in the browser.**
   - A `cdylib` entry point for wasm-bindgen.
   - Without: the single instance claim, shutdown handling, tray, autostart, log file, keyring.
   - Settings in memory, since there is no data folder (`directories` finds none).
   - On the web, `slint::run_event_loop_until_quit` doesn't return as on the desktop, so nothing
     after it in `main.rs` may be needed there.
3. **Fonts and style.** The browser gives no system fonts: one must be bundled, and emoji (the
   mock uses 🦀) need one too. The `native` style falls back to Fluent, so the demo won't look
   like Breeze.
4. **GitHub Pages.**
   - A workflow that builds with `wasm-bindgen-cli` (or trunk), runs `wasm-opt`, and publishes
     with `actions/upload-pages-artifact` and `actions/deploy-pages`, pinned for zizmor.
   - Pages turned on in the repository's settings.
   - The download is several MB even optimized.

## Not the way

Real threads in wasm (`+atomics`, `build-std` on nightly) need cross-origin isolation headers,
which GitHub Pages can't send without a service-worker workaround. Making the mock async is
simpler and keeps stable Rust.

## How it would go in

A `web` feature of `b33p-slint` with its own entry point, so desktop and release builds stay as
they are. Likely one PR of a few hundred lines, most of it the async mock.
