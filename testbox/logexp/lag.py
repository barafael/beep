"""Experiment B (testbox/container-logs.md): for each Dropwizard line (`LEVEL [yyyy-mm-dd hh:mm:ss,mmm]`, UTC) in `podman logs -t`
output, the lag between the server's own stamp and the driver's."""
import datetime
import re
import sys

for path in sys.argv[1:]:
    lags = []
    for line in open(path, errors="replace"):
        parts = line.split(" ", 1)
        if len(parts) < 2:
            continue
        m = re.search(r"\[(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d,\d{3})\]", parts[1][:60])
        if not m or not re.match(r"^\s*(INFO|WARN|ERROR|DEBUG)\s", parts[1]):
            continue
        stamp = parts[0]
        # podman's RFC 3339 with nanoseconds: trim to microseconds for fromisoformat.
        stamp = re.sub(r"(\.\d{6})\d+", r"\1", stamp)
        taken = datetime.datetime.fromisoformat(stamp)
        own = datetime.datetime.strptime(m.group(1), "%Y-%m-%d %H:%M:%S,%f").replace(tzinfo=datetime.timezone.utc)
        lags.append((taken - own).total_seconds() * 1000)
    lags.sort()
    n = len(lags)
    if not n:
        print(path, "no lines")
        continue
    q = lambda p: lags[min(n - 1, int(p * (n - 1)))]
    print(f"{path.split('/')[-1]:18} n={n:5} lag ms: p50 {q(.5):9.1f} p95 {q(.95):9.1f} p99 {q(.99):9.1f} max {lags[-1]:9.1f}")
