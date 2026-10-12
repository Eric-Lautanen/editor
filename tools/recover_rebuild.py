#!/usr/bin/env python3
"""Rebuild readitor sources from the recovered session logs.

Strategy per path, in chronological order (seq):
  1. last write_file body = base (they are rare, complete files)
  2. else last read_file result (line numbers stripped) = base
  3. apply every patch_file (old_text -> new_text) and patch_lines
     (start_line..end_line -> new_text) AFTER the base in order.
Unapplied patches are reported, not fatal (verification below will catch
gross mismatches by compiling).
"""
import json
import re
import sys
from collections import defaultdict

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

ROOT = r"C:\github\readitor"
SESSIONS = [
    r"C:\github\AutoCode\target\debug\AutoCode_data\projects\readitor\sessions\dhgjk_continue_m5\messages.jsonl",
    r"C:\github\AutoCode\target\debug\AutoCode_data\projects\readitor\sessions\0wyvv_readitor_feedback_and_icon\messages.jsonl",
]
LINE = re.compile(r"^\s*(\d+) \| (.*)$")

def strip_line_numbers(text):
    out = []
    in_block = False
    for raw in text.splitlines():
        m = LINE.match(raw)
        if m:
            in_block = True
            out.append(m.group(2))
        elif not in_block:
            continue  # header lines before the block
        else:
            # footer lines after the block: stop at first non-match
            if raw.startswith("--") or "post token" in raw.lower() or raw.startswith("POST"):
                break
            out.append(raw)
    return "\n".join(out)

ops = []
seq = 0
for path in SESSIONS:
    with open(path, encoding="utf-8", errors="replace") as fh:
        rows = []
    with open(path, encoding="utf-8", errors="replace") as fh:
        for x in fh:
            x = x.strip()
            if not x:
                continue
            try:
                rows.append(json.loads(x))
            except Exception:
                continue
results = {}
for d in rows:
    ts = d.get("timestamp", 0)
    for tc in d.get("tool_calls") or []:
        seq += 1
        fn = tc.get("function", {})
        name = fn.get("name")
        try:
            a = json.loads(fn.get("arguments") or "{}")
        except Exception:
            a = {}
        rec = dict(seq=seq, op=name, args=a, rid=tc.get("id"), t=ts)
        if name in ("write_file", "patch_file", "patch_lines"):
            p = (a.get("path") or a.get("file") or "").replace("\\", "/")
            p = p.replace("C:/github/readitor/", "")
            rec["path"] = p
        elif name in ("read_file", "read_entire_file"):
            p = (a.get("path") or a.get("file") or "").replace("\\", "/")
            p = p.replace("C:/github/readitor/", "")
            rec["path"] = p
        elif name == "read_files":
            ps = []
            for x in a.get("paths", []):
                pp = x if isinstance(x, str) else (x.get("path") or "")
                pp = pp.replace("\\", "/").replace("C:/github/readitor/", "")
                ps.append(pp)
            rec["path"] = ps
        ops.append(rec)
    if d.get("role") in ("Tool", "tool", "ToolResult"):
        results[d.get("tool_call_id")] = d.get("content") or ""

by_path = defaultdict(list)
for r in ops:
    if not r.get("path"):
        continue
    if isinstance(r["path"], list):
        for p in r.pop("path"):
            q = dict(r); q["path"] = p
            by_path[p].append(q)
    else:
        by_path[r["path"]].append(r)

report = []
import os
for p, list_ in sorted(by_path.items()):
    if p.startswith(("C:/Users/", "C:\\")):
        report.append((p, "SKIP-external", 0))
        continue
    base = None
    base_seq = 0
    for r in list_:
        if r["op"] == "write_file" and r["args"].get("content"):
            base = r["args"]["content"]
            base_seq = r["seq"]
    if base is None:
        for r in list_:
            if r["op"] in ("read_file", "read_entire_file") and results.get(r.get("rid")):
                snap = strip_line_numbers(results[r["rid"]])
                if len(snap) > len(base or ""):
                    base = snap
                    base_seq = r["seq"]
    if base is None:
        report.append((p, "NO-BASE", len(list_)))
        continue
    applied = failed = 0
    for r in list_:
        if r["seq"] <= base_seq:
            continue
        if r["op"] == "patch_file":
            old = r["args"].get("old_text")
            new = r["args"].get("new_text")
            if old is not None and old in base:
                base = base.replace(old, new, 1)
                applied += 1
            else:
                failed += 1
        elif r["op"] == "patch_lines":
            s = r["args"].get("start_line")
            e = r["args"].get("end_line")
            new = r["args"].get("new_text")
            if s and e and new is not None:
                lines = base.splitlines(keepends=True)
                if 1 <= s <= len(lines) + 1 and e >= s:
                    base = "".join(lines[: s - 1]) + new + "".join(lines[e:])
                    applied += 1
                else:
                    failed += 1
    full = os.path.join(ROOT, p)
    os.makedirs(os.path.dirname(full) or ROOT, exist_ok=True)
    with open(full, "w", encoding="utf-8", newline="") as fh:
        fh.write(base)
    report.append((p, f"OK ap={applied} fail={failed}", len(base)))

for p, status, size in report:
    print(f"{p:45} {status:22} {size:8}")
