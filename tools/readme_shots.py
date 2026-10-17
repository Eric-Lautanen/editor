#!/usr/bin/env python3
"""Regenerate the README screenshots from `samples/`.

Every image in the README is produced by the app itself: `editor --render`
writes a headless PPM snapshot of a real sample file, and this script turns
that into the PNG the README links. No window, no X server, no screenshot
tool — so the pictures cannot drift from what the renderer draws.

    python tools/readme_shots.py [--profile release] [--only NAME]

Needs Pillow (PPM decode + PNG encode); the app itself has no dependencies.
"""

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    from PIL import Image
except ImportError:  # pragma: no cover - dev tooling only
    sys.exit("needs Pillow: python -m pip install pillow")

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "screenshots"

# The window is 1440x900 logical pixels: wide enough that the preview column
# is not cramped and the chrome (title bar, tab, status bar) reads properly.
#
# name, sample, logical size, dpi scale, mode, dark, scroll (logical px)
#
# The window is always described in *logical* pixels — 1200x780 is the window
# you would drag on screen — and `scale` is the DPI factor it is laid out and
# rendered at. The renderer is handed `size * scale` physical pixels, exactly
# as a real 2x display would hand it, so the picture is the same layout at
# twice the pixels. The README column is far narrower than 1200 logical px, so
# that downscale is what keeps the small text legible rather than mushy.
# The window every shot is taken in, in logical pixels.
WINDOW = (1440, 900)

SHOTS = [
    ("markdown-dark.png", "sample_markdown.md", WINDOW, 2, "preview", True, 0.0),
    # The full sample, not `sample_markdown_extra.md`: that one is 42 lines and
    # left two thirds of the frame empty in the light shot, which is the first
    # thing a reader sees in the render table.
    ("markdown-light.png", "sample_markdown.md", WINDOW, 2, "preview", False, 0.0),
    ("split-dark.png", "sample_markdown.md", WINDOW, 2, "split", True, 760.0),
    ("edit-light.png", "sample_markdown.md", WINDOW, 2, "edit", False, 0.0),
    ("json-tree.png", "sample_json.json", WINDOW, 2, "preview", True, 0.0),
    ("csv.png", "sample_csv.csv", WINDOW, 2, "preview", False, 0.0),
    ("code-dark.png", "sample_rust.rs", WINDOW, 2, "preview", True, 0.0),
    ("config-light.png", "sample_toml.toml", WINDOW, 2, "preview", False, 0.0),
    ("log-dark.png", "sample_log.log", WINDOW, 2, "preview", True, 0.0),
]


def render(binary: Path, shot, dest: Path) -> None:
    name, sample, (w, h), scale, mode, dark, scroll = shot
    cmd = [
        str(binary),
        "--render",
        str(dest),
        "--no-update",
        "--size",
        f"{int(w * scale)}x{int(h * scale)}",
        "--scale",
        str(scale),
        "--mode",
        mode,
        "--scroll",
        str(scroll * scale),
    ]
    # Pin the theme: without a flag the app follows this machine's saved
    # session, so a "light" shot taken on a dark desktop would come out dark.
    cmd.append("--dark" if dark else "--light")
    cmd.append(str(ROOT / "samples" / sample))
    subprocess.run(cmd, check=True, cwd=ROOT, stdout=subprocess.DEVNULL)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--profile", default="debug", choices=["debug", "release"])
    ap.add_argument("--only", help="render a single shot by output name")
    args = ap.parse_args()

    binary = ROOT / "target" / args.profile / ("editor.exe" if sys.platform == "win32" else "editor")
    if not binary.is_file():
        sys.exit(f"no binary at {binary} — cargo build first")
    OUT.mkdir(parents=True, exist_ok=True)

    for shot in SHOTS:
        name = shot[0]
        if args.only and args.only != name:
            continue
        with tempfile.TemporaryDirectory() as tmp:
            ppm = Path(tmp) / "shot.ppm"
            render(binary, shot, ppm)
            img = Image.open(ppm)
            img.load()
            # The snapshot is 0xAARRGGBB with the alpha byte ignored on
            # present, so every pixel is already opaque and RGB is lossless.
            img.convert("RGB").save(OUT / name, optimize=True)
        kb = (OUT / name).stat().st_size / 1024
        print(f"{name:22} {img.width}x{img.height}  {kb:7.1f} KiB")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
