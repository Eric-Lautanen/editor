#!/usr/bin/env python3
"""Build assets/icon-setup.ico from assets/icon.ico.

Why a second icon asset exists: MSI's Icon control draws the *first* frame of
an icon and stretches it to the control's pixel size. `assets/icon.ico` leads
with its 16px frame (the order Windows shell resolution wants), so the setup
dialogs' header icon was a 16px image blown up 3x - a blurry block. This asset
carries the same frames as `icon.ico`, reordered so the 48px frame comes first:
exactly the pixel size a 32x32-dialog-unit control gets at 100% scaling.

`icon.ico` itself is rasterized from assets/icon.svg (the vector source of
src/gfx/icon.rs). Re-run this script whenever `icon.ico` is regenerated:

    python tools/gen_setup_icon.py
"""
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "assets" / "icon.ico"
DST = ROOT / "assets" / "icon-setup.ico"
# Frame order the dialog wants first, then the rest for any other consumer.
ORDER = (48, 64, 128, 256)


def frames(data):
    count = struct.unpack("<H", data[4:6])[0]
    out = {}
    for i in range(count):
        off = 6 + 16 * i
        w, h, _c, _r, _p, bit_count, size, data_off = struct.unpack(
            "<BBBBHHII", data[off : off + 16]
        )
        px = w or 256  # 0 means 256 in the ICO directory entry
        out[px] = (data[data_off : data_off + size], bit_count)
    return out


def build(by_size):
    chosen = [(px, *by_size[px]) for px in ORDER if px in by_size]
    if not chosen:
        raise SystemExit(f"none of the frames {ORDER} exist in {SRC}")
    offset = 6 + 16 * len(chosen)
    directory = b""
    body = b""
    for px, image, bit_count in chosen:
        directory += struct.pack(
            "<BBBBHHII",
            px if px < 256 else 0,  # width (0 = 256)
            px if px < 256 else 0,  # height
            0,
            0,
            1,
            bit_count,
            len(image),
            offset,
        )
        offset += len(image)
        body += image
    return struct.pack("<HHH", 0, 1, len(chosen)) + directory + body


def main():
    data = SRC.read_bytes()
    out = build(frames(data))
    DST.write_bytes(out)
    print(f"wrote {DST.relative_to(ROOT)} ({len(out)} bytes)")


if __name__ == "__main__":
    sys.exit(main())
