#!/usr/bin/env bash
# Stops everything Android-related on this machine:
#
#   1. Test boxes with an Android container (pods `b33p-testbox[.<instance>]`): each is taken
#      down with `cargo testbox [--instance <name>] down`, which holds the box's lock and keeps
#      its volumes. The containers have `restart=always`, so killing what runs inside only makes
#      Podman start it again; and stopping one container behind the xtask's back leaves the
#      pod half down.
#   2. Then what is left on the host: emulators (and their qemu, crashpad and netsim helpers),
#      the adb server, and anything else running out of an Android SDK.
#
# Usage: testbox/android/stop-android.sh [-n|--dry-run]     (-n only lists what it would stop)
#
# Host processes are matched on the executable (the first word of the command line), never on
# the arguments, so this script, the shell running it and unrelated tools that merely mention
# "android" are left alone. Not touched: other emulators (Bigtable, Pub/Sub), Gradle daemons,
# Android Studio, scrcpy, and containers that are not a test box's.
set -u

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

dry_run=0
case "${1:-}" in
-n | --dry-run) dry_run=1 ;;
"") ;;
*) echo "usage: ${0##*/} [-n|--dry-run]" >&2; exit 2 ;;
esac

# Executables that are Android's: anything under an SDK directory (/opt/android-sdk,
# ~/Android/Sdk, $ANDROID_HOME, ...), plus the usual names wherever they are installed.
sdk_roots='/android-sdk/|/Android/Sdk/|/android-studio/'
[ -n "${ANDROID_HOME:-}" ] && sdk_roots="$sdk_roots|^${ANDROID_HOME%/}/"
[ -n "${ANDROID_SDK_ROOT:-}" ] && sdk_roots="$sdk_roots|^${ANDROID_SDK_ROOT%/}/"
names='^(adb|emulator|emulator64-[a-z0-9]+|netsimd|qemu-system-[a-z0-9_]+-headless)$'

# This script and its ancestors are never candidates.
protected=" $$ "
pid=$$
while [ "$pid" -gt 1 ]; do
  pid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')
  [ -z "$pid" ] && break
  protected="$protected$pid "
done

candidates() {
  ps -eo pid=,args= | while read -r pid exe rest; do
    case "$protected" in *" $pid "*) continue ;; esac
    base=${exe##*/}
    if [[ $exe =~ $sdk_roots ]] || [[ $base =~ $names ]] ||
      { [[ $base == qemu-system-* ]] && [[ " $rest " == *" -avd "* ]]; }; then
      printf '%s\t%s\n' "$pid" "$exe $rest"
    fi
  done
}

# The running test boxes' Android containers, named `<pod>-android`: one pod name per line.
boxes() {
  command -v podman >/dev/null 2>&1 || return 0
  podman ps --format '{{.Names}}' 2>/dev/null |
    grep -E -- '^b33p-testbox(\..+)?-android$' | sed 's/-android$//' || true
}

running_boxes=$(boxes)
list=$(candidates)
if [ -z "$list" ] && [ -z "$running_boxes" ]; then
  echo "No Android processes or test boxes running."
  exit 0
fi

if [ -n "$running_boxes" ]; then
  echo "Test boxes with an Android container (to be taken down):"
  printf '%s\n' "$running_boxes" | sed 's/^/  /'
fi
if [ -n "$list" ]; then
  echo "Android processes:"
  printf '%s\n' "$list" | cut -c1-160 | sed 's/^/  /'
fi
[ "$dry_run" = 1 ] && exit 0

failed=0
for pod in $running_boxes; do
  instance=""
  [[ $pod == b33p-testbox.* ]] && instance=${pod#b33p-testbox.}
  echo "cargo testbox ${instance:+--instance $instance }down"
  # A box held by another command (its lock) makes `down` fail at once: say so, go on.
  (cd "$repo" && cargo testbox ${instance:+--instance "$instance"} down) || {
    echo "Could not take $pod down." >&2
    failed=1
  }
done

# What ran in the boxes is gone with them; look again for what is left on the host.
list=$(candidates)

# Let adb stop its own server first: it is the polite way and leaves no stale socket.
adb_bin=$(command -v adb || true)
[ -n "$adb_bin" ] && "$adb_bin" kill-server >/dev/null 2>&1 || true

pids=$(printf '%s\n' "$list" | cut -f1)
if [ -n "$pids" ]; then
  # shellcheck disable=SC2086
  kill -TERM $pids 2>/dev/null || true
fi

# Give them up to ten seconds to end, then insist.
for _ in $(seq 20); do
  left=$(candidates | cut -f1)
  [ -z "$left" ] && break
  sleep 0.5
done
if [ -n "${left:-}" ]; then
  echo "Still running after 10 s, sending SIGKILL: $(echo $left)"
  # shellcheck disable=SC2086
  kill -KILL $left 2>/dev/null || true
  sleep 1
fi

still_up=$(boxes)
if [ -z "$(candidates)" ] && [ -z "$still_up" ] && [ "$failed" = 0 ]; then
  echo "All Android processes stopped and test boxes down."
else
  echo "Some Android processes or test boxes could not be stopped:" >&2
  [ -n "$still_up" ] && printf '%s\n' "$still_up" | sed 's/^/  box /' >&2
  candidates | cut -c1-160 | sed 's/^/  /' >&2
  exit 1
fi
