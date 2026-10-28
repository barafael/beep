#!/bin/sh
# Experiment B (testbox/container-logs.md): a tour round per log driver, under disk-write load;
# the server's log as the driver stamped it, kept from the box the round leaves up. Then
# `lag.py target/logexp/B-*.server` sums up the lag.
here=$(cd "$(dirname "$0")" && pwd); repo=$(cd "$here/../.." && pwd)
out=$repo/target/logexp; mkdir -p "$out"
instance=${TESTBOX_INSTANCE:-tour}
for driver in journald k8s-file; do
  ( cd "$repo" \
    && "$here/with-io-load.sh" env TESTBOX_LOG_DRIVER=$driver \
         cargo testbox run tour --rounds 1 --instance "$instance" ) > "$out/B-$driver.run" 2>&1
  podman logs -t "b33p-testbox.$instance-server" > "$out/B-$driver.server" 2>&1
done
