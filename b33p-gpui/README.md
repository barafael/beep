# b33p-gpui

The same Signal client as `b33p-slint`, with [GPUI](https://gpui.rs) (Zed's UI framework) drawing
the window instead of Slint. Everything that isn't the window is shared: `b33p-core` (the chat
state and the backends), `b33p-platform` (single instance, log, autostart, clipboard, audio,
reduced motion, file dialogs), `b33p-notify` and the `b33p-harness` protocol. What is here is
the window, and what only the window does.

It builds on `gpui-kit`, the umbrella crate of GPUI (a snapshot of Zed's, published as
`gpui-pre-*`), its component library (`gpui-component`: inputs, buttons, menus, a message
scroller) and the Lucide icons. Visual differences from the Slint window are expected; the
features are the same.

```sh
cargo run -p b33p-gpui                      # the real client
B33P_MOCK=1 cargo run -p b33p-gpui          # the offline fake: nothing is sent
cargo build -p b33p-gpui --features harness # drivable over B33P_AGENT_SOCKET, as b33p-slint is
```

## Looking at it

Never screenshot the desktop (`spectacle -a` takes whichever window is active, which may be
something private). `testbox/gpui-view/view.sh` runs the built binary on the mock on a display of
its own, a cage compositor in a container (testbox/tools `with-display`), and takes its
screenshots; `testbox/gpui-view/agent.py` drives it over the harness socket:

```sh
cargo build -p b33p-gpui --features harness
testbox/gpui-view/view.sh start               # B33P_MOCK=1; more NAME=value arguments are passed on
testbox/gpui-view/agent.py '{"method":"act","action":{"type":"select","conversation":"c:alice"}}'
testbox/gpui-view/view.sh shot alice          # -> target/gpui-view/shots/alice-b33p.png
testbox/gpui-view/view.sh logs; testbox/gpui-view/view.sh stop
```

The mock's chats are `c:alice`, `c:bob`, `g:rust`, `c:stranger`; the actions are the variants of
`Action` in `b33p-harness`. Setting up the image and tools is in the script's header.

## How it is put together

- `shared.rs`: `Shared`, everything that outlives a window: the `ChatState`, the backend's
  commands, settings, caches, timers, audio playback. A view asks the state what to do through
  `Shared::act`/`edit`/`command`/`redraw`, which send the command it answers with and draw what
  changed (`Update`), once per turn of the event loop (`refresh`, `flush`). `UiState` is what
  only the window keeps open or chosen (settings, the calendar, a confirmation, ...).
- Views read `shared.chat.borrow()` when they are drawn. There is no copy of the state and no
  diffing: GPUI draws what the tree says, and only the messages are a virtual list
  (`MessageScroller`, kept in step with `Update::messages` by `Shared::follow_messages`).
- `app.rs` is the root: banner, then `start.rs` (syncing, password, link, failed) or the chats:
  `chat_list.rs`, `conversation.rs` (header, messages from `bubble.rs`, `composer.rs`), and what
  covers them: `panels.rs` (over the messages) and `overlays.rs` (over the window), each a list
  of functions that return an element while the state says it is open.
- `theme.rs` has the colours (`tokens(cx)`, from the component theme with b33p's accent) and the
  sizes the Slint window uses; `widgets.rs` the small pieces (icons, icon buttons, avatars,
  `modal`).
- `agent.rs` is the harness (`--features harness`): the same protocol and the same `Action`
  handling as `b33p-slint/src/agent.rs`, which it was ported from. Both match `Action`
  exhaustively, so an action added to `b33p-harness` doesn't build until both have it.
- Keyboard shortcuts are GPUI actions (`actions.rs`) handled in `app.rs`; Esc (`CloseFront`)
  closes the innermost open thing, in the order `AppView::close_front` has.

The Slint window's code is the reference for what each part does: `b33p-slint/ui/*.slint` for
how it looks and behaves, `b33p-slint/src/ui/` for what each callback does to the state.
