#!/usr/bin/env python3
"""Convert a monospace BDF bitmap font into Meridian's raw console font format.

Output format: 256 glyphs (code points U+0000..U+00FF, i.e. Latin-1), each
`height` bytes, one byte per row, most significant bit = leftmost pixel.
Glyphs missing from the BDF are left blank.

Usage: python tools/bdf2font.py <input.bdf> <output.bin>
"""

import sys


def convert(bdf_text: str, width: int = 8, height: int = 16) -> bytes:
    lines = bdf_text.splitlines()
    ascent = None
    for line in lines:
        if line.startswith("FONTBOUNDINGBOX"):
            _, w, h, _xoff, yoff = line.split()
            if int(w) != width or int(h) != height:
                raise SystemExit(f"expected {width}x{height} font, got {w}x{h}")
            ascent = int(h) + int(yoff)
            break
    if ascent is None:
        raise SystemExit("FONTBOUNDINGBOX not found")

    out = bytearray(256 * height)
    i = 0
    while i < len(lines):
        if not lines[i].startswith("STARTCHAR"):
            i += 1
            continue
        encoding = None
        bbx = None
        rows = []
        i += 1
        while not lines[i].startswith("ENDCHAR"):
            parts = lines[i].split()
            if parts[0] == "ENCODING":
                encoding = int(parts[1])
            elif parts[0] == "BBX":
                bbx = tuple(int(p) for p in parts[1:5])
            elif parts[0] == "BITMAP":
                i += 1
                while not lines[i].startswith("ENDCHAR"):
                    rows.append(int(lines[i], 16))
                    i += 1
                break
            i += 1
        i += 1
        if encoding is None or bbx is None or not 0 <= encoding <= 255:
            continue
        w, h, xoff, yoff = bbx
        top = ascent - (yoff + h)
        nbytes = (w + 7) // 8
        for r, value in enumerate(rows):
            y = top + r
            if not 0 <= y < height:
                continue
            # Left-align the row to 8 bits, then apply the x offset.
            row = value >> (nbytes * 8 - 8) if nbytes > 1 else value
            row = (row >> xoff) & 0xFF if xoff >= 0 else (row << -xoff) & 0xFF
            out[encoding * height + y] = row
    return bytes(out)


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    with open(sys.argv[1], "r", encoding="latin-1") as f:
        data = convert(f.read())
    with open(sys.argv[2], "wb") as f:
        f.write(data)
    print(f"wrote {len(data)} bytes to {sys.argv[2]}")


if __name__ == "__main__":
    main()
