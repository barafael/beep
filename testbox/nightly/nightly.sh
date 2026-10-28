#!/bin/sh
# A tour run on main, unattended (testbox/nightly/README.md): the worktree $WORKTREE follows
# origin/main, and its box is the instance `nightly`, beside any other. What it said goes to
# target/testbox-nightly-runs/<date>.log in that worktree, and the run's records, as `run`'s,
# under target/testbox-nightly-record/run/.
# usage: nightly.sh [rounds]; NIGHTLY_WORKTREE overrides where the worktree is.
set -eu
repo=$(cd "$(dirname "$0")/../.." && pwd)
worktree=${NIGHTLY_WORKTREE:-$repo/../beep-nightly}
rounds=${1:-6}
git -C "$repo" fetch -q origin main
if [ ! -d "$worktree" ]; then
  git -C "$repo" worktree add -q --detach "$worktree" origin/main
fi
worktree=$(cd "$worktree" && pwd)
git -C "$worktree" checkout -q --detach origin/main
logs=$worktree/target/testbox-nightly-runs
mkdir -p "$logs"
log=$logs/$(date +%Y-%m-%dT%H%M).log
echo "main at $(git -C "$worktree" log --oneline -1)" > "$log"
cd "$worktree"
# Not `exec`: the summary's last lines go to the journal too.
status=0
cargo testbox --instance nightly run tour --rounds "$rounds" >> "$log" 2>&1 || status=$?
# Down at the end: the records hold what there is to look at, and a box left up would be in the
# service's cgroup, which systemd kills when the service ends.
cargo testbox --instance nightly down >> "$log" 2>&1 || true
grep -E '^round|^testbox:' "$log" || true
echo "log: $log"
exit $status
