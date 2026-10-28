"""Lag between a line's own stamp and when the log driver took it, per file: `<taken ns> <own ns>`."""
import os
import sys

for path in sys.argv[1:]:
    lags = sorted((int(taken) - int(own)) / 1e6 for taken, own in (l.split() for l in open(path)))
    n = len(lags)
    q = lambda p: lags[min(n - 1, int(p * (n - 1)))]
    name = os.path.join(os.path.basename(os.path.dirname(path)), os.path.basename(path))
    print(f"{name:22} n={n:6} lag ms: p50 {q(.5):8.1f} p95 {q(.95):8.1f} p99 {q(.99):8.1f} max {lags[-1]:8.1f}")
