#!/usr/bin/env bash
# Builds b33p-slint every way it draws, as the `testbox` profile with the harness, one build at
# a time, and copies the binaries to a directory (4 to 8 minutes each; Skia's first build also
# downloads prebuilt Skia libraries):
#
#   testbox/gpui-view/build-ways.sh <out-dir> [variant...]
#
# Variants (all of them when none is named):
#   slint-femtovg           Slint's winit backend, FemtoVG on OpenGL, Fluent style
#   slint-skia              the same with Skia (Vulkan if the driver has it, else OpenGL)
#   slint-femtovg-patched, slint-skia-patched
#                           the same with the Slint fork patch of b33p-slint/fork-patches
#                           (the text cursor stops blinking after 10 s; the fix for animations
#                           from a value to itself is in the pinned fork now), applied to a copy of the pinned Slint revision that the
#                           workspace is pointed at for that one cargo call; Cargo.toml and
#                           Cargo.lock are put back afterwards, whatever happens.
# The out directory must not be inside the worktree (cargo would take the patched copy of Slint
# for a member of b33p's workspace) and holds about 300 MB for that copy.
# SLINT_FORK: a Slint checkout at the pinned revision to patch instead of a copy of cargo's own
# (it is patched in a copy at <out-dir>/slint-fork-patched either way).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
out=$(mkdir -p "${1:?usage: build-ways.sh <out-dir> [variant...]}" && cd "$1" && pwd)
shift
case $out/ in "$root"/*) echo "the out dir must be outside $root" >&2; exit 2 ;; esac
variants=("$@")
[ ${#variants[@]} -eq 0 ] && variants=(slint-skia slint-femtovg slint-skia-patched slint-femtovg-patched)

features() { # the cargo arguments that select the graphics stack of a variant
    case ${1%-patched} in
    slint-femtovg) echo "--no-default-features --features harness,signal,gpu-femtovg" ;;
    slint-skia) echo "--no-default-features --features harness,signal,gpu-skia" ;;
    *) echo "unknown variant $1" >&2; return 1 ;;
    esac
}

rev=$(grep -m1 '^slint = ' "$root/Cargo.toml" | sed 's/.*rev = "\([0-9a-f]*\)".*/\1/')
fork=$out/slint-fork-patched
patched_fork() {
    [ -d "$fork" ] && return
    local src=${SLINT_FORK:-$(ls -d "$HOME"/.cargo/git/checkouts/slint-*/"${rev:0:7}" | head -1)}
    [ -d "$src" ] || { echo "no Slint checkout at $rev (build once unpatched first)" >&2; return 1; }
    cp -r "$src" "$fork.tmp"
    for p in "$root"/b33p-slint/fork-patches/*.patch; do
        patch -d "$fork.tmp" -p1 --quiet <"$p"
    done
    mv "$fork.tmp" "$fork"
}

cd "$root"
for v in "${variants[@]}"; do
    # shellcheck disable=SC2046
    args=(build --profile testbox -p b33p-slint $(features "$v"))
    if [[ $v == *-patched ]]; then
        patched_fork
        cp Cargo.toml "$out/Cargo.toml.orig"; cp Cargo.lock "$out/Cargo.lock.orig"
        trap 'cp "$out/Cargo.toml.orig" "$root/Cargo.toml"; cp "$out/Cargo.lock.orig" "$root/Cargo.lock"' EXIT
        python3 - "$fork" <<'PY'
import re, sys
fork = sys.argv[1]
where = {"slint": "api/rs/slint", "slint-build": "api/rs/build", "i-slint-core": "internal/core",
         "i-slint-common": "internal/common", "i-slint-backend-testing": "internal/backends/testing"}
s = open("Cargo.toml").read()
for name, path in where.items():
    s, n = re.subn(r'^(%s = \{ )git = "https://github\.com/barafael/slint", rev = "[0-9a-f]+"' % re.escape(name),
                   r'\1path = "%s/%s"' % (fork, path), s, flags=re.M)
    assert n == 1, name
open("Cargo.toml", "w").write(s)
PY
    fi
    echo "== $v: cargo ${args[*]}" >&2
    cargo "${args[@]}" 2>&1 | tail -3
    cp target/testbox/b33p "$out/$v"
    if [[ $v == *-patched ]]; then
        cp "$out/Cargo.toml.orig" Cargo.toml; cp "$out/Cargo.lock.orig" Cargo.lock; trap - EXIT
    fi
    ls -l "$out/$v"
done
