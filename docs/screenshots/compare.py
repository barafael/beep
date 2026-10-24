#!/usr/bin/env python3
"""Compares two folders of screenshots from `shoot.py`, picture by picture.

    python3 docs/screenshots/compare.py target/shots/before target/shots/after --diff target/shots/diff

Prints how many pixels differ in each, and with `--diff` writes the pictures side by side with
the differences marked in red, for a look at what changed.
"""

import argparse
from pathlib import Path

from PIL import Image, ImageChops


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("before")
    parser.add_argument("after")
    parser.add_argument("--diff", help="folder for side-by-side pictures of what differs")
    args = parser.parse_args()
    before, after = Path(args.before), Path(args.after)
    out = Path(args.diff) if args.diff else None
    if out:
        out.mkdir(parents=True, exist_ok=True)
    for path in sorted(before.glob("*.png")):
        other = after / path.name
        if not other.exists():
            print(f"{path.name}: missing after")
            continue
        a = Image.open(path).convert("RGB")
        b = Image.open(other).convert("RGB")
        if a.size != b.size:
            print(f"{path.name}: size {a.size} -> {b.size}")
            b = b.resize(a.size)
        delta = ImageChops.difference(a, b).convert("L").point(lambda v: 255 if v > 24 else 0)
        changed = delta.histogram()[255]
        share = 100 * changed / (a.size[0] * a.size[1])
        print(f"{path.name}: {share:.2f}% of pixels differ")
        if out and changed:
            marked = b.copy()
            marked.paste((255, 0, 0), mask=delta)
            side = Image.new("RGB", (a.size[0] * 3, a.size[1]))
            side.paste(a, (0, 0))
            side.paste(b, (a.size[0], 0))
            side.paste(marked, (a.size[0] * 2, 0))
            side.save(out / path.name)


if __name__ == "__main__":
    main()
