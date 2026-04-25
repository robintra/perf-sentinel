#!/usr/bin/env python3
"""Trim the near-black empty space at the bottom of terminal PNG screenshots.

VHS Screenshot captures the entire viewport, which leaves ~150-250 px of
empty terminal background below short outputs (e.g. `explain --trace-id`
on a small trace). This pass post-processes the captured PNGs to drop
that dead space without touching the width or the actual content.

Usage:
    python3 scripts/trim-bottom-png.py docs/img/**/*.png

Behaviour:
- Width is preserved as-is.
- Height is reduced to the row of the last non-background pixel + 16 px
  padding (so a trailing prompt cursor stays in frame).
- Files where less than 5 px would be cropped are left untouched, so
  this script is safe to run against the whole `docs/img/` tree.
"""
import sys
from PIL import Image

THRESHOLD = 25  # max R/G/B average considered "background black"
PADDING = 16


def trim(path: str) -> None:
    img = Image.open(path).convert("RGB")
    w, h = img.size
    pixels = img.load()
    last_content_row = 0
    for y in range(h - 1, -1, -1):
        for x in range(0, w, 4):  # sample every 4 px for speed
            r, g, b = pixels[x, y]
            if (r + g + b) / 3 > THRESHOLD:
                last_content_row = y
                break
        if last_content_row:
            break
    new_h = min(h, last_content_row + 1 + PADDING)
    if new_h < h - 4:
        img.crop((0, 0, w, new_h)).save(path)
        print(f"trim {path}: {w}x{h} -> {w}x{new_h}")
    else:
        print(f"keep {path}: {w}x{h} (no significant empty bottom)")


if __name__ == "__main__":
    for arg in sys.argv[1:]:
        trim(arg)
