#!/usr/bin/env python3
"""Rust-vs-Rust regression report: what moved between two of our own frames.

Captures are byte-reproducible, so any difference is a real layout change. The
report answers two questions a screenshot does not:

  where  <before> <after>          rectangles that changed, as row bands and
                                   their column span
  cells  <before> <after> X Y W H  content runs inside one strip: every group
                                   of ink with its width and the gap before it

`where` is the guard after a change that was meant to be local: a band outside
the area you touched is a regression. `cells` is the guard against clipped
text — a run that lost pixels is a label that no longer fits its box, which
neither a colour histogram nor an edge report will tell you.
"""

import subprocess
import sys

GAP = 3


def load(path, x, y, w, h):
    raw = subprocess.run(
        ["ffmpeg", "-loglevel", "error", "-i", path,
         "-vf", f"crop={w}:{h}:{x}:{y}", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
        capture_output=True, check=True).stdout
    return [[tuple(raw[(r * w + c) * 3:(r * w + c) * 3 + 3]) for c in range(w)] for r in range(h)]


def size(path):
    out = subprocess.run(
        ["ffprobe", "-loglevel", "error", "-select_streams", "v:0",
         "-show_entries", "stream=width,height", "-of", "csv=p=0:s=x", path],
        capture_output=True, check=True, text=True).stdout.strip()
    return (int(v) for v in out.split("x"))


def runs(values, gap=GAP):
    """Groups sorted indices into runs, closing gaps of `gap` or fewer."""
    out, start, prev = [], None, None
    for value in values:
        if start is None:
            start = value
        elif value - prev > gap:
            out.append((start, prev))
            start = value
        prev = value
    if start is not None:
        out.append((start, prev))
    return out


def where(before, after):
    w, h = size(before)
    one, two = load(before, 0, 0, w, h), load(after, 0, 0, w, h)
    rows = {}
    for r in range(h):
        cols = [c for c in range(w) if one[r][c] != two[r][c]]
        if cols:
            rows[r] = (cols[0], cols[-1])
    if not rows:
        return ["identical"]
    return [f"  y {a}..{b} ({b - a + 1} rows)  x "
            f"{min(rows[r][0] for r in range(a, b + 1) if r in rows)}.."
            f"{max(rows[r][1] for r in range(a, b + 1) if r in rows)}"
            for a, b in runs(sorted(rows), gap=1)]


def cells(path, x, y, w, h):
    img = load(path, x, y, w, h)
    fill = max({(p, sum(1 for row in img for q in row if q == p)) for row in img for p in row},
               key=lambda kv: kv[1])[0]
    ink = [c for c in range(w) if any(img[r][c] != fill for r in range(h))]
    out, prev_end = [], -1
    for a, b in runs(ink):
        out.append(f"  gap {a - prev_end - 1 if prev_end >= 0 else a:>3}  "
                   f"ink {a:>4}..{b:<4} ({b - a + 1:>3}px)")
        prev_end = b
    out.append(f"  gap {w - 1 - prev_end:>3}  (trailing)")
    return out


def main():
    mode, before, after = sys.argv[1], sys.argv[2], sys.argv[3]
    if mode == "where":
        print("\n".join(where(before, after)))
        return
    x, y, w, h = (int(v) for v in sys.argv[4:8])
    for name, path in (("before", before), ("after", after)):
        print(f"{name}:")
        print("\n".join(cells(path, x, y, w, h)))


main()
