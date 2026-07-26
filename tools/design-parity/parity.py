#!/usr/bin/env python3
"""Structural parity report: canon frame vs ours.

Two views, both blind to rasteriser differences because they look at where
colour changes rather than at exact pixels:

  bands  <canon> <ours>                 horizontal splits: strip heights and
                                        the colour of every divider
  strip  <canon> <ours> X Y W H         vertical segments across a strip:
                                        cell edges, widths, fills
  ink    <canon> <ours> X Y W H         air around the content of one cell:
                                        left/right/top/bottom, in pixels

`ink` is the one that catches a glyph glued to its border; `strip` catches a
cell that is the wrong width or missing; `bands` catches a doubled divider.
Cells we deliberately do not have show up as a segment present on one side
only, not as a metric mismatch.
"""

import subprocess
import sys


def load(path, x, y, w, h):
    raw = subprocess.run(
        ["ffmpeg", "-loglevel", "error", "-i", path,
         "-vf", f"crop={w}:{h}:{x}:{y}", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
        capture_output=True, check=True).stdout
    return [[tuple(raw[(r * w + c) * 3:(r * w + c) * 3 + 3]) for c in range(w)] for r in range(h)]


def hexs(colour):
    return "#%02x%02x%02x" % colour


def dominant(run):
    counts = {}
    for p in run:
        counts[p] = counts.get(p, 0) + 1
    colour, n = max(counts.items(), key=lambda kv: kv[1])
    return colour, n / len(run)


def bands(path, w=200, h=420):
    """Rows where the dominant colour changes — strip boundaries."""
    img = load(path, 0, 0, w, h)
    out, prev = [], None
    for r in range(h):
        colour, share = dominant(img[r])
        if share <= 0.6:
            continue
        if prev is not None and colour != prev:
            out.append((r, hexs(colour)))
        prev = colour
    return out


def strip(path, x, y, w, h):
    """Runs of columns sharing a dominant colour — cells and their dividers."""
    img = load(path, x, y, w, h)
    out, start, prev = [], 0, None
    for c in range(w):
        colour, share = dominant([img[r][c] for r in range(h)])
        key = hexs(colour) if share > 0.55 else "mixed"
        if key != prev:
            if prev is not None:
                out.append((x + start, x + c - 1, prev))
            start, prev = c, key
    out.append((x + start, x + w - 1, prev))
    return out


def ink(path, x, y, w, h):
    """Air between a cell's edges and its content."""
    img = load(path, x, y, w, h)
    fill = dominant([img[r][c] for r in range(h) for c in range(w)])[0]
    cols = [c for c in range(w) if any(img[r][c] != fill for r in range(h))]
    rows = [r for r in range(h) if any(img[r][c] != fill for c in range(w))]
    if not cols:
        return f"fill {hexs(fill)}, empty"
    return (f"fill {hexs(fill)}  left={cols[0]} right={w - 1 - cols[-1]}"
            f" top={rows[0]} bottom={h - 1 - rows[-1]}")


def main():
    mode, canon, ours = sys.argv[1], sys.argv[2], sys.argv[3]
    if mode == "bands":
        for name, path in (("canon", canon), ("ours", ours)):
            print(f"{name}:")
            for row, colour in bands(path):
                print(f"  y={row:<5} {colour}")
        return
    x, y, w, h = (int(v) for v in sys.argv[4:8])
    if mode == "strip":
        for name, path in (("canon", canon), ("ours", ours)):
            print(f"{name}:")
            for a, b, colour in strip(path, x, y, w, h):
                print(f"  {a:>5}..{b:<5} ({b - a + 1:>4}px) {colour}")
        return
    if mode == "ink":
        print(f"canon: {ink(canon, x, y, w, h)}")
        print(f"ours:  {ink(ours, x, y, w, h)}")


main()
