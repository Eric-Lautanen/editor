  - `app.rs`: kind-dispatched relayout, `collapsed` set, click-to-toggle, JSON status in the status bar
    (`error 2:5`, `3 records, 1 error`); `--pretty` re-anchors JSONL errors to file lines.

### M3 acceptance checklist (all verified headlessly; window pass still manual)
- [x] `samples/sample.json` renders a tinted tree; `--pretty` output re-parses identically
- [x] Invalid JSON shows a banner (`! line:col: msg` + snippet + caret) and a status error
- [x] `samples/sample.jsonl` shows one tree per valid record + 1 error banner + separators (incl. its broken line 3)
- [x] Clicking `[+]`/`[-]` collapses/expands; inner toggles keep working; zoom/reflow keeps place
- [ ] Same three checks eyeballed in a real window (tree colors, toggle hit areas, banner)

- **M4** document Markdown, 49 unit tests, `cargo fmt --check` / `cargo clippy
  --all-targets` / `cargo build` all warning-free with zero `allow`s:
  - `parse/markdown/ast.rs`: `Document`/`Block`/`Inline` model (headings, paragraphs, fenced code,
    quotes, ordered/unordered/task lists, GFM tables, rules; code spans, emph/strong, strike,
    links, images, soft/hard breaks). Parsing is total — unknown input degrades to paragraphs.
  - `parse/markdown/block.rs`: ATX + setext headings, fenced code (info string, unclosed runs to
    input end), blockquotes with lazy continuation, nested lists with task boxes, `---`/`***`/`___`
    rules, GFM tables (escaped pipes, alignment, ragged rows padded), block HTML skipped.
    Deliberate omission: indented code blocks render as text.
  - `parse/markdown/inline.rs`: CommonMark-style delimiter matching (flanking + odd-match rules,
    intra-word `_` stays literal), emphasis reaching across code spans/links, `[t](href)` links +
    `<autolinks>`, `![alt](src)` images, `~~strike~~` (exact pairs), backslash escapes, soft breaks
    and two-space/backslash hard breaks. Inline HTML tags stripped, content kept.
  - `layout/markdown.rs`: per-block sizes (scaled bold headings, h1/h2 rules), mixed-style
    proportional wrapping (`wrap_spans`, per-character faces), inline-code panels, strike lines,
    accent links with hit regions (click → status bar shows the URL; opening is M6), quote bars,
    bullets/numbers/task boxes with hanging indents, code panels, tables (measured columns,
    alignment, ellipsis truncation over budget), image placeholders.
  - Restored for M4: `FontStyle::{Bold, Italic, BoldItalic}`, `Theme::quote_bar`, `HitAction::Link`.
  - Fixed along the way: tree-layout-class early-wrap bug (rows broke at the last space when the
    whole line fit) + inline code spans blocking emphasis matches, both caught by rendered snapshots.

### M4 acceptance checklist (verified headlessly + eyeballed via `--render` PNGs)
- [x] `samples/sample.md` renders: scaled headings + rules, bold/italic/code/link row, nested +
      task lists, quote bar, code panel, aligned table with rule, closing rule
- [x] Adversarial inputs (unclosed fences/markers, deep nesting, ragged tables, unicode, empty
      file) degrade gracefully with no panics
- [ ] Same eyeballed in a real window (proportional faces, link clicks, table alignment)

- **M5** editing, 76 unit tests, `cargo fmt --check` / `cargo clippy --all-targets` /
  `cargo build` all warning-free with zero `allow`s:
  - `ui/editor_view.rs`: the editor model — caret + optional anchor (selection), goal
    column for stable vertical navigation, `move_key` (arrows/Home/End w/ shift-extend,
    arrow-from-selection collapses to the travel edge), `insert_text` (replaces selection),
    `delete_key` (whole selection or one char; backspace runs merge via `UndoStack`),
    copy/cut/paste, select-all, undo/redo landing the caret at the edit origin, and
    overlay painting (selection rects + caret bar) over the plain-text layout grid.
    `ensure_visible` scrolls the caret row into its pane.
  - `core/undo.rs`: `UndoStack` API trimmed to `push`/`pop_undo`/`pop_redo`; merge rules
    unchanged (contiguous single-char runs; Ctrl+Z/Y toggle between them).
  - `layout/text.rs`: exposed `line_height` so caret math and pane scrolling share the
    same grid (all caret/selection math already lived here from M2/M4).
  - `app.rs`: event dispatch reorganized — `on_key` (chords, then editing keys, then
    scroll navigation), `on_mouse_down/move/up` (click-to-place caret + drag select in
    Edit/Split bottom pane; read-only toggles/links elsewhere), `on_text` (printable
    input only; control chars stay on KeyDown), Ctrl+S save (dirty flag clears, title
    dot clears; Untitled rejected until M6 dialogs), `follow_caret` scroll upkeep, and
    status bar with `Ln, Col` + `[sel]` marker in Edit mode.
  - Shortcuts added: `Ctrl S` save · `Ctrl C/X/V` copy/cut/paste · `Ctrl A` select all ·
    `Ctrl Z`/`Ctrl Y` undo/redo · arrows/Home/End/PageUp/PageDown now move the caret in
    Edit/Split (window scroll keeps them in Preview); `Tab` inserts four spaces.

### M5 acceptance checklist (verified headlessly; window pass still manual)
- [x] Typing/Enter/Tab/Backspace/Delete edit the buffer in Edit & Split; ignored in Preview
- [x] Caret movement incl. goal column, selection (shift + mouse drag), arrow-out-of-selection
- [x] Undo/redo round-trips (typing runs merge, selection replace = 2 steps, backspace runs merge)
- [x] Copy/cut/paste through the `Host` clipboard (NullHost-backed tests)
- [x] Ctrl+S writes the file and clears the dirty flag; Untitled buffers report in the status bar
- [x] Split bottom pane edits re-render the document pane live; caret stays visible
- [ ] Same in a real window: caret blink-free painting, drag-select feel, clipboard vs system

- **M6 – Windows polish + multi-file** (Oct 2026), 89 unit tests, `cargo fmt --check` /
  `cargo clippy --all-targets` / `cargo build` warning-free with zero `allow`s:
  - **Custom title bar** (`ui/titlebar.rs`, portable): wordmark, one small tab per open
    file (dirty dot, × per tab, hover highlight, proportional shrink with ellipsis),
    min/max/close buttons drawn with rects + the `×` glyph. Win32 side: `WS_POPUP |
    WS_THICKFRAME | WS_SYSMENU | WS_MIN/MAXIMIZEBOX`, `WM_NCCALCSIZE` (client = whole
    window; maximized insets by the frame), `WM_NCHITTEST` (edges resize, `Drag` →
    `HTCAPTION` so the OS moves the window, everything else stays a client click).
  - **Tabs** (`app.rs`): `Tab { path, kind, buffer, editor, editor_pane, collapsed,
    status_extra, scroll }`; `App` derefs to the active tab. Click/`×`/`Ctrl+W` close
    (dirty tabs warn once, any other action disarms), `Ctrl+Tab`/`Ctrl+Shift+Tab` cycle,
    re-opening a path focuses its tab, a lone untouched Untitled is replaced by the
    first opened file, closing the last tab yields a fresh Untitled.
  - **Open files**: `Ctrl+O` → `Host::open_file_dialog` (Win32 `GetOpenFileNameW`,
    comdlg32, filter from `EXTENSIONS`); drag-drop anywhere → `WM_DROPFILES`
    (`DragAcceptFiles`/`DragQueryFileW`) → `Event::FileOpen` per file, each opens a tab.
  - **Help** (`ui/help.rs`): `F1` toggles a dimmed overlay listing modes, every
    shortcut, file types and how to select/copy; any click or `Escape` closes it.
  - **Context menu** (`ui/menu.rs`): right-click opens a small popup — copy/cut/paste/
    select all (Edit/Split) or copy-all (Preview), open/save/close/help — hover
    highlight, click-outside dismiss.
  - **Cursors**: `Handler::cursor_at` → I-beam over editor panes, hand over tree
    toggles/links/tabs/window buttons, arrow elsewhere; Win32 `WM_SETCURSOR` +
    `SetCursor(IDC_IBEAM/IDC_HAND/IDC_ARROW)`.
  - **Status bar** always names the mode and ends with an `F1 help` hint.
  - **`--install`/`--uninstall`** (`platform/install.rs`): HKCU `Software\Classes`
    via advapi32 — ProgID `Readitor.Document` (+ `DefaultIcon`, `shell\open\command`
    `"exe" "%1"`), `OpenWithProgids` value per claimed extension, `SHChangeNotify`;
    no admin needed. Non-Windows keeps pointing at `packaging/`.
  - Fixed along the way: nested JSON `[+]` hit rects now span the key text *and* the
    marker (clicking the glyph on nested rows used to miss); `Error::Unsupported`
    folded into `Error::Platform`; stale M5 TODO in the Win32 backend removed.
  - Left for M6 follow-up: OS-theme auto-follow (still `Ctrl+T`), GUI-subsystem +
    `AttachConsole` for `--help`. (Both landed in round 2/3 — see below.)

### M6 acceptance checklist (verified headlessly; window pass still manual)
- [x] Tabs: open (dialog/drop/`Ctrl+O`), dedupe by path, click/`×`/`Ctrl+W` close with
      dirty warning, `Ctrl+Tab` cycle, per-tab mode/scroll/caret state
- [x] F1 help overlay paints and toggles; click/Escape dismiss
- [x] Right-click menu: Copy all (Preview) / copy/cut/paste/select all (Edit), open,
      save, close, help; click-outside dismiss
- [x] Title bar: tab/window-button hit test, tabs shrink to fit, hover states
- [x] Cursor shapes over panes, toggles and chrome
- [x] `--install`/`--uninstall` compile and write/clear the HKCU keys (registry
      contents still want a real `--install` run + Settings check)
- [ ] Real window: title-bar drag/resize/maximize feel, tab clicks, open dialog,
      drag-drop, cursors, help/menu rendering

### M6 round 2 (Oct 2026, user feedback) — delivered, 94 unit tests green
- **Preview highlighting**: drag-select over rendered rows (any file kind; rows are
  baseline-grouped text ops), selection bands painted, `Ctrl+C` copies the selected
  rows, I-beam cursor over preview text, cleared on mode/tab switch or Escape.
- **Split side-by-side**: preview left / editor right (55/45), auto-scaled; the editor
  grid lays out at the pane width so wrapping, caret and hit-testing all match.
- **Touch**: `RegisterTouchWindow` + `WM_TOUCH` — touch drags pan the document
  (taps still click); no more accidental drag-selection from touch.
- **Drag auto-scroll**: a 40 ms `SetTimer` heartbeat → `Event::Tick`; holding a
  selection drag at a pane edge keeps scrolling and extending (editor + preview).
- **Clickable status bar**: `Preview`/`Edit`/`Split`/`Help` buttons (active mode
  highlighted, hand cursor) — modes no longer chord-only.
- **Cursors fixed**: `IDC_IBEAM` is 32513, not 32613 (that is `OCR_IBEAM` and
  `LoadCursorW` returns NULL for it — the pointer *disappeared* over editor panes;
  caught by a `predefined_cursors_load` test).
- **Links open the browser**: `Host::open_url` — `ShellExecuteW` on Windows
  (`xdg-open`/`open` when the Linux/macOS backends land); the status bar still
  shows the target.
- Dynamic resize was already live (relayout on every `WM_SIZE`); verified by
  headless renders at multiple sizes.
- **OS theme auto-follow**: `AppsUseLightTheme` via `RegGetValueW` at startup,
  `WM_SETTINGCHANGE` → `Event::ThemeChanged` while it actually flips (cached on
  the backend `State`); `Ctrl+T` or `--dark` switches to manual and stops following.
- **Reload-on-external-change**: the 40 ms tick doubles as a ~2 s file watcher —
  clean buffers reload silently (scroll kept, caret clamped), dirty ones get
  `file changed on disk — Ctrl+R to reload`; `Ctrl+R` force-reloads any time.

### M6 round 3 (Oct 2026) — window icon + GUI subsystem, 98 unit tests green
- **`gfx/icon.rs`** (portable): renders the app icon from the embedded font
  atlas — a 32×32 rounded accent tile (`0x0969DA`) with a white bold "R",
  anti-aliased via a signed-distance rounded-rect + glyph coverage blend.
  Returns packed `0xAARRGGBB` pixels; X11/macOS will reuse it as-is.
- **Win32 wiring** (`platform/windows.rs`): `CreateDIBSection` (32bpp top-down)
  + empty `CreateBitmap` mask → `CreateIconIndirect`; set as the class icon
  (`WndClassExW.icon`/`icon_sm`) and via `WM_SETICON` (small + big) so the
  taskbar and Alt-Tab pick it up. No .rc resource, no extra asset file.
- **GUI subsystem**: `#![cfg_attr(all(windows, not(test)), windows_subsystem =
  "windows")]` in `main.rs` — no more console window next to the app. Console
  output (`--help`, `--version`, `--pretty`, `--render`, errors, `--install`)
  goes through `platform::print_line`/`print_text`/`print_err_line` → Win32
  `AttachConsole(ATTACH_PARENT_PROCESS)` once, then: console handle →
  `WriteConsoleW` (UTF-16); redirected file/pipe → `WriteFile` (UTF-8, so
  `cargo run` and `--pretty > f` keep working); std handle NULL after a
  successful attach → open `CONOUT$` via `CreateFileW` and write there
  (GetStdHandle is not guaranteed to pick up the attached console). Verified
  via an explicit pipe (PowerShell `Process` + `RedirectStandardOutput`).
  Note: the agent shell's own std-handle setup is non-deterministic (it
  sometimes hands GUI children a hidden-console handle even with `>`
  redirection), so live-console verification needs the user's real cmd/WT run.
- M6 is now feature-complete; what remains is the user's real-window pass.

### Editor UX round (Oct 2026) — scrollbars, gutter, click-select, session, 149 tests green
- **Functional overlay scrollbars** (`ui/preview_view.rs` + `app.rs`): shared
  `SbGeom` geometry (thumb rect, interactive zone, travel math) now backs
  painting, the cursor, and interaction. Thumb press starts a drag that maps
  pointer position back to scroll offset (`offset_for`); a track click pages
  toward the pointer and grabs the thumb mid-bar; `MouseUp` ends the drag.
  Works for the vertical bar in all modes and the horizontal bar of the
  no-wrap editor pane (Edit + Split right).
- **Gutter is off-limits to the caret**: editor content is now inset by the
  rail width (`PaneScroll::content_x`) so text, selection, and caret start
  right of the line numbers instead of sliding under the rail at column 0.
  Clicks inside the rail are ignored (no caret placement), the pointer is an
  Arrow there, hit tests subtract the rail, and `follow_caret` pans in
  content space (visible span = pane width − rail).
- **Click-select like a real editor**: `App` tracks click sequences
  (position + 40 ms tick window) — single click places the caret, double
  click selects the word (`EditorView::select_word`, char-class runs:
  whitespace / word / punctuation), triple click selects the whole line
  (`select_line`). Dragging after a double/triple click extends the
  selection from its start.
- **Session persistence** (`settings.rs`): theme (dark + manual flag), view
  mode, open tabs, and active tab persist to one tiny text file
  (`%APPDATA%\Readitor\session.txt`, XDG/`~/Library` on Unix) — written on
  change, loaded at startup unless a file is passed on the CLI. Missing
  files are skipped; any parse error falls back to defaults.
- **Centered window**: `CreateWindowExW` now positions the 1100×800 window
  at the center of the primary monitor (`GetSystemMetrics(SM_CX/ CYSCREEN)`)
  instead of `CW_USEDEFAULT`, which parks a `WS_POPUP` window top-left.
- **M7 phase 2 landed** (buffy): `FontSystem` now parses the embedded
  Liberation TTFs live and rasterizes on demand behind the same API (no
  pre-rendered bitmap atlas); glyph cache shape unchanged. RAM stays flat —
  faces borrow the embedded bytes, glyphs rasterize per (char, size) on
  demand.
- Also this round: `mem.rs` allocation census (`--mem-report FILE`, 2-tab
  delta ≈ 113 KB — the user-visible +5 MB/tab was framebuffer churn + heap
  arena retention, not document data), duplicate-block merge artifacts from
  the concurrent-edit window cleaned up (windows.rs centering, editor_view
  tests), `on_mouse_up` stray lines removed.

### Cross-OS readiness round (Oct 2026) — CI, releases, associations, update story
- **GitHub Actions CI** (`.github/workflows/ci.yml`): `cargo test` +
  `cargo clippy` on **windows-latest, ubuntu-latest, macos-latest** every
  push/PR — the whole suite is headless (NullHost + software canvas), so the
  portable core (parse/layout/gfx/app) is exercised on all three OSes —
  plus `cargo fmt --check` on Linux. Clippy stays non-denying until the
  deferred warning cleanup lands (TODO noted in the workflow).
- **Release pipeline** (`.github/workflows/release.yml`): pushing a tag
  (`v0.1.0`, …) builds + tests on all three runners, packages per-OS archives
  — Windows zip (exe), Linux tar.gz (binary + `packaging/linux/` install
  script), macOS zip (`.app` via `bundle.sh` + `ditto`) — and publishes them
  to a GitHub Release with install/update notes.
- **File associations everywhere** (user requirement): Windows `--install`
  (HKCU ProgID + `OpenWithProgids`, no admin) unchanged; Linux
  `packaging/linux/install.sh` installs the `.desktop` entry, the
  `.jsonl`/`.ndjson` MIME package, and `xdg-mime` defaults; macOS
  `Info.plist` `CFBundleDocumentTypes` covers the same extensions.
- **Update story** (documented in README + release notes): no in-app
  updater — releases are version-numbered overwrites. Replace the binary
  (or the whole `.app`); associations keep pointing at the same path, so
  nothing needs re-registering unless the executable moves (then re-run
  `--install` / `install.sh`). `--version` prints the build version.
- `mem-census` is now a real cargo feature (`cargo run --features
  mem-census -- --mem-report FILE` works; previously the cfg was undeclared
  and the flag unreachable).

### Window-shell round (Oct 2026) — scrollbar vs. resize edge, maximize honors the taskbar
- **Overlay scrollbars clear the OS resize border**: on a borderless window the
  client area reaches the very edge, so a bar drawn flush with the border sat
  inside the strip the platform claims for resizing (measured: 5 of the 8 thumb
  columns inside the band, so most of the thumb was unclickable).
  `platform::edge_grip()` now reports that strip in *logical* px (Win32: a fixed
  4 px classic sizing border, DPI-scaled — deliberately not `SM_CXFRAME`, which
  reports 8 px on current Windows and would push the bar off the edge; X11:
  `RESIZE_BAND`), `PaneMetrics { scale, edge }` carries it into
  `ui::preview_view` geometry, and `v_geom`/`h_geom` put the bar's outer edge
  exactly on the strip boundary with the click slack inward — net visual change
  from the old build: 1 px. The maximize frame inset is measured separately
  (`frame_inset`) so narrowing the strip cannot disturb the work-area clamping.
  `PaneMetrics` replaces the bare `scale` parameter through
  `PreviewView::{paint, v_sb, h_sb, scrollbar_hit}`, so painting, cursor, and hit
  testing stay in agreement by construction; the strip is checked for every
  mode (Preview/Edit/Split) in one test. Runtime probes on the live window: over
  the thumb `HTCLIENT`, one pixel further out `HTRIGHT`.
- **App-drawn controls win the hit test**: window buttons and tabs sit flush
  with the top/right border, so a band-first `WM_NCHITTEST` ate the corner of
  whatever button the pointer was on. Both backends now ask `title_hit` first
  and return `HTCLIENT` for real controls, falling through to the resize band
  only for empty title-bar filler and panes.
- **Maximize respects the work area (taskbar/panel)**: a borderless
  `WS_POPUP | WS_THICKFRAME` window maximized to the whole monitor (measured
  before: client `1918×1198` at `(1,1)` on a 1920×1200 screen with a 48 px
  taskbar). `WM_GETMINMAXINFO` clamps to `MonitorFromWindow(…,
  MONITOR_DEFAULTTONEAREST)`'s `rcWork`, expressed so the maximized
  `WM_NCCALCSIZE` frame inset lands the client exactly on the work area
  (`maximize_metrics` is a pure, tested function, incl. a negative-origin
  secondary monitor). Verified on the real window: client `1920×1152` at
  `(0,0)` == `rcWork`, taskbar untouched.
- **X11 maximize is tracked, not guessed**: `_NET_WM_STATE` is read on
  `PropertyNotify` so a manual edge drag stands down while the WM has the
  window maximized instead of dragging a maximized window out of place.
- **`--cfg readitor_check_linux` works from Windows** (the symmetric of the
  existing Win32-from-Linux check): the Win32 module is gated on
  `any(target_os = "windows", readitor_check_win)` instead of excluding
  `readitor_check_linux`, and both cfgs are declared in `Cargo.toml`. The X11
  backend type-checked for the first time under that flag (a stray brace and a
  duplicated `intern` signature were blocking it).
- **Samples are long enough to scroll** (`samples/sample.md`, `sample.txt`,
  `sample.jsonl`, `sample.json`): the old samples fit on one screen, which is
  why no scrollbar ever appeared in a window or a `--render` snapshot.

## Review notes (Oct 2026 hygiene + gap pass)
- `PLAN.md` (root) and `docs/PLAN.md` are byte-identical; root is the working copy, `docs/` the
  published copy — keep them in sync until one is declared canonical (suggest: root wins, M10).
- OS theme-following is still manual (`Ctrl T`); automatic detection belongs to M6 Windows polish.
- Restored-by-milestone ledger (all live as of M4): `FontStyle::{Bold, Italic, BoldItalic}`,
  `Theme::quote_bar`, `HitAction::Link`. Still parked: `Host` regrows clipboard (M5) and dialogs
  (M6); `Event` regrows drag-drop (M6) and macOS open-file (M9). Remaining docs-only placeholders
  (`core/undo.rs`, `gfx/font/ttf.rs`) are re-opened by M5/M7.
- Golden-image (PPM hash) tests deferred: structural op assertions + non-blank canvas checks cover
  M3; hashes can land once the M4 renderer stops moving pixels.

### M2 acceptance checklist (Windows)
- [ ] Text is crisp at 100% and 150% display scaling; no flicker while resizing
- [ ] Wheel/keys scroll smoothly; scrollbar thumb tracks position
- [ ] `Ctrl +/-/0` zoom reflows and keeps your place; `Ctrl T` flips theme
- [ ] Status bar shows file name, kind, line count, zoom %

Time: 2026-10-12 13:42:24 UTC