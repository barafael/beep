#!/bin/sh
# Experiment A (testbox/container-logs.md): one container per log driver writes N lines stamped
# with its own clock (`date +%s%N`); afterwards each line's stamp is compared with when the driver
# took it. Uses the server image of the box TESTBOX_INSTANCE (default: tour) for its shell.
# usage: drivers.sh <label> <lines>; results in target/logexp/<label>
set -eu
label=$1; lines=$2; here=$(cd "$(dirname "$0")" && pwd)
img=localhost/b33p-testbox.${TESTBOX_INSTANCE:-tour}-server
out=$here/../../target/logexp/$label; mkdir -p "$out"
for driver in journald k8s-file; do
  name=logexp-$label-$driver
  podman rm -f "$name" >/dev/null 2>&1 || true
  podman run --name "$name" --log-driver "$driver" --entrypoint sh "$img" \
    -c "i=0; while [ \$i -lt $lines ]; do echo \"line \$(date +%s%N) \$i\"; i=\$((i+1)); done" >/dev/null
  if [ "$driver" = journald ]; then
    sleep 2
    journalctl --user CONTAINER_NAME="$name" -o json --no-pager \
      | python3 -I -c '
import json,sys
for l in sys.stdin:
    e=json.loads(l); m=e.get("MESSAGE","")
    if isinstance(m,str) and m.startswith("line "):
        print(int(e["__REALTIME_TIMESTAMP"])*1000, m.split()[1])' > "$out/$driver.tsv"
  else
    # k8s-file: conmon stamps each line as it reads it (RFC 3339 with nanoseconds).
    podman logs -t "$name" | python3 -I -c '
import sys,datetime
for l in sys.stdin:
    t,_,m=l.rstrip("\n").partition(" ")
    if not m.startswith("line "): continue
    d=datetime.datetime.fromisoformat(t[:26]+t[29:] if len(t)>32 else t)
    print(int(d.timestamp()*1e9), m.split()[1])' > "$out/$driver.tsv"
  fi
  podman rm -f "$name" >/dev/null
done
python3 -I "$here/summarize.py" "$out"/*.tsv
