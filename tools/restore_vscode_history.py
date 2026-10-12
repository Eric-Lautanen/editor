"""Restore C:\\github\\readitor files from VS Code local history.

Scans every History/*/entries.json whose `resource` points inside the
readitor checkout and copies the newest timestamped snapshot back to its
original path. Prints what was restored and what stayed missing.
"""
import json
import os
import shutil
import sys
from urllib.parse import unquote

HIST = r"C:\Users\ericl\AppData\Roaming\Code\User\History"
ROOT = r"C:\github\readitor"

restored = {}
for name in os.listdir(HIST):
    d = os.path.join(HIST, name)
    ej = os.path.join(d, "entries.json")
    if not os.path.isfile(ej):
        continue
    try:
        with open(ej, encoding="utf-8") as f:
            meta = json.load(f)
    except (OSError, ValueError):
        continue
    res = meta.get("resource", "")
    if not res.startswith("file:///"):
        continue
    path = unquote(res[len("file:///"):]).replace("/", "\\")
    if not path.lower().startswith(ROOT.lower() + "\\"):
        continue
    entries = meta.get("entries", [])
    if not entries:
        continue
    newest = max(entries, key=lambda e: e.get("timestamp", 0))
    src = os.path.join(d, newest["id"])
    if not os.path.isfile(src):
        continue
    rel = path[len(ROOT) + 1:]
    # Keep the newest snapshot per file across history folders.
    prev = restored.get(rel)
    if prev and prev[0] >= newest.get("timestamp", 0):
        continue
    restored[rel] = (newest.get("timestamp", 0), src)

missing = []
for rel, (ts, src) in sorted(restored.items()):
    dst = os.path.join(ROOT, rel)
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    shutil.copyfile(src, dst)
    print(f"restored {rel} (ts={ts})")
print(f"\n{len(restored)} files restored from VS Code history.")
