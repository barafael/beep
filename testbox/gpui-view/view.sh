#!/usr/bin/env bash
# Looks at b33p-gpui on a display of its own, which only ever holds b33p (testbox/tools
# `with-display`: cage and wayvnc, in a container), never the host's desktop.
#
#   testbox/gpui-view/view.sh start [env=value...]   runs the built binary on the mock
#   testbox/gpui-view/view.sh shot <name>            writes target/gpui-view/shots/<name>.png
#   testbox/gpui-view/view.sh logs | stop
#
# VIEW_BIN: another binary to run (default target/debug/b33p-gpui; e.g. a b33p-slint build);
# VIEW_IMAGE: another image (default b33p-gpui-view; b33p-bench-view has Qt and the Vulkan driver);
# VIEW_GPU=1: draw on the host's GPU through its render node, as bench.py does.
#
# Needs: `cargo build -p b33p-gpui` (add `--features harness` to drive it through
# target/gpui-view/run/agent.sock), the image from the Containerfile beside this script
# (`podman build -t b33p-gpui-view -f testbox/gpui-view/Containerfile testbox/gpui-view`), and
# the tools: `CARGO_TARGET_DIR=target/testbox-workspaces cargo build --manifest-path
# testbox/tools/Cargo.toml --bin with-display --features display` and testbox/shot's.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
out=$root/target/gpui-view
tools=$root/target/testbox-workspaces/debug
# A worktree of its own has its own container, named after it.
name=b33p-gpui-view-$(basename "$root")
case ${1:-} in
start)
    shift
    podman rm -f $name >/dev/null 2>&1 || true
    rm -rf "$out/view" "$out/run"
    mkdir -p "$out/view" "$out/run" "$out/shots" "$out/data"
    # VIEW_WRAP: a shell command to run instead of plain `b33p` (e.g. under a session bus, with
    # a StatusNotifierWatcher beside it, to look at the tray); VIEW_MOUNT: a `-v` volume for it.
    run_cmd=(b33p)
    [ -n "${VIEW_WRAP:-}" ] && run_cmd=(sh -c "$VIEW_WRAP")
    mount_args=()
    [ -n "${VIEW_MOUNT:-}" ] && mount_args=(-v "$VIEW_MOUNT")
    gpu_args=()
    [ -n "${VIEW_GPU:-}" ] && gpu_args=(--device "${VIEW_RENDER_NODE:-/dev/dri/renderD128}"
        -e WITH_DISPLAY_RENDERER=gles2
        -e "WLR_RENDER_DRM_DEVICE=${VIEW_RENDER_NODE:-/dev/dri/renderD128}")
    env_args=()
    for pair in "$@"; do env_args+=(-e "$pair"); done
    podman run -d --rm --name $name --userns=keep-id --security-opt label=disable \
        "${gpu_args[@]}" -v "${VIEW_BIN:-$root/target/debug/b33p-gpui}:/usr/local/bin/b33p:ro" \
        -v "$tools/with-display:/usr/local/bin/with-display:ro" \
        -v "$out/view:/run/view" -v "$out/run:/run/b33p" -v "$out/data:/tmp/home" \
        -e HOME=/tmp/home -e B33P_MOCK=1 -e B33P_AGENT_SOCKET=/run/b33p/agent.sock \
        -e RUST_LOG="${RUST_LOG:-info}" "${mount_args[@]}" "${env_args[@]}" \
        "${VIEW_IMAGE:-localhost/b33p-gpui-view}" with-display /run/view 1280x900 -- "${run_cmd[@]}" >/dev/null
    for _ in $(seq 60); do [ -S "$out/view/watch" ] && break; sleep 0.5; done
    ;;
shot)
    "$tools/testbox-shot" "$out/shots" "${2:-shot}" "b33p=$out/view/watch"
    echo "$out/shots/${2:-shot}-b33p.png"
    ;;
logs) podman logs $name 2>&1 ;;
stop) podman rm -f $name >/dev/null 2>&1 || true ;;
*) sed -n 2,12p "$0"; exit 2 ;;
esac
