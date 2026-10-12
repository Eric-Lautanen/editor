#!/usr/bin/env python3
"""Recover readitor sources from AutoCode session logs.

Reads both session logs (messages.jsonl), extracts every file operation in
chronological order, and emits a manifest:

  {"dochistory": {relpath: [{"seq": i, "op": "write|read|patch_full|patch",
                             "content": ..., "patches": [...], "t": ts}]}}

read_file results carry line-numbered text (  1 | ...) — stripped to source.
write_file carries raw content. Later ops override earlier ones for the same
path when reconstructing; this step only records history.
"""
import json
import re
import sys
from collections import defaultdict

SESSIONS = [
    r"C:\github\AutoCode\target\debug\AutoCode_data\projects\readitor\sessions\dhgjk_continue_m5\messages.jsonl",
    r"C:\github\AutoCode\target\debug\AutoCode_data\projects\readitor\sessions\0wyvv_readitor_feedback_and_icon\messages.jsonl",
]

LINE = re.compile(r"^\s*(\d+) \| (.*)$|^(\d+) \| (.*)$")

def strip_line_numbers(text):
    out = []
    for raw in text.splitlines():
        m = LINE.match(raw)
        if m:
            out.append(m.group(m.lastindex))
        else:
            # outside the numbered block (header/footer lines) — drop them
            if out and text.strip():
                continue
    return "\n".join(out)

def parse_args(caller, arg_str):
    try:
        return json.loads(arg_str or "{}")
    except Exception:
        return {}

def main():
    ops = []
    seq = 0
    for path in SESSIONS:
        with open(path, encoding="utf-8", errors="replace") as fh:
            for ln in fh:
                try:
                    d = json.loads(ln)
                except Exception:
                    continue
                ts = d.get("timestamp", 0)
                for tc in d.get("tool_calls") or []:
                    fn = tc.get("function", {})
                    name = fn.get("name")
                    a = parse_args(name, fn.get("arguments"))
                    seq += 1
                    if name == "write_file":
                        p = a.get("path", "")
                        ops.append(dict(seq=seq, op="write", path=p,
                                        content=a.get("content", ""), t=ts))
                    elif name in ("read_file", "read_entire_file"):
                        p = a.get("path") or a.get("file") or ""
                        ops.append(dict(seq=seq, op="read", path=p, result_id=tc.get("id"), t=ts))
                    elif name == "read_files":
                        for p in a.get("paths", []):
                            pp = p if isinstance(p, str) else p.get("path", "")
                            ops.append(dict(seq=seq, op="read", path=pp, result_id=tc.get("id"), t=ts))
                    elif name == "patch_file":
                        p = a.get("path") or a.get("file") or ""
                        ops.append(dict(seq=seq, op="patch", path=p,
                                        patch=fn.get("arguments"), result_id=tc.get("id"), t=ts))
                    elif name == "patch_lines":
                        p = a.get("path") or a.get("file") or ""
                        ops.append(dict(seq=seq, op="patch_lines", path=p,
                                        patch=fn.get("arguments"), result_id=tc.get("id"), t=ts))
                # tool results
                role = d.get("role")
                if role in ("Tool", "tool", "ToolResult"):
                    tid = d.get("tool_call_id")
                    c = d.get("content") or ""
                    if not isinstance(c, str):
                        continue
                    # pair with the pending op for the same result id
                    for op in reversed(ops):
                        if op.get("result_id") and op["result_id"] == tid:
                            op["content"] = c
                            break
                        if op.get("op") == "patch" and op.get("result_id") == tid:
                            op["content"] = c
                            break
    # group by path
    by = defaultdict(list)
    for op in ops:
        p = (op.get("path") or "").replace("\\", "/").replace("C:/github/readitor/", "")
        if not p:
            continue
        by[p].append(op)
    out = {}
    for p, list_ in by.items():
        out[p] = [{k: v for k, v in o.items() if k != "result_id"} for o in list_]
    with open(r"C:\github\readitor\tools\recovery_manifest.json", "w", encoding="utf-8") as fh:
        json.dump(out, fh, indent=1)
    # report
    have_content = sum(1 for p, list_ in out.items() if any(o.get("content") for o in list_))
    print(f"paths: {len(out)}, with content: {have_content}")
    for p, list_ in sorted(out.items()):
        kinds = {}
        for o in list_:
            kinds[o["op"]] = kinds.get(o["op"], 0) + 1
            if o["op"] == "read" and o.get("content"):
                kinds["read_with_content"] = kinds.get("read_with_content", 0) + 1
        print(f"  {p}: {kinds}")

if __name__ == "__main__":
    main()
