#!/usr/bin/env python3
"""Dev tool: pre-rasterize Liberation fonts (SIL OFL 1.1) into assets/readitor-fonts.bin.

The app embeds the result with include_bytes!, so there is no runtime dependency.
Run:  python3 tools/gen_font.py        (needs Pillow + fontTools, dev machine only)

Binary layout (little endian):
  b"RFNT"  u32 version(1)  u16 src_em  u16 nfaces
  per face (order = FontStyle: Regular, Bold, Italic, BoldItalic, Mono):
    f32 ascent  f32 descent  u32 nglyphs
    nglyphs * { u32 codepoint, f32 advance, i16 left, i16 top, u16 w, u16 h, u32 offset }   (sorted by codepoint)
    u32 data_len, data (8-bit coverage, row-major, top-down)
  left = pixels right of pen origin; top = pixels ABOVE baseline to the bitmap's top row.
"""
import struct, sys, os
from PIL import ImageFont
from fontTools.ttLib import TTFont

SRC_EM = 36
D = "/usr/share/fonts/truetype/liberation/"
FACES = [D + "LiberationSans-Regular.ttf", D + "LiberationSans-Bold.ttf",
         D + "LiberationSans-Italic.ttf", D + "LiberationSans-BoldItalic.ttf",
         D + "LiberationMono-Regular.ttf"]
EXTRA = [0x2013, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2026, 0x2122, 0x20AC,
         0x2190, 0x2191, 0x2192, 0x2193, 0x2212, 0x2260, 0x2264, 0x2265, 0x25CF, 0x25CB, 0xFFFD]
WANT = list(range(32, 127)) + list(range(160, 256)) + EXTRA

out = bytearray(b"RFNT" + struct.pack("<IHH", 1, SRC_EM, len(FACES)))
for path in FACES:
    cmap = TTFont(path).getBestCmap()
    font = ImageFont.truetype(path, SRC_EM, layout_engine=ImageFont.Layout.BASIC)
    asc, desc = font.getmetrics()
    cps = [c for c in WANT if c in cmap]
    recs, blob = [], bytearray()
    for cp in cps:
        ch = chr(cp)
        adv = font.getlength(ch)
        l, t, r, b = font.getbbox(ch, anchor="ls")          # relative to baseline-left origin
        w, h = max(r - l, 0), max(b - t, 0)
        off = len(blob)
        if w and h:
            mask = font.getmask(ch, "L", anchor="ls")
            w, h = mask.size
            blob += bytes(mask)
        else:
            w = h = 0
        recs.append((cp, adv, l, -t, w, h, off))
    out += struct.pack("<ffI", float(asc), float(desc), len(recs))
    for cp, adv, l, top, w, h, off in recs:
        out += struct.pack("<IfhhHHI", cp, adv, l, top, w, h, off)
    out += struct.pack("<I", len(blob)) + blob
    print(f"{os.path.basename(path):30s} glyphs={len(recs):3d} data={len(blob):7d}B asc={asc} desc={desc}")

dst = os.path.join(os.path.dirname(__file__), "..", "assets", "readitor-fonts.bin")
open(dst, "wb").write(out)
print("wrote", os.path.normpath(dst), len(out), "bytes")
