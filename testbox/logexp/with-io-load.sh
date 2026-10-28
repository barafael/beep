#!/bin/sh
# Runs "$@" while 8 writers fill and fsync 2 GiB files in target/io-load (on the disk of the
# repository, and here of the journal), then stops them and removes their files, whatever "$@"
# did. Writers stop once $dir/running is gone.
dir=$(cd "$(dirname "$0")/../.." && pwd)/target/io-load
mkdir -p "$dir" && touch "$dir/running"
stop() {
  rm -f "$dir/running"
  wait
  rm -rf "$dir"
}
trap stop EXIT INT TERM
for i in 1 2 3 4 5 6 7 8; do
  ( while [ -e "$dir/running" ]; do
      dd if=/dev/zero of="$dir/f$i" bs=1M count=2048 conv=fsync status=none 2>/dev/null || break
    done ) &
done
sleep 8
"$@"
