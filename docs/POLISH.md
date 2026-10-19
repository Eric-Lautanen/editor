# Backend polish: a window that answers faster

The product is frozen. No new features, no visual changes, no new keys, no
different pixels — the only thing that may change is how long the window takes
to answer. Every item below is internal work on the existing design, and every
item is provable twice: by the release cost gate and by an equality test against
the unoptimized path it replaces.

Hard rules for this whole plan:

- **No new dependencies.** `Cargo.toml` says "NO dependencies. Ever." and that
  stands.
- **Same output.** A changed layout, status string, hit rect or scroll range is a
  bug, not a trade. Where a fast path replaces a slow one, a test compares the
  two and fails on any difference.
- **Measure first.** No item starts without a number from the gate or a probe.
  "Probably slow" is not a reason to touch a hot file.
- **Keep the structural checks.** The gate already asserts that a keystroke
  *patches* rather than rebuilds, and that it never reaches for the worker. Those
  assertions must keep passing; if an item needs to relax one, the item is wrong.
- **One commit per item**, gate green before and after, so a regression is
  bisectable and a revert is one command.

---

## 1. How to measure

```
cargo test --release -- --ignored perf --nocapture                   # the gate
cargo test --release -- --ignored pipeline_breakdown --nocapture     # attribution
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

`src/perf.rs` measures six synthetic 8 MB documents at 1440×900, one kind at a
time, and reports load, first frame, mean scroll frame, allocation churn, live
heap and pane ops against budgets set at roughly 3× the slowest of three
calibration runs. `pipeline_breakdown` is the attribution tool: it times the
parse, the plan, the whole-document layout, the band and (see G3) the editor
pane for the same fixtures.

Three things about that harness have to be fixed before these numbers can drive
priorities (P0).

### 1.1 The harness opens what it just wrote

On this machine the first read of the freshly written `.rs` fixture cost
**571 ms**, and the second read of the same file **4.8 ms**. Only that fixture
showed it; the gate's `code load 743.5 ms` is therefore mostly the filesystem's
first touch of a new file, not app work. Until G1 lands, treat code's load as
unmeasured.

### 1.2 Headless means inline

`App::new_streaming(path, headless)` sets `async_layout: !headless`, and the gate
passes `headless = true`. So the gate's "first frame" is the **inline** path: for
a big JSON/Markdown it includes the whole parse and plan (json's 538 ms and
markdown's 1 669 ms are those two passes plus paint). A real window hands those
to the `preview-layout` worker and paints a prefix band meanwhile. The gate is
measuring the worst case, not the shipped path — useful, but it must not be read
as "the window is blank for 1.7 s". G2 adds the shipped-path number.

### 1.3 One keystroke position

The gate types once, near the top of the document. Two costs grow with how far
down the caret is — `same_lines`' dirty compare and the JSON patch's slot move —
so the current number is the best case. G4 types at both ends.

---

## 2. Baseline (release, 8 MB, 1440×900, this machine)

| kind | load | first frame | frame | churn | typing | live | pane ops |
|---|---|---|---|---|---|---|---|
| json | 114.5 | 537.6 | **0.55** | 0.04 | **207.5** | 121.5 | 155 |
| jsonl | 56.8 | 569.2 | 0.74 | 0.03 | 65.3 | 69.4 | 155 |
| markdown | 95.5 | **1669.3** | 4.30 | 0.17 | **984.7** | 88.4 | 141 |
| code | 743.5¹ | 19.7 | 1.10 | 0.07 | 121.5 | 38.8 | 219 |
| csv | 64.1 | 83.1 | **4.65** | 0.00 | —² | 30.8 | **17011** |
| log | 61.8 | 20.5 | 1.40 | 0.02 | 70.6 | 29.3 | 109 |

ms, MB. Budgets: frame 20 ms, churn 6 MB, live 120–200 MB, pane ops 2 000
(CSV 40 000); typing is reported but **not budgeted**.

¹ First-touch artefact: a warm open of the same file is 4.8 ms read + 37.7 ms
drain (1.1).
² CSV has no plan, so `typing` is not measured at all — a gap in the gate, not a
fast path.

Scroll frames are inside budget everywhere; only CSV (4.65 ms) and Markdown's
band (4.30 ms) are close enough to matter, and the pump's per-tick cost during a
streamed load is not measured at all (§4, S4).

The pain is **typing (markdown, json, code)** and the **document-sized panes**
(CSV's 17k ops, markdown's 4.3 ms band).

### 2.1 Attribution (same run, probes)

| probe | ms |
|---|---|
| json parse | 349.3 |
| json plan (545 208 slots) | 95.7 |
| json band | 0.1 (161 ops) |
| markdown parse (56 528 blocks) | 546.3 |
| markdown plan | 930.2 |
| markdown band | 0.1 (157 ops) |
| **editor pane, `layout_plain` @712 px** | **json 90.7 (335 514 ops) · code 99.0 (262 980) · md 41.2 (101 748)** |
| code highlight plan (292 201 rows) | 15.6 |
| open, json: read + drain | 23.7 + 62.6 |
| open, code (warm): read + drain | 4.8 + 37.7 |

---

## 3. What the numbers say

**A Split/Edit keystroke rebuilds the editor pane from the whole buffer.** All
three patch paths — `patch_plan`, `patch_json_plan`, `patch_jsonl_plan`
(`app.rs`) — end in `install_preview`, which in any non-Preview mode calls
`layout::text::layout_plain` over every line and then walks every line again to
find `h_max`. On 8 MB that is 90.7–99.0 ms of layout plus a second full pass,
per keystroke, before anything the user typed is drawn. `relayout_band` (streaming
loads, the async wait) repeats the same work through `layout_plain_edit`. This is
the largest single piece of typing cost: 99 of code's 122 ms, 91 of json's 208
(the rest is the patch, the `h_max` walk and `sync_dirty`:
T3), 41 of
markdown's 985.

**Markdown re-parses the document on every edit.** `patch_plan` calls
`parse_buffer` for the whole buffer (546 ms in the probe) before
`PanePlan::Markdown::patch` re-measures the changed block run. Above
`async_lines` the edit is instead sent to the worker — the same
document-sized work, just off the UI thread.

**CSV is the last document-sized pane.** It has no plan: `layout::csv::layout`
produces one list for the whole (capped at 2 000 rows) sheet — 17 011 ops — and
every frame paints it, at 4.65 ms. It is also re-parsed and re-laid out on every
edit and resize, and the gate does not measure that at all.

**The streaming pump is unmeasured.** A window drains one 1 MB chunk per tick
and rebuilds the band for the grown buffer; the gate opens headless, so the pump
never runs there. Until S4 measures it, nothing in CI says whether the pump's
share of a tick fits in a frame.

**Markdown's band re-renders its blocks every frame.**
`layout::markdown::layout_band` renders each visible block (re-wrapping its
spans) per frame: 4.30 ms, against 0.1 ms for the JSON band, which reads
positions from its plan.

**Status text rescans the document.** A log keystroke walks 20 000 lines
(`log_counts`); a JSONL keystroke walks every record (`record_count_status`);
with the find bar open, every edit re-scans the whole buffer (`rescan_find`).

**The dirty compare grows with the caret's position.** `after_edit` →
`sync_dirty` → `same_lines` compares the buffer with the saved text line by line
and stops at the first difference. An edit near the top is free; an edit on the
last line of an 8 MB file walks all of it on every keystroke — and the gate's one
typing position (near the top) hides that.

**The widest line is computed three ways, with three answers possible.** The
editor pane's horizontal extent is derived inline in three places:
`install_preview` (ASCII lines as `len × advance`, plus one — the
longest non-ASCII — measured), `relayout`'s Edit branch (the single
longest-by-bytes line measured once), and `layout_plain_edit`'s `text_w`
(longest-by-bytes again). They are not the same answer: a short-ASCII/wide
render next to a long-bytes/narrow-render non-ASCII line makes the
Edit-branch and `layout_plain_edit` scan under-measure exactly the line an
ASCII line out-renders. Two of the three scanners are also a second
whole-buffer walk on every install.

**Memory tails the latency.** 121.5 MB live for an 8 MB JSON, and the editor
pane's 262k–335k ops are a second full row list of the document.

---

## 4. Workstreams

Each item: what the evidence is, what changes, what it should buy, what could
break, how it is proven. Sizes are relative: S = an afternoon, M = a day, L =
two or three.

### P0 — Make the gate tell the truth (do this first)

> **Progress note (this session):** G1 landed — one untimed warm read before
> `App::new_streaming` in `measure()` ([perf.rs](../src/perf.rs)); code's load
> went 743.5 (first-touch) → 61.0 → 65.9 ms across two warm runs, i.e. ~61 ms
> is the honest app-side load number for a warmed 8 MB code fixture.
> **G2 landed** — every kind now reports `read` (open → streamed load → band
> → real pane installed, shipped switches): json 598, jsonl 693, markdown
> 1282, log 78, csv 223, code 101 ms; the worker-installed pane fingerprint
> (ops, texts, ys, content height) equals the inline build's for every kind.
> Two harness-caused lessons recorded: the async app must receive
> `Event::Resized` at the gate's 1440×900 before painting (a band built at
> the default pane size compares against nothing), and the equality capture
> must rewind both runs to scroll 0 before fingerprinting.
> **G3 landed** — `pipeline_breakdown` reports the editor builders on the 8 MB
> code fixture at split width 720: `editor install` (layout_plain,
> install_preview's builder) 80.2 ms / 262 980 ops; `editor band+widest`
> (layout_plain_edit, relayout_band's builder, also the h_max source) 58.8 ms,
> widest 561.7 px.
> **G4 landed** — both caret ends measured and printed (`type` / `end`):
> code 114.5 / **926.8** ms (8× — `same_lines`' from-zero walk, the number the
> top-only position hid), json 119/119, jsonl 50/49, markdown 730/791,
> log 73/62 ms. P0 is complete; R items start next.
> **R1 landed** — one widest-line scanner: `layout::text::widest_width` is
> the single home of the ASCII-fast-path + one-measured-non-ASCII rule; both
> hand-rolled scans in app.rs now call it, and `layout_plain_edit`'s walk
> keeps the rule inline (same answer by construction, still one pass). The
> three-path equality test ([app.rs
> `r1_widest_rule_agrees_across_the_three_installs`](../src/app.rs)) asserts
> widest+rail+32 agrees to a pixel across relayout's Edit branch,
> install_preview, and the band, on a fixture of long ASCII, tab-expanded,
> accented-Latin and CJK lines. A reader note: the Edit-branch and
> install_preview scanners were both hand copies, and `layout_plain_edit`'s
> rule silently differed (longest-by-bytes, which a short-ASCII/wide-render
> line next to a long-bytes/narrow one under-measures) — all three now
> agree.
> **R2 landed** — one paid-patch tail: `Tab::finish_paid_patch(height,…)` is
> the single home of the install-empty-list-at-plan-height + patch-counter
> block, called from all three patch fns. The status-fact refreshes stay at
> the call sites (three different shapes). 16 patch tests pass unchanged.
> **R3 landed** — `App::install_scroll_geometry(content, view_h, ratio)` is
> the one scroll install; relayout's Edit branch, `install_preview`,
> `relayout_band` and `poll_preview_job` now call it (poll re-borrows the
> tab after the call, since it still clears the stream caches). Suite and
> clippy unchanged.
> **R4 landed** — `app.rs` is now `app/`: [`mod.rs`](../src/app/mod.rs) keeps
> the `App` struct, event surface and paint; [`tab.rs`](../src/app/tab.rs)
> holds `Tab` (buffer, revision, load plumbing) and the preview-engine
> support types (`PreviewLayout`, `JobSource`, `PaneInput`, `PreviewJob`);
> [`preview.rs`](../src/app/preview.rs) holds the whole P1–P4 cluster
> (relayout, install, bands, patch paths, the pump, the jobs);
> [`status.rs`](../src/app/status.rs) holds the status bar's app half;
> [`tests.rs`](../src/app/tests.rs) holds the suite. Movement only: the
> pass/fail list after every stage was `506 passed / 0 failed` (one new test
> from R1), fields/methods that crossed a module boundary widened to
> `pub(super)` (and `preview_job_pending` back to `pub`, the release gate
> reaches it), and the release gate ran 7/7 on the split tree.
> **R5 landed** — one band dispatch: [`App::band_for(pane_w, ctx, top, bottom)`](../src/app/preview.rs)
> is the single home of the per-kind band-assembly match (`PreviewPlan`'s
> text/lines/markdown arms plus the virtual JSONL and JSON-tree arms);
> `paint_panes` delegates to it. S1's CSV band, T1's editor band and S4's
> measurement now each add an arm, not a copy of the assembly. Suite 506/0,
> clippy `-D warnings` rc 0, release gate 7/7.
>
> **T1 landed (editor pane band).** `Tab::editor_pane.list` is built by
> `text::editor_band` everywhere an editor install happens — `relayout`'s
> Edit branch, `install_preview` (Edit + Split halves), and
> `relayout_band`'s pump half — all sharing `text::plain_params`' row grid
> (absolute y per line, screenful-sized ops, full-document height; R1's
> per-pixel equality test pins band == full list in the covered window).
> Coverage in `(first, next, edit_w, zoom, line_count)` rides on the tab;
> `App::editor_band_stale` guards it at paint time (a scroll past the
> covered rows marks dirty; the stream_top/Bottom check's editor twin) and
> three installs rebuild their own range before the tab borrow (pane's own
> area via `editor_area()`, offset via `editor_scroll()` — Split's halves
> paint at their own share, not the raw shared offset).
> **h_max**: the widest-row rule is cached in `Tab::widest` (extent;
> edit_w/zoom/scale params). Whole-document scans (`text::widest_width`)
> run only on a parameter change; a keystroke grows the cache through
> `after_edit`'s `grow_widest_after_edit` → `text::touched_extent`, and the
> edit log is taken exactly once there before `patch_plan_for_edit` rebases
> the plan with the same spans. Deletions leave the extent high until the
> next parameter change — a few px of slack, traded for typing that never
> walks the document. `layout_plain_edit` is `#[cfg(test)]` now: kept as
> the perf gate's before/after reference only. Suite 507/0 (T1's equality
> test in text::tests), clippy clean, release gate 7/7.
>
> **Session checkpoint (all P0 + P0r + T1 done).** T2–T7, S1, S2 and C1 have
> since landed — each carries its note below — and the gate runs every kind
> within budget (see the P5 section for the part still open: the typing
> numbers are measured but not yet budget-enforced).

**G1 · A fair load measurement.** (S)
The harness writes the fixture and immediately opens it, so the first read pays the filesystem's first-touch cost
(571 ms vs 4.8 ms measured). Read the file once untimed first — that both
warms the page cache and lets the first-touch finish — then time
`App::new_streaming`. No sleeps; comment why, so nobody "fixes" it
back. Measurement only. *Verify:* the code fixture's load lands near the other
kinds' and stays there across runs.

**G2 · Measure the shipped path.** (S/M)
Add a second number per kind, **readable**: `async_layout` on, pump `Tick` +
paint until `preview_job_pending()` is false and the real pane (tree/plan/doc) is
installed, and report that time. Keep "first frame" as "something on screen"
(the prefix band). *Verify:* the async run installs the same pane the inline run
does — the worker-installed tree/plan compared against an inline build of the
same text, the equality the patch tests already assert, applied across the
worker boundary — and json/markdown's readable number is well under the inline
one.

**G3 · One attribution tool.** (S)
Add the editor pane's two builders to `pipeline_breakdown` — `layout_plain`
(what `install_preview` pays) and `layout_plain_edit` (what `relayout_band` pays,
which also returns the widest line) at the split width — plus the status
scans once T4/T5 land. Future prioritisation starts from one command. *Verify:* the probe prints a line for
every cost named in §3 that runs without a streamed load (the pump is S4).

**G4 · Type at both ends.** (S)
Measure the typing keystroke near the top *and* on the last line of the
document, and report both (or the max). Costs that grow with caret position —
`same_lines` and the JSON patch's slot move — are invisible otherwise.
*Verify:* both positions are reported per kind, any difference is explained
(json's slot move and the `same_lines` walk are the expected ones), and both are
budgeted by B1.

### P0r — Refactors that make the rest cheap

These move code, not behavior, and are sized so each polish item below lands in
one place. Do them with P0, before the perf items, so the perf items are small
diffs against already-unified code.

**R1 · One widest-line scanner.** (S)
Extract a single `layout::text::widest_width(buf, param) -> f32` and use it in
both hand-rolled scanners; keep `layout_plain_edit`'s single-walk behavior, and
align its semantics with `install_preview`'s (max over the ASCII fast path plus
measured non-ASCII), since that is the more complete answer. *Serves:* T1 (the
editor band inherits its `h_max` from one source), and removes a second
whole-buffer walk from every install today. *Verify:* a test opens a fixture
mixing ASCII, tabs, accented Latin and CJK lines and asserts the three paths
(install_preview, Edit branch, band) agree on `h_max` to a pixel, plus the
scrollbar extents match between modes.

**R2 · One patch tail.** (S)
`patch_plan`, `patch_json_plan` and `patch_jsonl_plan` end identically: set the
revision, refresh the status fact, install an empty list of the plan's height,
bump the patch counter. Extract `Tab::finish_paid_patch(...)` and call it from
all three. *Serves:* T2/T3/T4 — and the status-fact refresh in one place is
what T4's counters plug into. *Verify:* the existing patch tests are the proof;
nothing else changes.

**R3 · One scroll install.** (S)
`content/viewport/offset = ratio*content + scroll_by(0)` is written out in
`relayout`'s Edit branch, `install_preview`, `relayout_band` and
`poll_preview_job`. Extract
`Tab::install_scroll_geometry(content, view_h, ratio)`. *Serves:* T1, S4 and the
band work — every one of them must change the same four lines. *Verify:*
the scroll/ratio tests already in the suite.

**R4 · Split app.rs by what it already is.** (M)
`app.rs` is 9 789 lines, ~4 776 of them its test module, and 279 functions:
everything from the streaming loader glue to the status bar's Copy button.
Split it into a directory module without changing a line of logic:
`app/mod.rs` (the `App` struct + event surface + paint), `app/tab.rs` (Tab,
revision/dirty/save/load glue), `app/preview.rs` (relayout, install, bands,
patch paths, preview jobs — the whole orchestration that P1–P4 edits),
`app/status.rs` (bar text, buttons, copy), and `app/tests.rs` (the suite keeps
its access to everything as a child module). *Serves:* every item; each polish
diff stops being a needle in a 10k-line haystack, and the plan's equality tests
land next to the layouts they check. Do it after R1–R3 so the shared code is
extracted once, not moved three times. *Verify:* `cargo test` byte-for-byte the
same pass/fail list before and after.

**R5 · One band dispatch.** (S)
`paint_panes` builds each planned kind's band in its own match arm and paints
them all the same way. Extract the arm bodies into one
`fn band_for(kind_or_plan, ...) -> DisplayList`, so S1's CSV, T1's editor band
and S4's measurement each add an arm, not a copy-paste. *Verify:* the
band-vs-full-layout tests already assert what S1 needs.

### P1 — Typing

**T1 · Give the editor pane a band.** (M)
The editor never wraps: one row per line at a fixed height. It needs no
measurement at all, so lay out only the visible rows.

- `layout::text::editor_band(buf, width, ctx, first_line, last_line)` → a
  `DisplayList` whose ops sit at `pad + line * line_h`, with
  `height = pad*2 + line_count * line_h`. The mono fast path already knows the
  row geometry; this is the arithmetic, not a new wrapper.
- The tab keeps the band and the line range it covers; rebuild it when the
  scroll leaves the range, the width/zoom changes, the line count changes, or an
  edit touches a visible line. Any rebuild is O(visible), so a keystroke costs a
  screenful, never the document.
- `h_max` comes from `layout_plain_edit`'s `text_w` on open, width change, zoom
  and reload, and is updated by an edit only when the edit log names a line that
  could be the widest. Today every install walks every line for it.
- The band replaces **three** whole-buffer builders, not two:
  `relayout`'s Edit branch (`layout_plain` + its own widest-line walk),
  `install_preview` in Split (`layout_plain` + the wider-line walk for `h_max`),
  and `relayout_band`'s `layout_plain_edit` — so streaming loads, the async wait,
  mode switches and tab switches get the same win. So does a resize or a zoom
  drag in Edit/Split, which takes that path on every step: 90–100 ms per step on
  an 8 MB file today, a screenful after.

*Should buy:* code 122 → under 30 ms, jsonl 65 → under 30, log 71 → under 30,
json 208 → under 120 (T3 then takes it lower), markdown 985 → under 100 (T2 then
takes it lower). Memory: 262k–335k ops and their strings go.
*Risk:* caret placement, selection, horizontal scroll, clipping, scroll range.
`PreviewView::paint` scrolls a y-band-indexed list today; the band is small
enough that it needs no index. The targets apply to Split and Edit: Preview does
not pay the editor layout at all (the `mode != Preview` guard), and the gate
measures typing in Split for exactly that reason.
*Verify:* a test that paints a window from the band and from the full
`layout_plain` and compares canvases pixel for pixel (tabs, non-ASCII, long
lines), plus the existing editor UI tests; then the gate.

**T2 · Parse only the edited Markdown block.** (L)
`Document` gains a parallel `Vec<(u32, u32)>` of block line spans — *not* fields
inside `Block`, so `Block`'s `PartialEq` keeps comparing content and
`PanePlan::Markdown::patch`'s prefix/suffix match keeps working. On an edit, find
the block run that contains the changed lines (back to a blank line or block
start; forward to the run's end, extending through an unclosed fence or to the
list's end), re-parse only that slice, splice blocks and spans, then hand it to
the existing patch, which re-measures the changed run. An edit that cannot be
localized falls back to a full parse — the honesty the JSON patch already has.

*Should buy:* markdown typing 985 → under 60 ms. Today a keystroke on a big
document costs a whole parse before anything updates, inline in the gate and on
the worker in a window; after T2 it costs the edited block.
*Risk:* block context (fences, lists, tables, quotes) and splice equality.
*Verify:* for each block kind and each edge — opening a fence that runs to EOF,
deleting the blank line that joined two paragraphs, editing a table row, an edit
inside a nested list item — assert the spliced `Document` equals
`parse_buffer` of the whole text and the resulting plan equals a full plan.

**T2 landed (Oct 2026).** The splice runs at the edit, on both the typing and
the paint-time path, and the plan re-measures only the named run:

- `block::patch_document` takes the document by value, rebases its spans in
  place, and `Vec::splice`s both `blocks` and `spans` — the kept head and tail
  are the caller's own blocks, never deep clones of a 40k-block AST. It returns
  the run it replaced (`(prefix, old-run, new-run)`) with the document; a
  refused edit returns the document untouched and no run.
- The reference-map guards: an edit whose edits are line-structural, or whose
  removed bytes cannot be reconstructed, still goes whole when kept definitions
  or resolved links are in play. A pure in-line insertion rewrites one line and
  cannot change the map unless a def line changed — the pre-edit line is
  reconstructible from `EditSpan::at` (the offset is identical before and
  after), so a def rewrite is caught exactly; otherwise the splice reuses the
  old parse's map, and a keystroke prices no scan of the document.
- `Document` carries `defs` so that reuse is free; the pane keeps a current
  document beside its revision for the whole re-parse fallback.
- `MdPlan::patch_run` measures and splices only the run's blocks; the
  compare-derived `patch` stays for tests that hold two whole documents.
- Gate on the 8 MB fixture (release): markdown `type` 695 → 1.9 ms, `end`
  2.4 ms; the first keystroke after the Split replan 2.1 ms, one at the
  document end 25 ms. Structural asserts unchanged: a keystroke patches, and
  what reaches the worker is never the document.

**T3 · Attribute and cut the rest of the JSON keystroke.** (M)
After T1, roughly 110 ms of json's 208 is unexplained. Candidates: `Doc::patch`
shifting `plan.slots` and `spans` for 545k nodes, `plan.patch_node`,
`sync_dirty`, `follow_caret`. Probe first (timers in the release breakdown, not
guesses), then fix what it shows; the likely one is the slot/span move, which
M2 would shrink. *Should buy:* under 30 ms. *Verify:* gate (both caret
positions, G4) + the existing patch-equals-parse tests.
>
> **Status (Oct 2026 audit): still open.** The gate measures json at both
> caret ends today: `type` 119 / `end` 119 ms on the sync-patched path — the
> topics above were reworked by the patch rounds (`Doc::patch_node`'s slot
> move went away with the virtualized tree's plan; `sync_dirty` starts at
> T7's watermark), and the number stopped moving once T7 landed, so the
> residual ~119 ms is `plan.slots`/spans maintenance the measurement cannot
> name yet. The node may legitimately be smaller now; run the attribution
> probe in `pipeline_breakdown` before touching anything — §9's recipe is
> the rule, not the localhost numbers.

**T4 · Count status facts incrementally.** (S)
`log_counts` rescans 20 000 lines on every log keystroke and
`record_count_status` walks every record on every JSONL keystroke. Keep both as
running counters built from the edit: for a log, the severity words of the lines
added and of the lines the edit replaced or removed, inside the first 20 000
lines the full scan reads (entries below that boundary count, as they do today);
for JSONL, the error count carried beside each record and summed as the index
moves. *Should buy:* under
1 ms each. *Verify:* unit tests for add/remove/replace and a gate run.

> **T4 landed** — a per-line severity `LogTally` on the log tab is priced by
> the edit batch (single localized edits patch the byte array, everything else
> rescans), and the JSONL tab keeps `records_errors` next to the records so the
> status is a running sum of the records' `Value` results; both agree with a
> recount in tests.

**T5 · Make find incremental.** (M)
`rescan_find` runs from `install_preview`, and a new revision makes it call
`Find::rescan`, which searches the whole buffer; `Find` already narrows the old
match list when the query is extended, so it is the edit case that costs. After
an edit, matches strictly below the changed run shift by the edit's line delta
(multiline edits widen it), and everything from the first changed line is rescan
from there — a full rescan only for opening the bar, changing the query in a way
that cannot narrow, or an edit that cannot be localized. *Should buy:* under 10 ms per key with the bar
open on 8 MB. *Verify:* a test for matches after an edit above and below the
caret, deletions, and an edit crossing a line break, plus a probe in
`pipeline_breakdown`.

> **T5 landed** — `Find::patch` rebuilds the list for one edit span: matches
> above keep their positions, matches below shift by the line delta, and only
> the edited run `[start, start+inserted)` is searched anew; anything wider
> (batch, no span, an edit past the buffer) still falls back to the full
> rescan. Probe: 51.4 ms full rescan → 0.4 ms per keystroke on the 8 MB code
> fixture.

**T6 · Decide CSV typing, with a number.** (S)
CSV has no plan, so an edit re-parses (47 ms) and re-lays out (16 ms) the capped
sheet, and the gate measures none of it. Add the measurement (gate row), then
either patch the changed rows' cells or record it as a documented limit. A
decision item, not a commitment: a CSV is a viewer's kind first.

**T7 · Walk the dirty compare from the edit.** (S)
`sync_dirty` calls `same_lines`, which walks from line 0 and stops at the first
difference, so a keystroke at the end of an 8 MB file compares the whole file
each time. Two costs, both O(document) per keystroke:

- `same_lines` starts by counting the saved text's lines (`saved.split('\n')
  .count()`), which walks every byte before the comparison even begins; cache
  that count beside the saved text.
- the line walk itself can start at a per-tab watermark — the smallest line
  edited since the file was loaded or saved — because nothing above it differs.
  The edit log cannot supply that line on its own: `install_preview` calls
  `forget_edits`, so the watermark is a tab field, raised by each edit and reset
  on save/reload, not read from the log at compare time.

*Should buy:* the dirty flag in constant time wherever the caret is. *Verify:* a
test that edits the last line of a large buffer and compares `dirty` against a
full comparison, plus the two caret positions in G4.

> **T7 landed** — the saved text keeps its line count beside it
> (`tab.saved_lines`, no per-compare split), and the compare starts from the
> tab's `edited_from` watermark, the smallest line any edit since the
> load/save touched; G4's end-caret keystroke now walks the changed run, not
> the document, and the dirty answer is still the oracle's.

### P2 — Scrolling, resizing, zooming

**S1 · Band the CSV grid.** (M)
17 011 ops and 4.65 ms/frame is the last document-sized pane. CSV has no plan
today — the new `layout::csv::plan` records what `layout` already recomputes each
call (column widths, the header geometry, the capped row count, a row height),
and a band painter draws the rows in view through the same `paint_panes` path
JSON/JSONL use (a CSV branch of the band dispatch, beside theirs). The
per-install row index (`preview.rows` over 17 011 ops) shrinks with it.
*Should buy:* under 1.5 ms/frame, under 500 ops. *Risk:* zebra stripes, the
header band, per-column ellipsis. *Verify:* the band against the full list pixel
for pixel, then the gate.

> **S1 landed** — `layout::csv::plan` records the width scan (column widths,
> x positions, the numeric-right-align decision) and a unit index over header,
> data rows and the cap note; `layout_band` draws only the units in view from
> the same painter the full list runs, and the sheet rides on the tab beside
> its revision. `layout::PanePlan::Csv` joins the planned kinds, and CSV typing
> now answers T6 with its measurement: a keystroke re-parses the capped sheet
> and replans inline (4.1 ms at the gate's scaled view; 314 band ops, frame
> 2.8–3.6 ms, was 4.65 ms and 17 011 ops).

**S2 · Keep the Markdown band between frames.** (S)
The band re-renders every visible block per frame (4.30 ms). Cache the band with
the document range it covers and reuse it while the scroll stays inside that
range and the revision/width are unchanged. *Should buy:* under 1.5 ms/frame
inside a band. *Verify:* a counter test that scrolling within one band renders
nothing, plus the gate.

> **S2 landed** — the tab keeps the last markdown band with its covered units
> and the revision/width/zoom/scale it answers to; a scroll whose viewport's
> units all lie inside the covered range paints the cached ops, anything else
> builds fresh and becomes the next cache. Counter test proves the reuse, and
> the gate row reports it (frame 1.82 ms, was 4.30 ms of per-frame re-render).

**C1 · Fill code's tint checkpoints ahead of a far band.** (S)
The line plan's `state_at` is lazy: the first banded paint whose top lands in a
distant checkpoint with nothing known between it and the caret scans from the
last known checkpoint, which after a top-of-file edit is checkpoint zero — a
one-frame ~590 ms pass over an 8 MB document (the gate's G4 `end` row for code
measures it). Every later band near that place is a checkpoint-hop away, but an edit
above resets the states again. The fix should precompute or resume the
state cheaply: fill checkpoints during the line plan's own build when it runs
on the worker, and only line-wise when the plan is built inline. *Should buy:*
the first far band under 10 ms. *Verify:* a band counter test (no scan past
the band when the checkpoints are known) and the gate's G4 `end` row.

> **C1 landed** — the worker plan fills its checkpoints as it lays
> (`build_plan_states` with the file's extension hint); the split/resize replan
> adopts the width-independent states from the plan it replaces
> (`LinePlan::adopt_states`), so the fill survives a mode switch; a keystroke
> re-proves them with one bounded scan (`STATE_EVERY` × `STATE_VALIDATE_SPANS`
> lines): a benign edit re-proves every checkpoint, a state-changing one wipes
> only past the first boundary it breaks (`LinePlan::patch`); and where no
> worker exists (headless and CLI opens) the plan fills at load instead —
> `App::new_streaming` pre-plans the big code pane, `relayout`'s width hand-off
> carries the states over the resize by adoption. `--pretty`, which only ever
> needed to echo bytes, now returns before any `App` is built at all — the
> load it once paid before printing is gone (an 8.3 MB file prints in ~0.2 s
> wall, essentially all of it the console write). Verified by the two band
> tests the entry asked for (an eager plan's band is the lazy one's; a patch
> near a known checkpoint scans only the touched window) — and the gate: code
> `type` 1.6 ms, `end` 1.0–1.8 ms (was ~590 ms once), first frame 14 ms, with
> the fill in `load` (~640 ms, budget 3000), all kinds within budget. A
> regression pin sits at app level — `a_width_change_hands_the_plan_over_and_a_stale_plan_rebuilds`
> — driving both halves of the hand-off's contract
> through the real event loop — width work on a current plan lands a fresh
> plan's height without an edit patch, and an edit that slipped past the
> edit-time patcher (Edit mode) forces a rebuild that matches the *edited*
> buffer at the new width, never the stale rows.

**S3 · Prove the resize/zoom path, don't redesign it.** (S)
Only one worker job runs at a time (`relayout` returns early while
`preview_pending`), and `poll_preview_job` drops a result whose tab, revision,
kind, mode, width, scale, zoom or collapse generation no longer match, marking
the layout dirty so the next paint starts a job that does. So a drag coalesces,
but nothing tests the end state. Add the test: five resizes during a pending job
end with the last size's plan installed and no stale plan painted. No behavior
change. Carried forward by the Oct 2026 audit: no such test exists in the
suite yet (it is the one C1-adjacent semantic without a regression pin — both
the hand-off and the stale-job drop are proven in their own tests, not
across a worker round-trip).

**S4 · Measure the stream pump's tick.** (S)
The gate opens headless, so `Loader::drain` runs synchronously at open and the
pump's real cadence is never measured. In a window, one 1 MB chunk is drained per
`Tick` and the band is rebuilt for the grown buffer — the code's own comment
sizes a chunk at "~15–20 ms", which is a whole frame on a 60 Hz display before
the paint. Measure it on a 64 MB stream (drain + band + editor rebuild, per
tick, in Split) and only then decide whether the chunk or the pump's work needs
to change. After T1 the editor half of that tick is O(visible), so re-measure
there rather than pre-emptively tuning `STREAM_CHUNK_BYTES`. Carried forward
by the Oct 2026 audit: the gate's `readable` row measures the whole open,
not the per-tick slice, so this number still does not exist.

### P3 — Opening a document

**O1 · Verify the shipped open, then leave it alone.** (S)
A window's first paint for a big JSON/Markdown is a prefix band, with the real
pane arriving from the worker; the gate's 538 ms / 1 669 ms are the inline path
(1.2). G2 supplies the real number. Only if "readable" on 8 MB is over ~1 s after
G1/G2 does this item propose anything: a longer prefix, or a lower
`PREVIEW_ASYNC_LINES`. Both are internal knobs, and neither is touched before
the measurement.

### P4 — Memory

**M1 · Measure what T1 frees.** (S, with T1)
The editor band removes 262k–335k ops and their strings; report live before and
after in the same table. No separate change.

**M2 · Compact the JSON plan, if T3 asks for it.** (M)
`NodeSlot` is `top, h: f32` plus five `u32`s (`parent`, `next`, `ord`, `id`,
`first_id`) — 28 bytes — and there is one per node (545 208 on the fixture), with
a matching span per node. That, plus the parsed values, is a large part of the
121.5 MB live for an 8 MB file. Only do this if T3 shows the patch's slot/span
move is the cost, or if live memory itself becomes a target: the plan is the
document the pane paints from, and shrinking it trades clarity for bytes.
*Verify:* the plan equals a full build after every patch test, and the gate's
live number.

### P5 — Hold the line

**B1 · Budget typing.** (S)
Once T1/T2/T7 land, assert per-kind typing in the gate at both caret positions
at ~3× the new measured value, the same doctrine as the other budgets, and
tighten as the numbers fall. The structural assertions (patched, no worker) stay.
>
> **Status (Oct 2026 audit): half landed.** The half that needed the numbers
> is done — the gate measures both caret positions per kind
> (`Cost::typing_ms` / `typing_end_ms`, `code end` now the headline row) and
> prints them; the enforcement half is not: `Budget` has no typing fields,
> so a typing regression fails nothing today except CI's eyes on the
> printed rows. Budgeting it is a small diff (two fields + their 3×
> calibration) gated on a stable three-run baseline of the current values —
> do it with T3, so json's number is final before it is frozen.

**B2 · Budget the bands.** (S)
Pane ops < 500 for CSV after S1, and the editor band's ops < 500 in Split after
T1, so a future change cannot quietly reintroduce a document-sized list.
>
> **Status (Oct 2026 audit): the planned half is landed.** `Budget::pane_ops`
> bounds what every planned pane may hold (json/jsonl/markdown/log/code at
> 2 000 ops, CSV bounded by its parser's row cap instead, so it cannot grow
> with the file), and `check` fails the build naming the kind and the
> count. The editor band is the uncovered half — its op count is not a
> gate row at all; counting it into the existing `pane_ops` check in Split
> is the natural home, guarded by the measured screenful.

**B3 · Keep one command honest.** (S)
Every budget failure must name the kind and the number; the gate stays green on
CI, so any red is a real regression.

---

## 5. Order

| # | item | size | depends on |
|---|---|---|---|
| 1 | G1, G3, G4 | S | — |
| 2 | R1–R3 | S | — |
| 3 | R4 (split `app.rs`) | M | R1–R3 |
| 4 | T1 (+M1, B2-editor) | M | R1–R3, G3 |
| 5 | G2, O1 | S | G1 |
| 6 | T2 | L | R4 |
| 7 | T3 (+M2 if it asks) | M | T1, G4 |
| 8 | T4, T5, T7 (landed) | S, M, S | R2 |
| 9 | S1, S2 (landed) | M, S | R5 |
| 10 | S3, S4, T6 decision | S | G2, G3, G4 |
| 11 | B1, B3 | S | T1, T2, T7 |

G1/G3/G4 and R1–R3 come first because every later claim leans on those numbers
and those unifications. R4 then splits `app.rs` while the diff is small, and is
pure movement — the suite is its proof. After that, T1 is a small diff against
one focused file; T2 is the only L and unlocks markdown entirely; T4 lands in
R2's status point rather than three call sites; S1 needs only R5's extra arm.

---

## 6. Definition of done

- All six kinds inside their budgets, with **typing asserted** at both caret
  positions for every kind the gate measures it for — JSON, JSONL, Markdown,
  code, log — CSV's either patched or recorded as a limit, per T6, and the
  editor pane's band ops bounded.
- The gate reports both numbers: **first frame** (something on screen) and
  **readable** (the real pane installed), on the shipped async path.
- The streaming pump's per-tick cost is measured (S4), and any chunk-size change
  follows that number rather than the constant's original comment.
- The three widest-line scanners answer identically (R1's test), the patch tail
  and the scroll install exist in exactly one place (R2/R3), band builders
  dispatch through one function (R5), and `app.rs` is split with the suite's
  pass/fail list byte-for-byte unchanged (R4).
- Every fast path has an equality test against the path it replaced: editor band
  vs `layout_plain` (canvas), `h_max` incremental vs a full scan, markdown splice
  vs `parse_buffer`, CSV band vs full layout (canvas), find matches vs a full
  rescan, status counters vs full scans, dirty flag vs `same_lines`.
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`
  and `cargo test --release -- --ignored perf` green at every commit.
- No visual change: the screenshot fixtures in `assets/screenshots` and the
  pixel tests are untouched, and no new keystroke or menu exists.

---

## 7. Non-goals

- No UI, layout, wording, colour or interaction changes. No new file kinds, no
  new shortcuts, no change to the 64 MB refusal limit.
- No async redesign: the worker, the prefix band and the one-job-at-a-time rule
  stay as they are; this plan only measures them and removes work around them.
- No font, shaping or raster work: the gate does not show it as the cost, and
  the glyph caches are shared.
- No status-bar micro-optimisation before the items above: churn is
  0.00–0.17 MB/frame today, an order of magnitude below the budget.
- No `unsafe`, no SIMD, no dependencies.

---

## 8. Risks and rollback

- **Correctness drift between fast and slow paths** is the central risk. Each
  item is paired with an equality test, and the existing plan/patch tests
  (`patch == parse_spanned`, "the virtual tree plan matches a full layout",
  Markdown block tests) are the model for the new ones.
- **Scroll geometry** depends on `editor_pane.list.height` and on `h_max`. T1
  replaces both with arithmetic; a wrong row height or pad shifts every
  scrollbar and the caret. Test the scroll range and the horizontal bar, not
  just the pixels.
- **Markdown context**: fences, lists and tables can make a local parse differ
  from a whole one. The fallback to a full parse is the safety valve; the
  equality tests are the proof.
- **Hidden serialisation**: adding a band/plan must not move work to another
  thread without the same checks `poll_preview_job` already makes (revision,
  width, scale, zoom, collapse generation, mode).
- **The refactors themselves** move ~10k lines with no behavior change; the
  guard is the suite's pass/fail list (R4) and the equality tests R1/R5 add for
  the code they unify. One commit per item, as everywhere else.
- **Rollback**: every item is one commit behind a green gate. If an equality
  test cannot be written for an item, that item is not ready.

---

## 9. Appendix — probe recipe

Temporary probes go in `pipeline_breakdown` (release, `--ignored --nocapture`)
and are reverted before committing. The ones used for §2.1:

```rust
// The editor pane's whole-buffer builders.
let buf = crate::core::buffer::Buffer::from_text(&json);
let t = Instant::now();
let ed = layout::text::layout_plain(&buf, 712.0, &ctx, false);
println!("editor plain {:8.1} ms ({} ops)", t.elapsed().as_secs_f64() * 1000.0, ed.ops.len());
let t = Instant::now();
let (band, text_w) = layout::text::layout_plain_edit(&buf, 712.0, &ctx);
println!(
    "editor edit  {:8.1} ms ({} ops), widest {text_w}",
    t.elapsed().as_secs_f64() * 1000.0,
    band.ops.len()
);

// A warm open, so the filesystem is not billed to the app.
let t = Instant::now();
let _loaded = crate::load::load(&path).unwrap();
let read = t.elapsed().as_secs_f64() * 1000.0;   // second read: 4.8 ms, first: 571 ms
```

Numbers quoted in this document are from one release run on one machine. They
are for ranking and for detecting an order-of-magnitude change, not for
budgeting; the gate's budgets remain the 3×-of-three-runs doctrine it already
documents.
