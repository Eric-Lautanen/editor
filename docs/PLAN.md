  - `app.rs`: kind-dispatched relayout, `collapsed` set, click-to-toggle, JSON status in the status bar
    (`error 2:5`, `3 records, 1 error`); `--pretty` re-anchors JSONL errors to file lines.

### M3 acceptance checklist (all verified headlessly; window pass still manual)
- [x] `samples/sample_json.json` renders a tinted tree; `--pretty` output re-parses identically
- [x] Invalid JSON shows a banner (`! line:col: msg` + snippet + caret) and a status error
- [x] `samples/sample_jsonl.jsonl` shows one tree per valid record + 1 error banner + separators (incl. its broken line 3)
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
- [x] `samples/sample_markdown.md` renders: scaled headings + rules, bold/italic/code/link row, nested +
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
    via advapi32 — ProgID `Editor.Document` (+ `DefaultIcon`, `shell\open\command`
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
  (`%APPDATA%\Editor\session.txt`, XDG/`~/Library` on Unix) — written on
  change, loaded at startup unless a file is passed on the CLI. Missing
  files are skipped; any parse error falls back to defaults.
- **Centered window**: `CreateWindowExW` now positions the 1100×800 window
  at the center of the primary monitor (`GetSystemMetrics(SM_CX/ CYSCREEN)`)
  instead of `CW_USEDEFAULT`, which parks a `WS_POPUP` window top-left.
- **M7 phase 2 landed**: `FontSystem` now parses the embedded
  Liberation TTFs live and rasterizes on demand behind the same API (no
  pre-rendered bitmap atlas); glyph cache shape unchanged. RAM stays flat —
  faces borrow the embedded bytes, glyphs rasterize per (char, size) on
  demand.
- Also this round: `mem.rs` allocation census (`--mem-report FILE`, 2-tab
  delta ≈ 113 KB — the user-visible +5 MB/tab was framebuffer churn + heap
  arena retention, not document data), duplicate-block merge artifacts from
  the concurrent-edit window cleaned up (windows.rs centering, editor_view
  tests), `on_mouse_up` stray lines removed.

### Memory round (Oct 2026) — fallback fonts cost the whole file, so map them

Baseline, release build, `samples/sample_markdown.md` (1100×800): **41.7 MB working set,
31.4 MB private**, of which 22.9 MB was live heap. The census attributed 22.9 MB
to "untagged", and a size-class histogram in `mem.rs` narrowed it to *one*
19,704,352-byte allocation — exactly the size of `C:\Windows\Fonts\msyh.ttc`.
The cause: the sample contains 中文, Liberation does not cover it, so the
fallback chain read the whole 19.7 MB CJK face (plus `seguisym.ttf`, 2.5 MB)
into a `Box::leak`ed buffer for two characters. With emoji as well it was worse —
a CJK+emoji document (中文 测试 😀 🚀 🎉) reached **44.1 MB private / 54.5 MB
working set**, and one emoji pulled in `seguiemj.ttf` (12.9 MB) on top.

- **Font files are mapped, not read**: `platform::map_file` (Win32
  `CreateFileW`/`CreateFileMappingW`/`MapViewOfFile`, `FILE_SHARE_READ|WRITE|DELETE`
  so mapping never locks a font; X11 `mmap(MAP_PRIVATE)`; macOS `None` until M9)
  and `fallback::Chain::load` uses it, falling back to a plain read where a
  platform cannot map. Untouched pages never enter the working set, the pages we
  do read are shared with every other process using that font and evictable under
  pressure, and the process no longer commits the file size. Same trade the
  browsers' font loaders make.
- **`loca`/`hmtx` are read through, not copied**: `Face` used to build a
  `Vec<u32>` and a `Vec<(u16,i16)>` per face — ~4 bytes per glyph per table, so
  ~240 KB for a CJK face, to serve the few dozen glyphs a document draws. They
  are now views into the (already resident) font bytes, one bounds-checked
  big-endian load per lookup; the length validation that used to live in the
  copy is kept, so malformed faces still fail exactly as before.
- **A collection's members are parsed on demand**: `Chain` keeps slots
  (bytes + directory) and parses a member only when a lookup reaches it, instead
  of parsing every face of every candidate file eagerly. A lookup stops at the
  first face that answers, so the members behind it never allocate.
- **Measured, before → after** (same binaries, same documents, `--mem-report` and
  `Get-Process` on the live window): `samples/sample_markdown.md` live heap **22.9 MB →
  359 KB**, working set **41.7 → 19.8 MB**, private **31.4 → 9.4 MB**; the
  CJK+emoji document live heap **35.5 MB → 739 KB**, working set **54.5 →
  20.4 MB**, private **44.1 → 9.4 MB**. An emoji now costs what a Latin
  character costs.
- **Nothing looks different**: the pre-change binary was rebuilt from `HEAD` in a
  detached worktree and 25 headless renders (all four samples + the CJK/emoji
  document × five size/DPI/dark/scroll combinations) compared with `cmp` — **zero
  differing bytes** — plus a `PrintWindow` capture of the running window, also
  byte-identical. Three tests pin the new behaviour: a fallback font is mapped
  and does not grow live heap, `load` parses no member until a lookup asks, and a
  memoized lookup parses nothing new.
- Fixed on the way: `mem.rs`'s `CURRENT` was a `static mut` shared by all
  threads although the module documents it as thread-local, so parallel tests
  billed each other's allocations to whichever component was "current" — which
  is why `component_covers_glyph_cache` failed intermittently. It is a real
  `const`-initialised `thread_local` now (no lazy init, no destructor, so the
  allocator still cannot allocate while reading it).

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
  `platform::edge_grip()` now reports, in *logical* px, the **widest** strip the
  backend offers (Win32: the classic 8 px sizing border, DPI-scaled; X11:
  `RESIZE_BAND`; macOS `0.0`), `PaneMetrics { scale, edge }` carries it into
  `ui::preview_view` geometry, and `v_geom`/`h_geom` put the bar's outer edge
  exactly on the strip boundary with the click slack inward — net visual change
  from the old build: 1 px. The maximize frame inset is measured separately
  (`frame_inset`) so the strip width cannot disturb the work-area clamping.
  `PaneMetrics` replaces the bare `scale` parameter through
  `PreviewView::{paint, v_sb, h_sb, scrollbar_hit}`, so painting, cursor, and hit
  testing stay in agreement by construction; the strip is checked for every
  mode (Preview/Edit/Split) in one test.
- **The strip self-tunes instead of being hardcoded**: a fixed width has to
  satisfy two opposite wants — wide enough to grab, narrow enough not to eat the
  scrollbar — so it is now *derived* and asked for per point.
  `preview_view::bar_grip(scale, band)` = half a bar's thickness
  (`BAR_W_LOGICAL * GRIP_SHARE`), capped by the platform's band, so widening the
  band can never grow into the bars; `Handler::grip_at(x, y, band)` (default:
  the whole band) lets the app hand the strip back exactly where a scrollbar's
  interactive zone covers the point, and both the Win32 `WM_NCHITTEST` and the
  X11 `BUTTON_PRESS` edge tests consult it. The edges the app draws nothing on
  therefore keep the platform's full band: measured on the live window, the
  left/bottom/top edges claim 8 px instead of 4, and the right edge claims 8 px
  whenever the pane shows no scrollbar (a document that fits) versus 4 px over
  the bar — verified by per-column `WM_NCHITTEST` sweeps for a short and a long
  document, by a drag on the bar's columns changing the rendered content, and in
  a headless test across all three modes (which fails under the old uniform
  band, so the coverage is real).
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
- **`--cfg editor_check_linux` works from Windows** (the symmetric of the
  existing Win32-from-Linux check): the Win32 module is gated on
  `any(target_os = "windows", editor_check_win)` instead of excluding
  `editor_check_linux`, and both cfgs are declared in `Cargo.toml`. The X11
  backend type-checked for the first time under that flag (a stray brace and a
  duplicated `intern` signature were blocking it).
- **Samples are long enough to scroll** (`samples/sample_markdown.md`, `sample_text.txt`,
  `sample_jsonl.jsonl`, `sample_json.json`): the old samples fit on one screen, which is
  why no scrollbar ever appeared in a window or a `--render` snapshot.

### M8 round (Oct 2026) — Linux X11 backend lands (type-checked), 229 tests green
- **`src/platform/linux.rs`** (~1250 lines, full Xlib FFI, zero crates):
  borderless window (`_MOTIF_WM_HINTS` decorations off) with the app-drawn
  title bar, **manual move** (title-bar `Drag` region) and **manual edge
  resize** (6px bands, min 200×100), full event translation — keys with
  Ctrl/Shift/Alt chords + `XLookupString` text, mouse, wheel/tilt (buttons
  4–7 → `Event::Scroll`), `ConfigureNotify` → `Resized`, `WM_DELETE_WINDOW`
  → `CloseRequested` — canvas blit through an Xlib-made `XImage`
  (ZPixmap, depth 24, 4 bytes/px: `XCreateImage` once, `sync_image` restating
  the buffer pointer and row stride before each blit, and never
  `XDestroyImage`d because the pixels belong to the canvas),
  **clipboard** through the CLIPBOARD/PRIMARY selections (we own the
  selection; `SelectionRequest` answered with UTF8_STRING/STRING/TARGETS,
  reads spin the queue with a deferred-event replay), **file dialogs** via
  `zenity`/`kdialog`, `xdg-open` for links, `_NET_WM_ICON` from the portable
  icon renderer, `_NET_WM_STATE` toggle for maximize, `XIconifyWindow`,
  dark preference via `gsettings`, centered on the primary screen, 40 ms
  poll-loop `Tick` (XPending + sleep), arrow/xterm/hand2 cursors.
- **`editor_check_linux`** dev cfg: type-checks the X11 backend from
  Windows/macOS (`RUSTFLAGS="--cfg editor_check_linux" cargo check`) —
  the mirror of `editor_check_win`. `platform/mod.rs` + `install.rs` cfg
  gating updated so exactly one backend is `use`d per configuration;
  `system_prefers_dark` now routes to the Linux backend on Linux.
- **Not yet on Linux**: XDND drag-and-drop (the one parity gap, tracked for
  the M8 follow-up), XIM/UTF-8 dead-key compose input (`XLookupString`
  covers Latin-1 today), DPI scale is fixed at 1.0.
- Runtime verification needs a real X server (X11/XWayland) — CI
  (`ci.yml`) compiles + runs the headless suite on ubuntu-latest from the
  first push, and the `smoke (linux window)` job starts the release binary
  under Xvfb and asks that server for a mapped window named `Editor`
  (`tools/linux-window-smoke.sh`, which `release.yml` runs on the binary it is
  about to publish too). That probe is not ceremony: the first time this
  backend was ever started it died at the first blit with a bare SIGSEGV,
  because the hand-built `XImage` header above left `f.put_image` null and
  `XPutImage` calls through it. A human pass is still what the checklists
  above mean by "eyeballed".

### Cross-OS verification round (Oct 2026) — the macOS CI job would have failed on push

The M8 work was re-checked against what CI actually runs by *simulating each
foreign backend's values* on Windows (temporary env-var toggles, since removed)
rather than waiting for a runner. Three defects, all fixed:
- **The macOS release zip unpacked wrong**: `ditto -c -k --sequesterRsrc
target/Editor.app out.zip` without `--keepParent` archives the bundle's
*contents*, so the download extracts a stray `Contents/` directory instead of
`Editor.app` — "drag it to /Applications" could not work. `--keepParent` is
what puts the bundle itself at the archive root.
- **`editor_check_macos` could not be run from Linux**: the macOS arm of
`system_prefers_dark`/`map_file` was keyed on `target_os` alone, which is false
on Linux, so a Linux host selecting the AppKit backend fell through to a
`linux::` the configuration never compiled — the one host combination the check
exists for. Every entry point all three backends implement (`map_file`, `run`,
`edge_grip`) is now reached through the `backend` alias so the selection cannot
drift from the module cfgs, and the one dispatch that still needs cfgs
(`system_prefers_dark`, until macOS has a stub to alias) repeats that module's
predicate verbatim.
- **A new edge-grip test only held on Windows/Linux**:
`resize_grip_self_tunes_to_what_is_at_the_edge` asserted the bar-safe strip is
*strictly* narrower than the platform band, which is unsatisfiable on a backend
reporting `0.0` — the macOS stub — so `cargo test` on macos-latest fails. It now
asserts `<=` unconditionally, the strict form only when a band exists, and pins
the strip to `preview_view::bar_grip` at the band it was handed.

**Verified with no Linux or macOS machine**: `EDITOR_TEST_BAND=0` (macOS
band), `=6` (X11), `=12`, and font mapping forced off (macOS, pre-M9) each run
the full suite green — 234 tests, where `=0` was 233 passed / 1 failed before the
test fix. With mapping off, 15 headless renders (four samples + a
CJK/emoji/symbols document × three size/scale combinations) are byte-identical
to the mapped renders, so the macOS read-through path draws the same pixels.
`cargo test`, `cargo clippy --all-targets` (with and without `mem-census`),
`cargo fmt --check`, and `cargo check` under all three `editor_check_*` cfgs
are clean; the Linux tarball + `install.sh` and the Windows zip were reproduced
exactly as the workflow builds them (the installer lays down binary, `.desktop`
and MIME package with no source tree present).
- **Still not verifiable here**: no Linux host, so the X11 *link* and the native
suite get their first real run on CI — which is why both workflows install
`libx11-dev` (the runner image ships the runtime library but not the `libX11.so`
linker symlink, and every link step would die without it). `ditto` itself was
never executed, so the flag behaviour above is from its documentation and the
macOS zip wants one manual unzip.

### Drag-release round (Oct 2026) — a scrollbar click can no longer stay grabbed, 236 tests green
User: "clicking on the scroll bar in any view, the click gets stuck on the
scrollbar and takes a click elsewhere to free it." A drag lives from its press
to its `MouseUp` and nothing else ends it, so the only way to feel that is a
*release the app never receives* — the thumb then follows the pointer with no
button held, until some later click happens to end it.
- **Windows (the reported platform) never captured the mouse.** A borderless
  window gets no implicit capture, so a `WM_LBUTTONUP` for a press that left the
  client area — or whose capture was taken away by a modal dialog of ours, or by
  another window — is delivered to whatever is under the pointer and the app
  never hears the release. `WM_LBUTTONDOWN` now takes `SetCapture`; the
  `WM_LBUTTONUP` arm clears the held-button record *before* `ReleaseCapture`,
  because the `WM_CAPTURECHANGED` that follows re-enters the window proc and
  must not fulfil the same gesture twice.
- **The release that cannot arrive is synthesized** (`end_gesture`, called from
  `WM_CAPTURECHANGED` and `WM_KILLFOCUS`): the app's gestures end only on
  `MouseUp`, so the platform owes it one whenever capture changes hands or the
  window is deactivated mid-drag (Alt+Tab). The same arm drops a stranded touch
  contact, which otherwise held the promotion guard and swallowed every mouse
  message for the rest of the session.
- **X11 gets the same reconciliation.** X hands us the release implicitly while
  a button is down, but focus leaving takes the grab with it (the WM's own
  Alt+drag, another client's popup), so `FOCUS_OUT` drops a manual move/resize
  drag and synthesizes the release for a held left button
  (`FOCUS_CHANGE_MASK` added to the selected events).
- **The app-side floor**: `on_mouse_down` starts every gesture from a clean
  slate (`drag_anchor`, `preview_drag`, `sb_drag`), so a drag whose release was
  lost is bounded by one click rather than waiting on a release that is never
  coming. Regression test `a_stranded_scroll_drag_ends_on_the_next_press` (it
  fails with the reset reverted — checked), plus
  `a_lost_release_is_delivered_exactly_once` in the Win32 backend for the
  double-delivery hazard, since one Alt+Tab fires both messages.
- 236 tests green; `cargo clippy --all-targets -- -D warnings`, `cargo fmt
  --check` and `cargo check` under all three `editor_check_*` cfgs clean.
- **Not verified here**: no GUI automation, so the capture fix is argued from
  the Win32 capture rules and the event traces rather than from a reproduced
  click. The window wants one manual pass: press a bar, drag the pointer out of
  the window, release, then move — the bar must not follow.
- Architectural debt this touches, not created by it: a nested message pumped
  while one of our own modal dialogs is open runs the app's handler re-entrantly
  (`&mut App` while the outer call still holds one). `WM_PAINT` already does
  this on every click that opens the file dialog; the synthesized release is one
  more instance. A modal-depth guard in `State` is the way out, and belongs to a
  round that also covers `paint`.

### Window-restore round (Oct 2026) — the session remembers size, place and maximized, 246 tests green
User: "it also needs to remember what size and location the app was when closed
to persist next session like maximized or what not." The session file already
survived restarts (theme, mode, tabs); the window did not.
- **Only the normal rect is remembered.** A maximized window reports the work
  area, so storing whatever the window says while maximized would hand back a
  screen-sized window the moment it was un-maximized. The app keeps the last
  geometry the platform reported with `maximized: false` (`App::window`) plus
  the live flag, and persists the pair; the platform reopens at the normal rect
  and *then* maximizes.
- **The platform reports, the app remembers.** `Event::WindowGeometry` carries
  x, y, w, h and the flag, sent on every OS-driven change: Win32 `WM_MOVE` +
  `WM_SIZE` (`GetWindowRect` + `IsZoomed`, so Aero Snap and a double-clicked
  title bar count), X11 `ConfigureNotify` + `_NET_WM_STATE`. Neither backend
  keeps geometry state of its own — the app is the single owner, which is why
  the rule is testable headlessly.
- **Restore is a request, not a command.** `Handler::initial_window` (default
  `None`) is asked once before the window exists; `platform::open_at` uses the
  saved rect only when `title_reachable` says the screens we have now can still
  show it — a rect saved on a monitor that is no longer attached would otherwise
  open off-screen with no way to drag it back. Win32 measures against the
  virtual screen, X11 against the root window; the rule itself is shared, so the
  two backends cannot drift.
- **Every way out persists** (`App::quit`: the window's ×, Ctrl+Q, and the OS's
  close request). A move or resize deliberately does *not*: a drag is a stream
  of them and only the last one matters.
- Settings format grows `window <x> <y> <w> <h>` (negative origins are normal —
  a monitor left of the primary one) and `maximized <0|1>`, with a partial
  window line dropped rather than applied (a half-written rect would open a
  window at the origin with no size).
- **Verified end to end on the real window**, not just headlessly: a PowerShell
  harness drove `editor.exe` with `APPDATA` pointed at a temp dir — moved it to
  `200,150 900x700` and closed → the session file held exactly that; moved to
  `240,180 880x660`, maximized and closed → the file kept that rect with
  `maximized 1`; the next launch came up maximized and `SW_RESTORE` landed back
  at `240,180 880x660`. A session hand-edited to `window -5000 -5000` opened
  centered (`410,200 1100x800`) instead. Clicking the title bar's × with a
  synthetic client click quit the app and wrote the new rect.
- 246 tests (10 new, all headless: the normal-rect rule, the restore request,
  quitting, the settings round trip, and the placement guard); `cargo clippy
  --all-targets -- -D warnings`, `cargo fmt --check` and `cargo check` under all
  three `editor_check_*` cfgs clean.
- **Not verified**: the X11 path is type-checked only (no X server here) — it
  shares the placement rule and mirrors the reporting, but the `_NET_WM_STATE`
  restore wants one run on a real desktop. macOS has no window yet (M9), so its
  stub takes the `initial_window` default and remembers nothing.

### Crash-safe session round (Oct 2026) — a killed window still reopens where it was, 250 tests green
- **The gap**: the session was written on the way out, which only covers a clean exit. A
  process killed from Task Manager, a crash or a power cut never reaches `App::quit`, so the
  next launch reopened at the rect of the *last clean exit* — the window would jump back to
  where an older run was left, however the current one had been moved.
- **The write also happens from the heartbeat** (`App::on_tick`), once a reported geometry has
  held still for `GEOMETRY_SAVE_TICKS` (25 × 40 ms ticks ≈ 1 s), ahead of the drag auto-scroll
  branches because those return early. Cost: one small file write per *completed* move, resize
  or maximize — not one per frame.
- **Only a change restarts the wait.** A report equal to what the app already holds neither
  marks the geometry dirty nor resets the countdown, so a platform that echoes the same rect
  cannot starve the write, and a drag — every intermediate rect — collapses to the one the
  gesture ended on.
- **Maximizing is queued on its own**: a maximized report keeps the normal rect and only flips
  the flag, so the pair written is the one the next session needs. A first report that merely
  confirms the state already on disk is not a change, so a session reopened maximized does not
  rewrite itself on launch.
- **Trade-off, documented rather than hidden**: a kill inside the ~1 s window keeps the previous
  rect. The heartbeat writes the geometry only — tabs and theme keep their write-at-change
  points.
- **Verified end to end on the real window** (PowerShell harness, `APPDATA` pointed at a temp
  dir, `Stop-Process -Force` so no clean exit is possible): moved to `260,170 900x640`, killed
  → the session file held exactly that, and the relaunch opened at exactly that rect; moved to
  `320,210 880x660`, maximized, killed → `maximized 1` with the *normal* rect kept (not the
  `1936×1168` work area the maximized window reports), relaunch came up zoomed and
  `SW_RESTORE` landed back on `320,210 880x660`; killed 1.3 s after a move → the new rect was
  already on disk; killed 0.3 s after the next move → the previous rect (the debounce is a wait,
  not a write per report); 4 s idle → the file's mtime and contents unchanged (the timer does
  not churn the disk).
- 250 tests (4 new, headless: the pending write, the same-rect re-report, the drag, the
  maximize); `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` and `cargo check`
  under all three `editor_check_*` cfgs clean.
- **Not verified**: the X11 path is type-checked only (no X server here) — it is the same
  app-side code, reached through `ConfigureNotify` → `Event::WindowGeometry` → the same `Tick`.
  macOS still has no window (M9).

### Atomic-session round (Oct 2026) — the session file is replaced, never rewritten, 253 tests green
- **The gap the heartbeat created**: an in-place `fs::write` truncates and refills the file, so a
  power cut or a kill landing inside one of those ~1 s writes left a *truncated* `session.txt` —
  and the next launch came up with no session at all (theme, tabs and window together), which is
  the exact failure the heartbeat was added to prevent.
- **`settings::save_in`** now stages the text in `session.tmp` **in the same directory**, flushes
  it (`File::sync_all`), and only then replaces `session.txt` with a rename: one commit step, so a
  reader or a crash sees either the whole new session or the whole previous one. Same-directory
  staging is what keeps the rename a same-volume move.
- **`load` reads only the real file**, so a temp file a crash left behind is ignored — the last
  *committed* session is what the next launch sees — and the next save deletes it.
- **Fallback**: when the target refuses to be replaced (a locked file, an unusual filesystem, a
  second instance winning the race for the single temp name) the write happens directly — late,
  complete, non-atomic, which is strictly better than losing the session. Every failure is
  swallowed: persisting must never take the app down.
- `load_in`/`save_in` take the directory as an argument (`load`/`save` are the thin wrappers over
  `config_dir()`), which is also what lets the tests run in their own temp dirs.
- 3 new tests: a save replaces the session in one step and leaves no scratch file behind; a write
  interrupted before the rename keeps the previous session (and the leftover is cleaned up); a
  session path that cannot be replaced does not take the app down.
- **Verified on the real window** (`APPDATA` in a temp dir, `Stop-Process -Force`): after two moves
  the session file's **file index changed** (`00200000-0012754C` → `00110000-0012754F`, read with
  `GetFileInformationByHandle`) — a new file object moved into place, not the old one rewritten —
  the config dir held only `session.txt` after each write, and the killed/relaunched round still
  came back at `310,205 870x615`.
  - Incidental finding: NTFS *tunneling* preserves a recreated file's creation timestamp
    (`CreationTimeUtc` came back identical across the two writes), so a creation-time check proves
    nothing here — the file index does.
- 253 tests; `cargo clippy --all-targets -- -D warnings` and `rustfmt --check` on
  `src/settings.rs`/`src/app.rs` clean; `editor_check_win`/`_linux` clean. At the time of
  writing, crate-wide `cargo fmt --check` and `editor_check_macos` fail on another agent's
  in-flight `src/platform/macos.rs` (dialogs in progress there: `url_path`/`screen_visible`
  missing, and that backend has no `run`/`edge_grip`/`map_file` yet) — nothing this round touched.

### Responsive-reading-column round (Oct 2026) — the page reflows with the window, 258 tests green
- **The complaint**: maximize the window and the text stayed in the same narrow strip with two
  thirds of the screen empty beside it — "it should be like a dynamic webpage and auto adjust
  based on window size". It was a fixed measure: `Theme::content_max_width = 820`, applied by
  `.min(...)` in the three wrapped panes, so 1920 px of window still drew an 820 px column.
- **`layout::column(viewport_w, ctx) -> (col_w, x0)`** is now the single place that decides the
  reading column, replacing the same five lines that had been copied into `markdown::layout`,
  `json_tree::tree` and `text::plain_params`. The width is `COLUMN_SHARE` (0.82) of what the
  margins leave, floored at `COLUMN_MIN` (820 — an ordinary window is unchanged) and capped at
  the theme's `content_max_width`, which becomes a *ceiling* (1500) instead of the whole policy.
  Floor and cap are logical, so DPI and `Ctrl +/-` zoom scale the column with the text in it, and
  it can never exceed the space the margins leave — a 280 px window gets a 216 px column, not one
  that runs under the window edge.
- **Long code lines fold instead of running out of the panel.** `Block::Code` drew each source
  line as one text op with no wrap, so a long command was painted straight past the panel and off
  the pane edge — the end of the line could not be read at all. Rows now fold through
  `text::wrap_ranges` at the panel's inner width, with folded continuations indented so they
  cannot be mistaken for new commands (and the panel grows with them).
- **Measured** (`editor --render`, same file, 1920×1168): code panel 550..1369 with text
  overflowing to 1778 **before** → panel 215..1704 with no ink past it **after**; at 1100 px the
  column is 838 (the old measure, i.e. small windows are untouched); at 2560 it stops at the
  1500 px ceiling.
- **Verified on the real window**: maximized `editor.exe` captured with `PrintWindow`
  (1936×1168): text ink 218..1717, i.e. a 1500 px column centered with 218 px margins on both
  sides — 1.83× the old measure — and nothing drawn outside it.
- 5 new tests (the column grows and stops at the ceiling; stays centered and inside a narrow
  window; scales with zoom; an 800-char code line folds into rows that fit the panel; the app's
  preview column grows when the window does). A negative control — the cap back at 820 — fails
  the two growth tests and only those (the theme file was restored byte-identical afterwards).
- 258 tests; `cargo clippy --all-targets -- -D warnings`, `rustfmt --check` on every touched file
  and `editor_check_win`/`_linux` clean (`editor_check_macos` still fails on another agent's
  in-flight `platform/macos.rs` dialogs, as before).
- **Note for the next round**: 1500 px is a readability ceiling, not a limit on the idea — a user
  who wants the text to fill an ultrawide screen needs a setting (or `Ctrl+scroll` adjusting the
  measure) rather than another constant.

### Touch-scroll round (Oct 2026) — a finger drag pans again, 259 tests green
- **The report**: "touch scroll isn't working" on a touchscreen, and a finger drag selected text /
  moved the caret instead of scrolling.
- **The cause, read off the wire**: `TOUCHEVENTF_DOWN` and `TOUCHEVENTF_MOVE` were declared the
  wrong way round (`0x1` and `0x2`). Win32's values are MOVE = `0x1`, DOWN = `0x2`, UP = `0x4` —
  the opposite of the order the names are read in — and a real digitizer sends `0x1A`
  (PRIMARY|INRANGE|DOWN) on contact, `0x19` per move, `0x14` on lift-off. So every *down* was
  handled as a *move* (the pan branch, the only one that dispatches `Event::Scroll`, never ran —
  no scrolling at all) and every *move* was handled as a *down*, which reset `touch_travel` on
  every step. Lift-off then saw `travel == 0` and synthesized a click where the finger came up:
  the caret jump / selection the user saw.
- **Fixed** through `touch_action(flags) -> TouchAction` (Down/Move/Up/Ignore), a pure function with
  the values named correctly, plus two details a finger drag needs: the pan follows the `PRIMARY`
  contact when a batch marks one (a second finger cannot make the view jump between contacts,
  while a digitizer that marks none still pans), and a repeated `Down` cannot restart a live
  gesture. The existing guard — a live contact owns the pointer, so the mouse messages Windows
  promotes from touch are swallowed — stays as it was, and is why a drag no longer selects.
- **Reproduced and verified on the real digitizer**, not in a simulation: `tools/touch_probe.ps1`
  injects *genuine* touch input (`InitializeTouchInjection` + `InjectTouchInput`), so the driver,
  the message loop and `WM_TOUCH` are the ones the user hits, and captures the window with
  `PrintWindow` before and after the drag. Before the fix a 250 px finger drag changed **0
  pixels** and left a click behind; after it the document scrolled **exactly 250 px** — the before
  image shifted by 250 matches the after image with a residual of **0 changed pixels**.
  - Field notes for whoever runs it next are in the probe's header: `pointerInfo.frameId` must stay
    0 and `rcContactRaw` must be left alone (either one makes the first `InjectTouchInput` fail
    with ERROR_INVALID_PARAMETER 87, which reads like a bad struct layout), `POINTER_FLAG_UP` is
    `0x40000` (`0x80000` is WHEEL), and the window must be on screen under the injected point —
    touch is delivered by hit test, not to the focused window alone.
- The diagnosis came from temporary logging in the window proc (message id, extra info, touch
  state, and each contact's flags plus screen/client coords) — since removed, but it is the reason
  this round could name the exact bug instead of guessing.
- New test `touch_contacts_decode_to_the_phase_they_are` pins the measured flags (`0x1A` → Down,
  `0x19` → Move, `0x14` → Up) *and* the Win32 values themselves; putting the swapped constants
  back fails exactly that one test and nothing else (windows.rs was restored byte-identical
  afterwards). 259 tests, `clippy --all-targets -- -D warnings`, `rustfmt --check` and
  `editor_check_win`/`_linux` clean.
- **Not verified**: pen input (this machine has a pen digitizer; pen contacts are treated as
  touch), and the Linux backend still has no touch handling at all — it reads wheel buttons 4–7
  only, so touch scrolling on X11 remains an open gap.

### M9 round (Oct 2026) — macOS AppKit backend lands (type-checked), 269 tests green
- **`src/platform/macos.rs`** (~1350 lines, objc-runtime + AppKit/CoreGraphics FFI, zero
  crates): borderless `NSWindow` subclass (`canBecomeKeyWindow` overridden — borderless
  windows refuse key status by default, which would starve the view of `keyDown:`) with the
  app-drawn title bar, **manual move** (title-bar `Drag` region) and **manual edge resize**
  (6 px bands, min 200×100, `Handler::grip_at` consulted like Win32/X11), an `NSView`
  subclass (`objc_allocateClassPair` + `class_addMethod`) for `drawRect:`/`keyDown:`/mouse/
  `scrollWheel:`/drag-drop. The view is **flipped**, so the canvas blit
  (`CGDataProviderCreateWithData` + `CGImageCreate` + `CGContextDrawImage`; BGRA
  little-endian is exactly the canvas's `0xAARRGGBB` `u32`s) lands upright. `objc_msgSend`
  is declared once and transmuted per call signature (`objc_msgSend_stret` on x86_64 for
  `NSRect` returns, plain `objc_msgSend` on arm64). **Retina**: canvas = points ×
  `backingScaleFactor`, mouse points flipped (AppKit is bottom-left) and scaled to canvas
  px. Keys via `keyCode` (kVK table) + `characters` text, with **Command mapped onto the
  app's Ctrl chords**. `NSPasteboard` clipboard, `NSOpenPanel`/`NSSavePanel` dialogs,
  Finder drag-drop (`registerForDraggedTypes:` + `NSFilenamesPboardType`), `NSTimer`
  40 ms `Tick`, `NSCursor` arrow/IBeam/hand, dock icon from the portable icon renderer
  (`CGImageCreate` → `NSImage initWithCGImage:size:`), dark preference via `defaults
  read -g AppleInterfaceStyle`, `open` for links, `mmap(MAP_PRIVATE)` font mapping
  (libSystem). Session geometry: opens at `Handler::initial_window` through the shared
  `open_at` reachability rule, reports `Event::WindowGeometry` on every move/resize/
  maximize (bottom-left points → top-left physical px), and a maximized session
  re-maximizes after opening at the normal rect.
- **`editor_check_macos`** dev cfg: the module/install/Cargo plumbing landed with the
  memory round; this round added the missing `not(editor_check_macos)` arms in
  `install.rs` and the `print_*` dispatches, and collapsed `system_prefers_dark` to
  `backend::system_prefers_dark()` now that all three backends have it.
  `RUSTFLAGS="--cfg editor_check_macos" cargo check` type-checks the AppKit backend from
  Windows/macOS; `cargo check --all-targets` under it compiles the backend's own tests.
- **Not yet on macOS** (runtime pass pending, like M8): IME/`NSTextInputClient`
  (`characters` covers Latin-1 + composed keys), live theme-change notification (no KVO on
  the appearance — `Ctrl T` still flips manually), the scroll direction/multipliers want a
  real wheel/trackpad pass (line deltas ×48×scale, precise deltas ×scale, positive = up
  per AppKit's natural-scrolling-inclusive contract), there is no `NSMenu` at all (the
  app's own shortcuts work; `Cmd+Q` does nothing), and `application:openFile:` is the
  legacy singular (Finder/dock opens work; the modern `openFiles:` +
  `replyToOpenOrPrint:` path is a follow-up).
- 4 backend tests (kVK mapping, Command→ctrl, edge bits, manual-resize frame math) run on
  macOS CI; 269 tests green, `clippy --all-targets` and `rustfmt --check` clean, and
  `editor_check_win`/`_linux`/`_macos` all check clean.

### Scroll-smoothness round (Oct 2026) — a wheel notch glides instead of jumping, 269 tests green
- **The report**: scrolling "seems a little choppy or lagging just a little". Measured first, and the
  frame was never the problem: a full repaint of `sample_markdown.md` at 1100×800 costs ~1 ms in release
  (`cargo test --release -- --ignored --nocapture paint_cost`), so the eye had time to spare — the
  *cadence* was wrong. A wheel notch was applied as one 48 px jump, and the only clock the app had
  was the 25 Hz / 40 ms heartbeat, so nothing could ever happen in between.
- **Fixed** by giving scrolling two ways in (`ui/scroll.rs`): `scroll_by` moves *immediately* — a
  finger, a scrollbar thumb, drag auto-scroll, anything the pointer drives has to track it exactly,
  and a glide in the way of a finger reads as lag — while `scroll_smooth` sets a `target` the view
  *glides* to. The ease is exponential and measured in **time** (`GLIDE_TAU_MS = 55`), not frames, so
  the same glide feels the same on a ~120 Hz X11 poll loop, a ~50 Hz Win32 timer, or a host that
  only has the 40 ms heartbeat. `GLIDE_MAX_PX_PER_MS` keeps a fling from teleporting and the step
  (`d * (1 - e^-dt/τ)`) can never overshoot; notches accumulate into one glide, so spinning the
  wheel accelerates toward the total instead of restarting a glide per notch.
- **The clock the glide needed did not exist.** `Host::set_animating` + `Event::Frame` are new, and
  each backend supplies frames its own way: Win32 adds a second `WM_TIMER` id (1 ms asked for, so
  the OS coalesces to its ~15.6 ms tick) that runs *only* while something is animating and is killed
  the moment it stops, and the X11 loop — already awake every 1 ms — emits a frame every 8 ms
  (~120 Hz) while the app asks. A host with neither is not broken: `on_tick` advances the glide too.
- **Direct manipulation is its own event** (`Event::Pan`): a touch or pen drag follows the input
  exactly and *abandons* a glide in flight — its remaining distance belongs to the wheel, not the
  finger — and the frame clock stops with it. Windows sends it from the touch path (its only
  producer so far); the wheel path and the touch path now share `pan_horizontal`.
- **Verified on the running app, not in a simulation.** `tools/scroll_probe.ps1` sends a *real*
  wheel notch (`SendInput` / `MOUSEEVENTF_WHEEL`) to a release build and captures the window
  repeatedly while it moves, reporting the vertical shift between captures — the sequence the eye
  actually sees.
  - Before (a build with the glide thresholded out, so every notch lands at once): 3 notches,
    **one position, a single 144 px step**, in 3 of 3 runs.
  - After: 3 notches, the same **144 px total**, arriving as 9–11 captured positions over ~280 ms.
    (The capture rate, ~25–40 Hz through `PrintWindow`, undercounts the app's ~20 frames.) Every
    match was exact — cost **0**, the pixel grids differing by nothing but the shift.
  - The app's own trace (temporary, since removed) shows the same run in detail: three notches
    coalesced by Win32 into two messages, target 48 → 144, frames every ~22 ms, offset 76 → 102 →
    117 → 126 → 132 → … → 144.0, ~90% of the distance by ~165 ms, and the frame clock stopping with
    the last frame landing exactly on 144.0.
  - Field notes for whoever runs it next are in the probe's header: bring the window *truly* to the
    foreground before sending (`GetForegroundWindow`), or the notches go nowhere and the probe
    reports "no motion" for a scroll that never happened; and a row-luminance signature is
    *periodic with the line height*, so a shift of exactly one line matches text against text and
    beats the truth whenever the real shift is not a whole number of lines — with the first,
    repetitive probe document it read an 18 px hop for a 144 px jump. Match on subsampled pixels,
    and keep the body non-periodic.
- **Tests**: `ui::scroll` pins that a coarse step glides (monotone, exact landing, no overshoot, the
  visible part over in ≤200 ms), that a huge delta stays under the speed cap, that spinning the
  wheel builds one glide, that a fine delta is applied at once, that direct manipulation takes over
  from a glide, that a glide stops at the end of the document, and that *the frame rate does not
  change the feel* (one 40 ms step and five 8 ms steps cover the same distance, equal to f32
  rounding). `app` pins the wheel → frames → landing contract including `set_animating(true)` and
  `(false)`, and that a finger pan drops a glide. `platform::windows` pins that the two `WM_TIMER`
  ids stay told apart. 269 tests, `clippy --all-targets -- -D warnings` and `rustfmt --check` clean,
  `editor_check_win` / `editor_check_linux` 0.
- **Not verified / limits**: the macOS backend implements no frame clock (the `set_animating`
  default), so its glide would advance on the 40 ms heartbeat alone — its file was another agent's
  in-flight work this round and was left untouched; the X11 frame clock is type-checked
  (`editor_check_linux`) but cannot be run from this machine; and the glide's tail is deliberate
  (the last ~2 px arrive at 0.05–0.2 px per frame — invisible, but they keep the clock alive for
  ~100 ms). The *first* frame is the largest step, because exponential easing starts at full speed.

### M10 slice (Oct 2026) — recent files, 284 tests green
- **The session keeps a recent-files list** (`settings.rs`): `recent <path>` lines beside the
  `open` ones, capped at `RECENT_MAX = 10`, newest first, independent of the tabs a session
  reopens — a file stays recent long after it stopped being open. `note_recent` (a free
  function, so `App` and the tests share the exact rule) moves a path to the front and drops
  the duplicate.
- **The right-click menu lists them** (`ui/menu.rs`): a "Recent" caption (painted, muted,
  never answers a click) followed by up to 6 file names — menu labels became owned `String`s
  for it. `Action::Recent(i)` opens the i-th entry, which `open_path` then bumps to the front.
- **Wiring** (`app.rs`, `cli.rs`): `App` keeps the list, notes every `open_path` and every
  save-as (a brand-new file is as recent as an opened one), persists it with the session, and
  `cli` restores it at startup — a file given on the command line is noted too, because
  `App::new` opens it without going through `open_path`.
- Tests: the settings round trip carries the list, `note_recent` moves/dedupes/caps, and an
  app-level test opens two files, checks the menu rows, and reopens one through
  `Action::Recent`. 284 tests, `clippy --all-targets` and `rustfmt --check` clean.
- **Oversized files are refused, not frozen on** (`app.rs`): everything Editor does —
  buffer, layout, render — is whole-document, so a huge file is a memory and latency trap.
  `Tab::from_path` checks the size before reading and reports "file is N MB — larger than the
  64 MB open limit" in the status bar instead (`size_error`, split out so the rule is testable
  without a 64 MB fixture). Windowing/streaming a huge file stays out of scope for now.
- Find (`Ctrl+F`) closed M10 — see the next section.

### M10 find (Oct 2026) — Ctrl+F, 324 tests green
- **Find bar** (`ui/find.rs`, new): `Find` holds the query, the match list, and the current
  index; `search` is a case-insensitive, line-oriented substring scan whose haystack chars
  are tagged with the byte offset of the char they were folded from, so columns stay on
  char boundaries even when case folding expands (İ → "i" + combining dot). Matches never
  span a newline and never overlap.
- **The bar** (`ui/find.rs::paint`): a small strip docked at the viewport's top-right —
  "Find", the query (mono, with a blinking caret), and an `n/m` count ("0 matches" when the
  query hits nothing). `bar_rect` is pure, so painting and hit-testing cannot drift. Clicks
  on the bar are swallowed; Escape and Ctrl+F own closing it.
- **Wiring** (`app.rs`): Ctrl+F opens (and closes the full-window overlays); while open the
  bar owns the plain keys — typing goes to the query via `Event::Text`, Backspace edits it,
  Enter/F3 step (Shift = back, wrapping), Tab/Delete are consumed, Ctrl chords stay app
  chords. `relayout` re-runs the search whenever the content, tab, or view changed.
- **Showing matches**: Edit/Split tints every match over the editor pane (accent at low
  alpha, through the `selection_rects` path, only paying for rows that can be on screen)
  and makes the current one the editor selection, with `follow_caret` scrolling to it.
  Preview has no buffer-offset geometry, so it gets the honest count plus a best-effort
  jump: the view scrolls to the preview row containing that occurrence of the query.
- **Concurrent round — Split scroll lock**: the two Split panes share a 0..1 scroll
  *ratio* instead of one pixel offset, so the short pane can never sail past its own end
  (phantom gutter rows, pinned thumb); each pane paints and hit-tests its own share, and
  dragging either thumb drives both. The gutter stops drawing numbers past the last line.
- **Concurrent round — dialog filters**: the open/save dialogs on all three backends build
  their filters from `core::file_kind` (`globs()` / `SAVE_GROUPS`), so what the dialogs
  offer can never drift from what the app opens and the installer registers; pinned by
  test.
- **Concurrent round — horizontal wheel glide**: tilt-wheel steps on the editor pane now
  glide over frames toward an `h_target` (same easing, same frame-clock discipline as the
  vertical ride), fine deltas still land at once, and pointer/finger input takes the view
  over and ends the glide. `scroll::glide_step` is the shared easing.
- **Concurrent round — procedural app icon**: the taskbar/`CreateIconIndirect` mark is no
  longer a font glyph — it is drawn as a slate document page with a folded corner and one
  brand-blue ruled line on a light gradient tile (signed-distance anti-aliasing), so it
  reads as a page at 16 px and separates from dark and light taskbars alike.
- Tests: `ui::find` covers the scan (case, multibyte, folding, non-overlap), refresh
  landing at/after the caret, wrap in both directions, and the bar's geometry; app-level
  tests cover open/type/count/select, Enter/F3 wrap, Backspace editing the query live,
  zero-match reporting, case-insensitivity, and the Preview count+jump. 324 tests,
  `clippy --all-targets` and `rustfmt --check` clean.
- M10 is feature-complete; what remains are the runtime passes (real-window test, Linux
  X11, macOS) tracked above.

### M10 find round 2 (Oct 2026) — touch + responsiveness, 329 tests green
- **The bar grew a × close button** (`ui/find.rs`): a 32 px zone flush with the bar's right
  edge (the bar itself is taller now), Hand cursor, closes on press. `close_rect` is pure
  like `bar_rect`, so painting and hit-testing agree.
- **Typing no longer rescans the buffer** (`ui/find.rs`): every match of the longer query
  starts where a match of the shorter one did, so appending chars *narrows* the previous
  match list (`Find::refresh`, O(matches)) instead of rescanning (`Find::rescan`,
  O(buffer)). Backspace and content/tab changes still rescan — a shorter query's matches
  are a superset, and a stale list must never be trusted. The scan itself reuses one
  scratch buffer across lines instead of allocating per line.
- **Match tints are one pass** (`layout/text.rs::match_rects`): the editor pane never
  wraps, so each match is a single horizontal band and only the matched characters are
  measured — the paint cost follows the match count, not the document size (it was one
  whole-buffer `selection_rects` call per visible match).
- **A Find button on the status bar** (`ui/statusbar.rs`, `app.rs`): leftmost, next to
  Open, toggling the bar for touch users; the bar's × and Escape still close it.
- **Touch targets everywhere**: status-bar buttons hit-test at the full bar height (the
  pill still paints inset), the tab close/save zones are 20 px and the tab's full height,
  and the find bar's × is 32 px wide.
- **Double-tap drag-select** (`app.rs`): a double-tap selects the word (it always did —
  taps promote to clicks); a finger that pans within ~1 s of the tap now *extends the
  selection* instead of scrolling, riding a virtual pointer from the tap spot (a moving
  finger reports `Pan`, never `MouseMove`). `PanEnd` retires the gesture with no coast —
  a selection drag is not a flick. A later drag scrolls as before.
- **The status bar reads left to right** (`ui/statusbar.rs`, `app.rs`): the document's
  own facts (`Markdown · 186 lines · 100%`) at the far left in every mode, then the
  contextual detail (caret in Edit, a parse error, "reloaded"), then the accent-tinted
  Copy button — full bar height, only while a selection exists — and last the address
  of the link under the pointer. The bar describes what is under the eye, it is not a
  log: clicking a link no longer writes its address there, hovering does.
- **Copy acknowledges itself where it was pressed** (`app.rs`): the label becomes
  "Copied" until the selection changes (any press, key, typed text or finger drag),
  then back to "Copy"; with nothing selected the button is gone. No second message in
  another corner of the bar.
- **Preview clicks speak the editor's language** (`app.rs`, `ui/preview_view.rs`): one
  click clears, two select the word under the pointer (`PreviewView::word_at` measures
  the exact span from the op's advances, falling back to the row on white space), three
  select the row. A press that turns into a drag still selects the rows it crosses, so
  drag-select is unchanged; on touch the double-tap arms the row anchor and the pan
  after it extends the selection, retired by `PanEnd` with no coast.
- Tests: narrowing equals the full search (append, append, backspace, edit-then-rescan),
  the × and Find button toggle the bar, double-tap-drag extends without scrolling or
  coasting, the Copy button sits after the data and says "Copied", preview clicks clear/word/row,
  a finger drag after a preview double-tap selects rows. 368 tests,
  `clippy --all-targets` and `rustfmt --check` clean.
- **Editor geometry is O(line), not O(buffer)** (`layout/text.rs`): the editor pane never
  wraps, so `hit_test` jumps straight to the clicked line, `caret_point` to the caret's
  line, and `selection_rects` visits only the selected lines — a keystroke, arrow, or
  click on a huge file no longer walks the whole document with a per-line allocation.
  The preview's find jump reuses one lowercase scratch buffer across rows for the same
  reason. 330 tests, `clippy --all-targets` and `rustfmt --check` clean.

### M10 find round 3 (Oct 2026) — real-window probe + the land fix, 331 tests green

- **A runtime probe for the find bar** (`tools/find_probe.ps1`): launches a release build
  on a generated document and drives it through the same window proc the OS uses
  (PostMessage `WM_CHAR`/`WM_KEYDOWN`/`WM_LBUTTONDOWN` — SendInput needs a foreground
  window, and this desktop has none while locked), verifying each step by pixel-diffing
  PrintWindow captures. The status-bar buttons are located by pixel analysis (the bar's
  text clusters; the block is right-aligned in a known order), not hardcoded x. Twelve
  checks: window renders, buttons found, Edit switch, bar appears, tints drawn, count
  updates, narrowing clears tints and drops the count, backspace restores, bar closes,
  strip returns to its pre-bar look, Enter advances the match. All 12 pass.
- **The probe caught a real bug**: `Find::land` compared match *starts* against the
  caret, but `find_jump` leaves the caret at the current match's *end* — so every
  keystroke extended the query and skipped the selection ahead one match (line 5 → 15 →
  25 → 35 while typing "alpha"), and once that landed below the fold `follow_caret`
  scrolled the view mid-word. `land` now prefers the match *containing* the caret and
  falls back to the first match at or after it, so typing keeps the selection put and
  the view still. `tools/find_dump_diff.py` (kept) analyses probe dumps offline.
- Tests: the typing sequence keeps `current` on the first match with the view at offset
  0; `land` keeps the match under the caret, wraps past the last. 331 tests,
  `clippy --all-targets` and `rustfmt --check` clean; all three platform cfgs compile.

### Rename round (Oct 2026) — Readitor → Editor, 331 tests green

- The name was taken, so the app is now plainly **Editor** — everywhere the name
  appears, not just the chrome: crate/binary (`editor.exe`), window title
  (`file — Editor`), About, help legend, context menu, CLI usage/version/error
  strings, window class (`EditorWindow`), macOS class names and bundle id
  (`dev.editor.app`), X11 XDND atom (`EDITOR_SEL`), Windows ProgID
  (`Editor.Document`) and its registry strings, config dirs
  (`%APPDATA%\Editor`, `~/.config/editor`, `~/Library/Application Support/Editor`),
  the `editor_check_{win,linux,macos}` cfg flags (CI updated to match), packaging
  (`Editor.app`, `editor.desktop`, `editor-fonts.bin`), release artifact names,
  README/PLAN/docs, and the probe tools' usage lines. Deliberately untouched: the
  repo directory and git remote (renaming the checkout breaks the working
  environment), the `tools/chat_*.py` room name (agent-coordination protocol), and
  the Oct 12 recovery scripts (they record real historical paths). 331 tests,
  `clippy --all-targets` and `rustfmt --check` clean; all three cfgs compile under
  the new `editor_check_*` names.

## Review notes (Oct 2026 hygiene + gap pass)
- `PLAN.md` (root) and `docs/PLAN.md` used to be byte-identical, kept in step by hand; the
  heuristic used only the names. The rule now: `docs/PLAN.md` is canonical, the packaging README
  and release workflow point at it, and a duplicate root copy should not come back.
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
### Installer round (Oct 2026) — a Desktop shortcut, on by default
- **Windows**: the one-page wizard grew a third checkbox under a `Shortcuts`
  heading. `DesktopShortcutFeature` (Level 1, so a silent `msiexec /passive`
  install from the updater gets it too) owns a `DesktopFolder` component whose
  KeyPath is an HKCU value — the same per-user trick the Start menu entry uses,
  since a per-user component's key path cannot be a machine-context shortcut.
  Verified by hand on the built MSI: a default silent install puts `Editor.lnk`
  on the Desktop, removing just that feature deletes the shortcut and nothing
  else (Start menu entry, exe and associations stay), and an uninstall takes
  both shortcuts, the install folder and the ARP row with it.
- **Linux**: `install.sh` writes the same `.desktop` to the Desktop as well as
  the applications menu, honouring `XDG_DESKTOP_DIR` (whose value is usually a
  literal `$HOME`, which sed cannot expand), marking it executable and — where
  `gio` exists — trusted, because GNOME will not launch an untrusted one.
  `EDITOR_NO_DESKTOP_SHORTCUT=1` skips it and nothing else; the CI packaging job
  now asserts the default, the XDG directory and the opt-out.
- Wording: each checkbox now only names a group ("Documents and config files")
  under a heading that states the action once, the extension lists name every
  claimed type, and the shortcut descriptions are ASCII — Windows Installer
  writes those through the shell, where the em dash they carried came out as a
  hyphen anyway (read back from both `.lnk` files after a real install).
- **Measured, not eyeballed** (this page has clipped a list before): the built
  MSI's dialog was captured by its own window rect and the ink bands compared
  with the control boxes. Fifteen names double-spaced cleared the documents
  list's right edge by 5px, so the lists now use single spaces — the same line
  clears it by 37px, and no two text rows overlap.

### Update-transport round (Oct 2026) — no curl, no leftovers, 379 tests green
- **Fetching** (`src/net.rs`, new): the updater's three requests no longer assume
  `curl` exists. Each is handed to the first of the OS's own routes that works —
  Windows: `curl.exe` (System32 since 10 1803) → PowerShell's
  `Invoke-WebRequest` → `urlmon!URLDownloadToFileW` (one FFI call, WinINet does
  the TLS, no child process at all) → `certutil -urlcache`; macOS: `curl` →
  `python3`; other Unix: `curl` → GNU/busybox `wget` → `python3`. Still no
  crates and no TLS of our own; a rung that is missing, or that security
  software refuses to start, reports why and the next one runs, and the
  all-failed message lists every rung instead of one opaque failure. URLs are
  gated first (`trusted`): HTTPS on GitHub's release hosts, or plain HTTP to
  loopback (which is how the tests drive the real helper commands against a
  server they start). Helpers get argv, never a command line to split —
  PowerShell is the one exception, and its single quotes are doubled.
- **Footprint**: everything the updater stages is one directory,
  `<temp>/editor-update-<tag>`, stable across versions by name. The next launch
  deletes older ones (2-minute age guard so a concurrent `--update` is never
  raced, and a locked directory simply retries later), Linux/macOS delete theirs
  before relaunching, and a failed download deletes what it wrote.
- **Restart after an update** (Windows): the running app used to be closed and
  left closed. Measured, in the verbose log of a silent install over a running
  Editor: Restart Manager shut the window down and never reopened it, so the
  update dialog's "close and restart" was only true where `src/update.rs`
  relaunches by hand (Linux/macOS). The first attempt put the relaunch *inside*
  the package (`RestartEditor`, deferred `asyncNoWait` before `InstallFinalize`)
  and had to be taken back out: on a major upgrade the execute sequence runs
  twice, so two Editors started, the second hung on the first one's
  single-instance mutex, and the process a custom action leaves behind in the
  transaction can still hold the exe — which made Windows Installer schedule a
  restart (on the test machine, an actual reboot). The relaunch now lives in
  `src/update.rs` only, as one `cmd` line — `msiexec /i … /passive
  REBOOT=ReallySuppress & start "" <installed exe>` — so the new window opens
  after the install exits, exactly once, argument-less into the saved session
  (last file, window place), and no update can ever reboot the machine. The
  package launches nothing, so a wizard install or a `/qn` deploy stays quiet.
- **Visible when it happens**: two Windows fixes for what the user kept
  reporting as "no update prompt" and "it doesn't come back". The prompt's
  message box is now *owned* by the main window (and waits up to 2 s for that
  window to exist — the check runs on its own thread and can reach the dialog
  before `CreateWindowExW` has finished), because an ownerless box mixes with
  the app's own window in z-order and can open behind it. And the app asks for
  the foreground when it appears: `SetForegroundWindow`, or `FlashWindowEx` on
  the taskbar button when Windows refuses (which it does for anything started
  by a process that was not itself in front — an installer's restart action is
  exactly that case, so the relaunched window used to appear silently *behind*
  whatever the user was looking at). The same rule now applies on the
  single-instance handoff path, where a second launch asks the *existing*
  window forward instead of creating one.
- **Check now** (About box, F2): the automatic check is silent and bounded to
  twice a day, so a user who wants to know *now* had only the command line — and
  a silent check is indistinguishable from a broken one. A button under the
  auto-update checkbox runs the same flow off the clock and off that setting
  (`update::check_now`, its own thread, so the window never blocks on the
  network) and always answers: **in the box**, on a status line under the
  button. Only the "install this version?" question is still a dialog — a
  message box for "you are up to date" played the Windows alert sound for a
  non-event (user report), so `update::Check` (`Idle`/`Checking`/`UpToDate`/
  `Available`/`Failed`) travels back through a shared `Mutex` and the app
  repaints when the 40 ms tick sees it change. The panel reserves the line so
  it does not resize when an answer arrives, and a long failure is trimmed to
  the panel instead of widening it. Pinned by
  `about_box_has_a_check_now_button_under_the_toggle`, `the_check_result_is_
  painted_under_the_button` (idle band blank, result band inked, long text
  trimmed with an ellipsis) and the geometry tests.
- **Uninstall**: `--uninstall` (portable, and the MSI's uninstall custom action)
  drops the associations *and* every staged payload plus the sidecar a failed
  Linux swap can leave. Settings/session/crash reports are kept, and the command
  says where they are.
- **Measured, on this machine**: the published 0.2.1 msi (1.6 MB) fetched and
  checksum-verified through the new code path, ending in a real `msiexec`
  install; a *fresh* 0.2.1 install timed 0.9 s against 13.4 s before
  `MSIFASTINSTALL=1` (12.4 s of it inside `SRSetRestorePoint`); a rebuilt 0.2.3
  msi's uninstall removed the fake `%TEMP%\editor-update-v9.9.9` payload, the
  install folder, the HKCU ProgID and the ARP row; `editor.exe --uninstall`
  removed a staged payload and printed where the settings stayed; and a launch
  swept a 3-hour-old payload while keeping a 36-second-old one.
- **Windows downloads spawn nothing any more** (Oct 2026, user report: Defender
  noise that started with the updater). The chain used to be `curl.exe` →
  `powershell -ExecutionPolicy Bypass -Command Invoke-WebRequest` → `urlmon` →
  `certutil -urlcache`. The last three are the problem, not the fix: a text
  editor running a bypassed script host to fetch JSON is precisely the shape
  endpoint protection interrupts, and `certutil -urlcache` is a malware
  favourite this machine refuses to start at all (`ERROR_ACCESS_DENIED` at
  process creation). The order is now `urlmon` (in-process WinINet via one FFI
  call, the OS's own TLS, no child process) → `curl.exe`, and PowerShell and
  certutil are gone from the code, not merely demoted. Verified live on this
  machine, not by inspection: `cargo test --ignored net::tests::live` fetched
  `https://api.github.com/` through **both** rungs and the body parsed as JSON
  (WinINet's default User-Agent is accepted; the assertion rejects an HTML
  error page that a status-blind rung would otherwise report as success).
  `the_chain_carries_the_url_and_never_runs_a_script_host` fails the build if a
  script host comes back.
- **The log named it.** `Microsoft-Windows-Windows Defender/Operational`,
  event 1116/1117: 31 detections, **27 of them on 2026-10-17** (during this
  work), every one `Trojan:Win32/Ceprolad.A` with a
  `CmdLine:_…certutil.exe -urlcache -split -f http://127.0.0.1:<port>/asset
  %TEMP%\editor_fetch_rung_test` path — that is `net::tests::`
  `every_available_rung_works_on_its_own` spawning the certutil rung, twice
  (detect + remediate) per `cargo test`. The rest are the same command typed by
  hand while building the chain. **Zero events name `editor.exe`**: the app was
  never the thing being flagged, the test harness was. Proof rather than
  assertion: the newest 1116 before the fix was 04:00:33Z; a full `cargo test`
  run after removing the rung left the count at 31 (08:06Z).
- **Not verified**: the macOS/Linux rungs beyond `curl` (`wget`, `busybox
  wget`, `python3`) are type-checked, not executed on this OS.

### Status-bar centring (Oct 2026) — a sign error, measured in pixels
- Reported from the running app: the status-bar labels sat low in the bar, with
  more air above than below, and the selected pill made it obvious.
- Cause: `Metrics::descent` is negative by convention (it grows *downward*, the
  same sign as a y axis), but `Metrics::height()` computed `ascent + descent`, so
  every centred row was told its line box was `descent` shorter than it is and
  pushed the baseline down by |descent|/2 — 1.3 px at 1x, 2.6 px at 2x. Three
  copies of the same `(h - ascent - descent) / 2 + ascent` arithmetic lived in
  `layout::{markdown,csv,json_tree}`, and the status bar had a `-1px` “optical”
  fudge that was cancelling part of the error instead of fixing it.
- Fix: `height()` is now `ascent - descent` (documented in place), the three
  duplicated call sites use it, and the status bar has one `mid` line from which
  the labels, the active pill and the Copy button are all measured — no bias.
  The pill also shrank to the text box plus 8 px and took a 6 px corner, so the
  highlight and the word inside it are the same shape.
- Pinned by pixels, not by eye: `the_button_label_is_centred_on_the_bar_not_sunk_into_it`
  and `the_active_pill_is_symmetric_about_the_same_centre_line` rasterize the
  bar at 1x and 2x and require the ink above and below to differ by at most 1 px.
  Both fail on the pre-fix code (13 px above / 10 below at 1x).

### Open pipeline (Oct 2026) — four things that were odd, found on request
- **The handoff happened too late.** `cli::run` built the whole `App` (parsing
  the file, laying it out) and only then asked `platform::forward_to_running`
  whether another window owned the launch. A big file was loaded to be thrown
  away, and — worse — a file that failed to open exited with an error *instead
  of* being handed to the window that was already running, which is an "Open
  with Editor" that appears to do nothing. The forward is now the first thing
  after argument parsing, skipped for `--render`/`--pretty`/`--mem-report`,
  which never open a window.
- **One file, two spellings, two tabs.** `open_path` compared `PathBuf`s for
  exact equality, so `C:\x\a.md` and `C:\x\sub\..\a.md` opened twice: two
  buffers over one file, two dirty flags, last save wins. It now falls back to
  `fs::canonicalize` on both sides (`App::same_file`), pinned by
  `the_same_file_spelled_differently_reuses_its_tab`.
- **Arguments were being dropped.** A second positional argument silently
  replaced the first, there was no `--` terminator (so a file called
  `-draft.md` could not be opened at all), and the parsing lived inside `run`,
  where nothing could test it. `parse()` is now separate and returns a list of
  files; `editor a.md b.json` opens both as tabs, in order, and the handoff
  carries the list (newline-separated — a Win32 path cannot contain a newline,
  so the receiver splits unambiguously) so a second launch with three files
  opens three tabs in the running window. Four tests in `cli::tests` pin the
  rules, including that `--` makes a later `--help` a *file*.
- **Startup did everything twice.** `restore_recent` and `note_recent` were each
  called twice in `cli::run`; the second pair is gone, along with a duplicate
  `restore_recent` that rewrote the list after `restore_window`. `--render` and
  `--pretty` now insist on exactly one FILE instead of rendering an Untitled
  buffer or ignoring the extra names.

### Host-capability gating (Oct 2026) — the last `allow(dead_code)` is gone, 390 tests green

The event enum carried an `#[allow(dead_code)]` because a build sees one
backend, and the variants the other backends construct looked dead. It was
load-bearing, not stale: Linux never constructs `Pan`/`PanEnd`/`FileOpen`/
`ThemeChanged`, and macOS never constructs `Pan`/`PanEnd`/`Frame`/
`ThemeChanged` either. Suppressing that made the vocabulary a lie.

Now the vocabulary tells the truth. A variant exists only where a host can
produce it (`cfg(any(target_os = …, test))`), the app's exhaustive match
carries the handler under the same gate, and the touch-selection state that
only pointer touch can arm — `TouchSel`, `PreviewTouch`, the two `arm_*`
helpers, the pan handlers, the release coast — follows it out of the Linux and
macOS builds. `test` is in every gate, so every path is still compiled and
driven on all three hosts. Nothing is suppressed; a backend that grows a
capability opts in at the enum, the match arm, and nothing else.

Verified by linting the real targets instead of a cfg simulation:
`cargo clippy --target x86_64-unknown-linux-gnu` and
`--target x86_64-apple-darwin`, both `--all-targets -- -D warnings`, are
clean (they need no linker, which is why they work from a Windows checkout).
The `editor_check_*` cross-builds cannot verify this: they leave `target_os`
alone, so on a Windows host the Linux build still lacks `unix` (and vice
versa) — they were already failing that way before this round.

Three release blockers fell out of the same `-D warnings` run: a `mut` binding
in `net::rungs` that only Windows uses, the deprecated `io::Error::new` form
in the MSI stub, and a null-returning `platform::main_window` stub that
nothing called once the gating above was honest about who owns the window.

### Repo audit (Oct 2026) — a release can no longer ship a version it isn't, 390 tests green

Prompted by a real bite: a locally built *test* MSI had been installed with a
fabricated version (0.3.1) above the shipping line (0.2.2). Windows Installer
treated every later release as a downgrade, refused it with "a newer version
of Editor is already installed", and the updater's `msiexec … & start …`
discarded that exit code — so the old build relaunched and the update looked
like it had simply done nothing.

- `release.yml` now fails before building anything unless the tag and the
  package version agree, on every OS. The MSI is stamped from the tag and the
  binary reports `Cargo.toml`; drift between them is invisible until an
  install outranks the line it came from.
- `update::parent_dir` replaces two `file.parent().unwrap()` calls in the
  Linux and macOS install paths: an update reports a malformed staging path
  instead of aborting the process.
- `windows.rs`'s doc comment no longer lists comdlg32 dialogs, `WM_DROPFILES`
  and `AttachConsole` as TODO(M6) — all three are implemented in that file.

Inventory of what the audit turned up but did not change, because each needs a
decision rather than a tidy-up: `--install` registers the icon and open command
against the *running* exe (a build tree, if that is where it was run from), so
`cargo clean` breaks those associations; `build.rs` embeds the icon and version
resources only for MSVC, so a GNU local build is iconless by construction; the
`editor_check_*` cross-builds leave `target_os` alone and cannot lint the other
backends (they were failing that way already); `tools/recover_*.py` and
`tools/restore_vscode_history.py` still hardcode `C:\github\readitor`.

### Preview-polish + syntax-highlighting round (Oct 2026) — 425 tests green

A presentation pass over every viewer, no new dependencies and no new runtime
work per frame. The rendering stays a display list over the system fonts; the
new work is parser and layout quality, plus a real code highlighter.

- **Markdown rendering** (`layout/markdown.rs`): task items draw real `☑`/`☐`
  boxes (the procedural symbol family) instead of the source's `[x]`; list
  items replace their trailing block gap with a small inter-item gap, so a list
  reads as one group rather than one paragraph per bullet; tables band alternate
  body rows; a fenced block with an info string gets a small language label over
  a hairline rule; image placeholders are framed cards naming the alt text and
  source (truncated to the card) instead of one bare line in a half-empty box;
  a quote's bar now ends at the last block's real gap.
- **Markdown parsing**: a UTF-8 BOM no longer hides the first heading or fence;
  text and link destinations decode the common HTML character references
  (`&amp;`, `&mdash;`, `&#65;`, `&#x2014;`); `<user@example.com>` becomes a
  `mailto:` link; inline HTML detection is restricted to an actual tag shape
  (`<not@an>` stays text); table cells keep non-pipe backslash escapes for the
  inline parser (`\*\*` is literal, not bold); frontmatter is stripped only when
  the block opens like a mapping, so a document that starts with a rule and has
  another one within 50 lines no longer loses everything between them.
- **JSON tree** (`layout/json_tree.rs`): fields and items carry the commas JSON
  has; containers use conventional `▼` open / `▶` collapsed expanders and a
  `{…}  N items` summary; segments are placed by their measured prefix width,
  so a wide CJK or emoji key no longer shifts the following `: ` and value. The
  JSONL virtual plan now measures the record's head row on error records and
  subtracts the body indent from the banner width — two places where the plan
  disagreed with a full layout (one line per error record, plus wrapped banner
  text), which showed up as scroll drift in long JSONL files. A parity test
  pins `Plan.height` and a full-band layout against `layout_records`.
- **CSV** (`layout/csv.rs`): a table narrower than the reading column grows into
  it (extra width shared by natural column width); numeric columns are detected
  in one scan and right-aligned; body rows alternate a faint tint.
- **Syntax highlighting** (`layout/syntax.rs`, new): the ad-hoc scanner became a
  line-oriented highlighter with a tiny cross-line state. Language hints and
  extensions map onto Rust, Python, shell, Ruby, C-like/Go/PHP, JS/TS, SQL,
  Lua, JSON, HTML/XML, CSS, TOML/YAML/INI or a generic fallback, each with its own comment
  forms, quote forms, keywords and types. It handles `//`/`#`/`--`/`;` line
  comments, `/* */` (nested for Rust), HTML comments, Lua `--[[ ]]`, Python
  triple quotes and Rust raw strings, JS template literals, Rust lifetimes vs
  char literals, preprocessor directives, decorators/attributes, shell
  `$var`/`${…}`/`$(…)`, SQL's case-insensitive keywords, HTML tags/attributes,
  CSS properties/custom properties/hex colors, and JSON keys vs string values.
  Function names are accented, types/literals use the literal color, and the
  state is threaded through both the code-file pane and a markdown code panel so
  a block comment or docstring that crosses lines keeps its color. A wrapped
  line is tinted once and sliced across rows rather than re-tinted per row.
- **Validation**: `cargo test` 424 passed; `cargo clippy --all-targets --
  -D warnings`, `cargo fmt --check`, and `cargo build` clean. Release cost gate
  (`PERF_REPORT_ONLY=1 cargo test --release -- --ignored perf_code
  perf_markdown`): code load 377 ms / first frame 509 ms / frame 1.96 ms, and
  markdown 36.8 ms / 375 ms / 3.14 ms — all well inside budget and unchanged in
  order from the pre-highlighter build. README screenshots were regenerated
  from the app (`tools/readme_shots.py`), so the pictures match the renderer.
### Virtualized JSON tree round (Oct 2026) — a million-node document is a plan, not a display list, 451 tests green

The round before this one taught a paint to skip the chunks of a display list
that are off screen (the band index; on its own it took a 3.8 M-op tree's frame
from 17.0 ms to 0.95 ms) and gave a big JSONL file a per-record plan. A single
JSON document was still laid out whole: parse, build every op, build the row
index, then paint one screenful of it. It now gets the treatment JSONL has.

- **A per-node plan** (`layout/json_tree.rs`, `ValuePlan`/`NodeSlot`): one
  24-byte slot per node in pre-order, each holding the y of the node's first
  row, the height of its whole block, its parent, its next sibling, its index
  within the parent and the toggle id a full layout would give it. Painting
  binary-searches the slot under the scroll offset, walks the ancestor chain
  down to it (which is how a container with a million children above the
  viewport is skipped in one step), and lays out only the blocks that overlap
  the viewport — every row placed at the y the plan decided, nothing
  accumulated from the top of the document.
- **The plan is shape counting, not a second layout.** A row that fits its
  column is counted (`cells × advance`, the same test `emit_row`'s fitting-row
  fast path makes) and never written out; only a row that overflows pays for
  being built and wrapped. A row holding a glyph the grid cannot size is
  measured for real through the same `wrap_ranges` the painter calls, so a plan
  cannot disagree with the paint it drives — an approximation there is scroll
  drift, which is exactly what a document full of wide glyphs used to produce.
- **Which documents take it**: over `VIRTUAL_JSON_NODES` (20 000) nodes, a
  count and not a line count — a minified 5 MB document is one line and a
  million nodes. Below it the pane lays out whole, as it always has, which is
  what keeps every existing layout test and the everyday file on one path.
- **The app side** (`app.rs`): the parsed tree is kept on the tab beside the
  buffer revision it came from, so a resize, zoom, pane switch or toggle
  re-plans without re-parsing, and a stale tree can never outlive an edit, a
  streamed seal or a reload. A big JSON rides the existing worker job
  (`PreviewLayout::Json`), and `paint_panes` bands it like a JSONL pane — in
  Preview, Split and against the live `collapsed` set, whose ids the plan
  assigns exactly as a full walk does (collapsing a node therefore paints the
  summary row the full layout would, at the y the plan resized everything below
  it to).
- **Fixed on the way**: the JSONL records cache is keyed on the buffer revision
  too. It was only ever invalidated by a worker pass, so a big JSONL file
  reloaded from disk could keep painting the records of the previous revision.
- **Numbers**, 8 MB pretty-printed JSON (558 702 nodes), 1440×900, release,
  same machine: first frame 1205 → 266 ms of which 192 ms is the parse this
  design cannot avoid; the pane holds 139 ops instead of 2 492 669; live heap
  299 → 98 MB; a scroll frame 0.36 ms; the plan itself 61 ms, and a band 0.1 ms.
  Perf gate at its then-2 MB (single-threaded A/B on one build, the threshold
  raised
  to disable the plan for the "before" run): first frame 407.9 → 90.6 ms, live
  83.3 → 33.0 MB, frame 0.55 → 0.34 ms. JSON's first-frame budget in the gate
  comes down from 4000 ms to 600 ms with it — 5-6x the measured path, which is
  a wall-clock guard for CI rather than a precise one: the *structural* half of
  this guarantee is a test that pins a 21k-node tree's pane
  under a thousand ops, and that holds on a runner of any speed.
- **Tests**: `the_virtual_tree_plan_matches_a_full_layout` (heights and the ink
  of a whole-document band against `layout_value`, at four widths);
  `a_tree_band_paints_exactly_the_rows_it_covers` (a scroll sweep, claiming
  nothing off the full layout and missing nothing the viewport covers);
  `a_collapsed_node_moves_the_tree_the_way_a_full_layout_does` (every node in a
  small document: height, rows, and that the toggle's hit rect did not move);
  `a_tree_below_the_threshold_lays_out_whole`; and in `app.rs`,
  `a_big_json_paints_a_band_and_toggles_inside_it` (bounded ops, the band
  follows the offset to the right rows, a click collapses inside it).
- **Validation**: `cargo test` 451 passed (debug and release), `cargo clippy
  --all-targets` and `cargo fmt --check` clean, release cost gate green for all
  six kinds.

## Banded panes for every reader kind

- **The gap**: plan-and-band was a JSONL/JSON feature, not a property of the
  pipeline. At 8 MB — an eighth of the open limit — a code pane held 1 607 100
  ops, 181.6 MB live and a 1125 ms first frame; a log 326 556 ops and 390 ms; a
  Markdown document 787 846 ops and 2579 ms. The cost gate was fixed at 2 MB
  precisely because those kinds could not go bigger, which is how the gap
  survived a green build.
- **One index for every kind** (`layout/plan.rs`, new): `Rows` keeps one `f32`
  per unit — the y a unit starts at — plus the document height, and answers
  which units a `[top, bottom)` band covers by binary search. The units are a
  line for text, code, logs and configs; a top-level block for Markdown. The
  painters stay kind-specific; the addressing rule is shared, so a new reader
  kind plugs in by building a plan.
- **The line family** (`layout/highlight.rs`, `layout/text.rs`): plain text,
  code, logs, YAML, TOML and INI are one family — `Kind::Plain` is the no-tint
  member, so they share one plan and one band painter. Rows are counted with
  the same fit test `layout` makes (cell arithmetic, `wrap_ranges` only for a
  line that overflows), so the plan costs a pass over the text and keeps no
  ops. Code tint is cross-line, so the plan carries `syntax::State` checkpoints
  every 256 lines; a band that starts inside a block comment scans from the
  nearest checkpoint and keeps the comment colour. `emit_line` is now the one
  emitter for both the full layout and a band, so the two cannot disagree.
- **Markdown** (`layout/markdown.rs`): a block's height is whatever its spans
  wrap to, so the plan measures each top-level block once at y=0 and drops its
  ops. Blocks are independent given their start gap, so a band re-renders only
  the blocks it covers, at the y the plan carries. This buys memory (787 846
  ops → 141) and frame cost, not a cheaper first measure: Markdown's plan is
  still one full pass, and at 8 MB that is 1.7 s on a worker thread while the
  plain band paints.
- **App** (`app.rs`): `PanePlan` — `Text`, `Lines`, `Markdown` — sits on the tab
  beside the buffer revision it was built from, and `PreviewLayout::Planned`
  carries it out of `preview_layout`. The threshold is `VIRTUAL_UNITS` (20 000
  lines or blocks), the same order as the worker threshold, so a small file
  keeps its full list *and* its row index. Line panes are planned inline (the
  plan is cheap, so they never wait on a worker or show plain text first);
  Markdown plans on the worker like JSON. `paint_panes` has one banded branch
  that dispatches to the plan, re-plans on a width change from the buffer or
  the kept document, and refuses to paint a plan from an earlier revision.
- **Universality is a test**: `every_file_kind_plans_or_is_bounded_by_its_parser`
  walks every `FileKind` and fails unless it maps into a planned family. CSV is
  the one exception, and the test proves it is bounded by the parser's row cap
  (17 011 ops at 8 MB, no matter the file size); if that cap is lifted the test
  is the reminder that the table needs a plan.
- **Tests**: per family, a whole-document band equals the full layout op for op
  (`text`, `highlight`, `markdown`), a mid-document band claims nothing and
  misses nothing visible, and a band 600 lines into a comment keeps its colour;
  in `app.rs`, planned panes for plain text, logs and Markdown (bounded ops, the
  band follows the offset to the right rows), the worker path a big Markdown
  document takes (plain band while the plan is measured, the plan and its parsed
  document installed on landing, then bands), the sealed 60k-line code stream
  now planned instead of worker-laid-out, and a small pane keeping its list and
  row index.
- **Numbers** (8 MB, release, 1440x900): code 1125 → 16.6 ms first frame,
  181.6 → 38.8 MB live, 1 607 100 → 219 ops; log 390 → 18.2 ms, 77.2 → 29.3 MB,
  326 556 → 109 ops; Markdown 2579 → 1706 ms (the measure pass), 106.2 → 88.4
  MB, 787 846 → 141 ops. The gate now runs at 8 MB for all six kinds and adds a
  `pane ops` budget (2 000 for every planned kind, 40 000 for the capped CSV),
  so an op per line can no longer pass as a green build.
- **Validation**: `cargo test` 467 passed, `cargo clippy --all-targets -- -D
  warnings` and `cargo fmt --check` clean, release cost gate green for all six
  kinds at 8 MB.

## Plans patch themselves on edit

- **The gap**: a plan made opening cheap, not editing. Every keystroke in Split
  set `layout_dirty`, and the relayout re-counted every line's wrapped rows for
  the reader pane. At 8 MB (134 546 lines) a keystroke cost 183 ms, and only
  8 ms of that was the plan: the editor half walked every op and measured each
  line's text for `h_max` — a glyph shaping call per line, 100 ms a keystroke,
  to answer "how wide is the longest line".
- **The buffer logs its edits** (`core/buffer.rs`): every mutation records an
  `EditSpan { start, removed, inserted }` in the coordinates of the text *after*
  it, capped at 64 spans (a streamed load coalesces its thousands of appends
  into one). `take_edits` hands them out and clears the log; an overflow, a
  cleared log or a plan of the wrong width makes the caller rebuild instead.
- **Rebasing happens at the edit, not at the paint** (`app.rs`,
  `patch_plan_for_edit`): a span names lines in the text that edit produced, so
  it is replayed while that text is current. The next paint finds the plan's
  revision current and only installs it (editor half, height, scroll) —
  `relayout` no longer has a path that re-measures a current plan. Multiple
  keystrokes before a frame each patch one line, not one document.
- **`Rows::splice`** (`layout/plan.rs`): replace a unit range with freshly
  measured heights and shift the tops below by the difference. The index keeps
  only tops plus a tail pad, so the old span's height is priced from the tops
  themselves.
- **Line panes** (`layout/text.rs`, `layout/highlight.rs`): `patch_lines` and
  `LinePlan::patch` count the inserted lines with the *same* function a fresh
  plan uses, so a patched index cannot drift. A code edit also drops the tint
  checkpoints from the edit onward; a band there rescans from the text instead
  of painting the old comment colour.
- **Markdown** (`layout/markdown.rs`): blocks are the parser's output, so an
  edit re-parses; the plan then diffs the block lists and measures only the
  changed run between the common prefix and suffix, keeping the rest of the
  heights and re-seating the gap after the run. Skipping the measure is the
  win: at 8 MB the measure is 967 ms of the 1.4 s relayout.
- **Universality**: `PanePlan` is the extension point and the patch match in
  `app.rs` is exhaustive over it, so a new planned family must answer there.
  Markdown's answer is the parse-bound one; a JSONL's records are patched the
  round after this one (see "JSONL records patch themselves on edit"); JSON's
  own tree had no per-unit plan to patch (a minified document is one line) until
  "A JSON tree patches itself on edit", which indexes it by source byte instead;
  and CSV stays capped. The rule is a test: the cost gate types one character into
  every planned pane and fails unless the plan was *patched* — a correct
  rebuild would pass every pixel test.
- **Numbers** (8 MB, release, 1440x900, Split): a code keystroke 183 → 74 ms
  (plan 8.2 ms → the one edited line; the 100 ms `h_max` walk → one widest-line
  scan and a single measure; the remaining cost is the editor half's own
  31 ms whole-buffer list and the paint). Markdown's real path is unchanged in
  kind and better in degree: the edit still re-plans on the worker, the pane
  stays responsive (9 ms first frame, 7 ms per keystroke while the job runs),
  and an inline plan patches in 0.98 s instead of 1.4 s, of which 0.6 s is the
  re-parse.
- **Tests**: buffer spans (insert, delete, stream coalescing, overflow);
  `Rows::splice` against a fresh index; line-plan patch parity after growth,
  deletion and a split, with a whole-document band equal to the full layout op
  for op; a code edit's band deep below it equal to a fresh plan's, tint
  included; a deletion shifting the pane up by the rows removed; Markdown
  patch parity with a fresh plan (tops, gaps, height) and a whole-document band
  equal to the full layout; in `app.rs`, typing in Split on a 24 000-line code
  file and on a planned Markdown document patching the plan (counted) and
  landing on a fresh plan's height.
- **Validation**: `cargo test` 484 passed, `cargo clippy --all-targets -- -D
  warnings` and `cargo fmt --check` clean, release cost gate green for all six
  kinds at 8 MB with the new `type` column.
- **Limitations, stated plainly**: Markdown still re-parses per edit (0.6 s at
  8 MB inline), and for a document over the worker threshold the edit takes
  the existing worker path rather than a synchronous patch; a keystroke still
  lays out the editor half's whole display list in Split (31 ms at 8 MB); the
  plan is rebuilt whole on a resize or zoom; and JSON's own tree has no
  incremental plan because its parse is the dominant cost. JSONL's records are
  patched as of the round below; JSON's tree as of the round after that.

## JSONL records patch themselves on edit

- **The gap**: the round above left the parse-bound kinds rebuilding from their
  parse on every keystroke, and for JSONL that was the worst of both worlds — an
  edit in Split minted a revision the records did not match, so they were
  dropped and the worker re-parsed the whole file, seconds on a 33 MB log. A
  JSONL record is one physical line and the lines are independent, so the work
  one keystroke owes is one line's parse and one record's measure: the plan was
  treating a property of the format as a property of the pipeline.
- **A line range parses like the whole file** (`parse/jsonl.rs`): a blank line
  still carries no record, and a record still carries its physical line number,
  through one shared `record_of` that `parse`, `parse_parallel` and the new
  `parse_lines(buf, start, end)` all use. A test splits a document at every
  boundary and checks that concatenating the parts is exactly `parse` of the
  whole text.
- **`Plan::patch`** (`layout/json_tree.rs`): the edit spans name the lines that
  replaced others, so the records on those lines are parsed again, measured with
  `measure_record` — the same probe a fresh plan uses, so a patched height
  cannot drift from a rebuilt one — and spliced into the index, with the tops
  below shifted by the height difference and the node ids renumbered. A slot now
  also keeps the number of nodes its record assigned, which is what lets an edit
  above a record renumber it without re-measuring it. The layout rule is the
  one `build_plan` lays slots down by (a separator line between records), and a
  patch that cannot describe the buffer — a non-virtual plan, a slot/record
  mismatch, an edit past the end of the text, a splice that would empty the list
  — says so instead of half-updating an index that then paints.
- **The app stops re-parsing** (`app.rs`): a virtual JSONL's records and index
  live on the tab, and both the edit-time pass (`patch_plan_for_edit`) and the
  paint-time fallback (`patch_jsonl_plan`) patch them from the buffer's edit
  log. Freshness has one home (`jsonl_plan_active`): the width the index was
  measured at, the collapse generation, the line threshold, and the slot/record
  pair, so a resize, a fold or a shrink below the virtual threshold takes the
  full path instead of a patch. `current_plan_height` is the one place that
  decides "this plan is current, install it" for both families — line/Markdown
  panes and a JSONL's records — which is what keeps a paint from re-measuring
  anything.
- **A fold is not a patch**: collapsing a record's node changes the height of
  every record below it while the records themselves stay valid, so the
  collapsed generation is part of the freshness rule: the index is rebuilt from
  the records already in hand (no re-parse), which the band test pins by
  clicking a toggle and checking both the shorter pane and the new generation.
- **The cost gate types into a JSONL too** (`perf.rs`): a pane with a virtual
  record index now counts as planned, so the `type` column asserts that a
  keystroke *patched* it — after a bounded wait for the records to land
  off-thread, since that is where an 8 MB file's first parse goes.
- **Tests**: `parse_lines` against the whole-file parse through every split
  point; `Plan::patch` against a full rebuild — records (their lines and their
  ok/error shape), slots (ids, tops, heights) and total height — through one
  character, a split record, an inserted blank line, a pasted run, a deleted
  record, two joined records, an appended record and a deleted middle run; and
  the four refusals above. In `app.rs`, typing on a 6 000-record file in Split
  patches the index with **no worker job started** and lands slot for slot on a
  fresh build of its own records, and a click on a toggle inside the band
  rebuilds the index at the new collapse generation.
- **Numbers** (8 MB, 89 359 records, release, 1440x900): the work one keystroke
  used to repeat — `parse_parallel` 145.1 ms plus `build_plan` 370.8 ms, 515.9 ms
  of whole-document work, on the UI thread for a file under the worker
  threshold — is now one `Plan::patch` of 0.62 ms. What the gate measures end to
  end is the paint the keystroke costs in Split: the `type` column reports it
  now that a JSONL counts as planned, and it reads 44-81 ms across runs on a
  developer box — the editor half's own whole-buffer list and the `h_max` scan,
  which is where the remaining keystroke cost sits, not the records.
- **Validation**: `cargo test` 488 passed, `cargo clippy --all-targets -- -D
  warnings` and `cargo fmt --check` clean, release cost gate green for all six
  kinds at 8 MB.
- **Limitations**: JSON's tree still rebuilt from its parse (a minified
  document is one line, so a line-span patch has nothing to hang on — the next
  round localizes an edit by byte instead); a JSONL
  edited outside Split, or more times than the buffer's 64-span log holds, still
  falls back to a whole re-parse; and a resize, a fold or a width change
  re-measures every record — a pure-arithmetic probe, but still O(records).


## A JSON tree patches itself on edit

- **The gap**: JSONL's records had been patchable for a round, but a JSON
  *document* still rebuilt from its parse. An edit in Split minted a revision
  the tree did not match, so the tree was dropped and the whole document was
  parsed again and re-planned — 300-400 ms of parse and 85-200 ms of plan at
  8 MB, run to run, before a single band could paint. A JSONL is one record per line, so an edit
  can be localized by line; JSON is one value, so there are no lines to hang
  that on. There is, however, a *byte*: the one the buffer says it edited.
- **Tokens know where they are** (`parse/json/lexer.rs`): `Lexeme` now carries
  `at`/`end` byte offsets beside its line/column, and the lexer walks a byte
  cursor instead of copying the document into a `Vec<char>` first — the offsets
  are free where the columns already were, and a UTF-8 document no longer pays
  a 4-bytes-per-char copy before the parse starts. A test round-trips tokens
  through their own byte ranges.
- **`parse_spanned`** (`parse/json/parser.rs`): the parser records every node's
  source range as it enters and leaves it, so the spans come out pre-order —
  the order a `ValuePlan` assigns slots — and `spans[i]` is exactly the text of
  the node slot `i` measures. `parse` is `parse_spanned` minus the list, so the
  two cannot disagree; a test checks that every node's slice parses back to that
  same node.
- **Byte queries on the buffer** (`core/buffer.rs`): `byte_len`,
  `line_start_byte` and `byte_slice` give an edit's byte offset and cut a node's
  text back out of the document; every `EditSpan` now also records `at` and
  `delta` where the mutation happens, which is the only place they are exact.
  A test replays every logged span against the text before it and requires the
  splice to reproduce the text after it, for typing, splitting, deleting across
  lines, wide glyphs and streamed appends.
- **`ValuePlan::patch_node`** (`layout/json_tree.rs`): replace one node's slot
  block with a plan of that node as it now parses, shifting the tops and
  renumbering the node ids below by the difference. `NodeSlot::first_id` is what
  lets ids be renumbered without re-walking; a patched plan equals a rebuild
  field for field, which the parity test pins across eight replacement shapes.
  The region a keystroke re-walks is that node, not the document.
- **`Doc`** (`layout/json_tree.rs`): the tree, its spans and its plan in one
  place, with `patch` — the localization rule. A span's byte range is a
  pre-order list sorted by start, so the innermost node holding an edit is one
  binary search away, and when that node ends before the edit the only nodes
  that can still contain it are its ancestors, which the plan's parent links
  give without touching the rest of the document. A token typed at the end of a
  scalar extends the scalar (`1` → `12`), which is why typing a digit into a
  huge flat object is a one-node patch; whitespace between tokens renders
  nothing, so it shifts offsets and patches no plan at all. A node is re-parsed
  from its own bytes, spliced into the tree by child ordinal, its ranges
  spliced in beside it and every offset below it shifted. An edit that leaves
  the document unparseable (a half-typed key, `1.`) had no node to describe and
  was refused, so the caller re-parsed — the round after next turns that
  refusal into one error row (see "A broken keystroke keeps the tree").
- **The app stops re-parsing** (`app.rs`): the tab holds
  `json_doc: Option<(revision, Doc)>` in place of the tree and the plan, and
  both passes patch it — at the edit (`patch_plan_for_edit`) and at the paint
  (`patch_json_plan`). Freshness has one home (`json_doc_active`: width,
  collapse generation, virtual threshold, and spans that still match slots), and
  `current_plan_height` decides "this plan is current, just install it" for
  JSON beside the JSONL and line/Markdown families. A resize or a fold
  re-plans from the tree already in hand, never from a re-parse; a relayout that
  finds the tree stale takes the worker path exactly as before.
- **The cost gate types into a JSON too** (`perf.rs`): a virtual JSON tree
  counts as planned, so `type` asserts a keystroke *patched* it — with the
  caret placed inside a string first, because at byte zero a keystroke really
  does make the document unparseable. It also asserts no worker job was started,
  so a re-parse cannot pass as a patch.
- **Tests**: `Doc::patch` against a full parse-and-plan — tree equality, ranges
  exactly equal to `parse_spanned`'s, plan equality slot for slot, and the band
  a patched plan paints equal op for op to a full layout — through a character
  in a string, a digit extending a number, a blank line, an item appended to an
  array, a member's line deleted and a member appended at the end; the refusal
  of an edit that breaks the document, and a key rename that stays valid. In
  `app.rs`, typing in Split on a 21k-node document patches the tree with no
  worker job started, lands on a fresh build of the same tree, and paints the
  edited row.
- **Fixed on the way**: `Buffer::byte_len` summed every line, and `append_line`
  called it once per line, which made a streamed load quadratic — 8 MB JSON
  loaded in 34.4 s, a log in 5.1 s, a source file in 23.6 s. The length is a
  field now, maintained by the four mutations: 103.5 ms, 75.8 ms and 961.6 ms
  for the same files. `byte_slice` also stops walking once it is past its range.
- **Numbers** (8 MB, 545 208 nodes, release, 1440x900): `Doc::patch` for one
  keystroke is 3.6 ms, of which the node's own parse and measure is a fraction —
  the rest is finding its bytes in a 450 000-line document. What the gate
  measures end to end is the paint the keystroke costs in Split, and JSON's
  `type` column now reports 138-228 ms across runs instead of `—`: the editor
  half's own whole-buffer list and its `h_max` scan (84-144 ms, measured
  separately) is where that sits, exactly as the JSONL round found for its
  44-81 ms.
- **Validation**: `cargo test` 497 passed, `cargo clippy --all-targets -- -D
  warnings` and `cargo fmt --check` clean, release cost gate green for all six
  kinds at 8 MB.
- **Limitations**: an edit that made the document temporarily invalid (a
  half-typed key, `1.`, a `,` waiting for its value) was re-parsed whole — a
  document being typed into is invalid far more often than valid, and the next
  round makes that state patchable. A structural edit (a key, a comma, a
  delimiter) belongs to the container around it, so it re-walks that container's
  children rather than one value. A tree edited past the buffer's 64-span log,
  or edited in Edit mode and then shown in Split, still falls back to a re-parse;
  and a resize, a fold or a width change re-measures every node — a
  pure-arithmetic pass, but still O(nodes).

## A broken keystroke keeps the tree

- **The gap**: the previous round made a valid keystroke patch the tree, but a
  keystroke that left the document unparseable still dropped it and re-parsed.
  That is most of the time a document spends being typed into: `{` before its
  value, `1.` before its digits, a comma before the member it separates, `"` in
  a key that is not finished. So the fastest path was the one a typist hits
  least, and the slow one was hit per keystroke until the thought was finished —
  a worker pass and a fresh plan for every character while a member was added.
- **`Value::Broken(msg)`** (`parse/json/value.rs`): a leaf that stands for text
  the parser could not describe, carrying what it said about it. Only the patch
  path builds one: a file's first parse either succeeds or reports its error as
  before, so a tree that was never edited cannot contain one. The layout paints
  it as one row — the node's key or index, then `! <message>` in the error
  colour — through the same `scalar_segs` the painter and the full layout share,
  so a patched plan and a rebuilt one put the row in the same place.
- **The patch keeps the node it cannot parse** (`layout/json_tree.rs`): when the
  located node's own bytes fail to parse, the node becomes one `Broken` leaf
  spanning exactly that text, its slot block is replaced the same way a parsed
  value's would be, and every other node — above, beside and below — keeps its
  ranges and its measurement. The row is therefore as large as the region the
  parser lost and no larger: a half-written number breaks one row, a dangling
  comma breaks the array it dangles in. `Doc::broken` reports the first broken
  node in pre-order; it is read back from the tree after a patch only while the
  tree has an error to report, so the healthy path never walks the document.
- **A repair is a patch too**: the fixing keystroke lands inside the broken
  leaf's range, which is exactly the text that has to parse again, so the node
  is spliced back as a real value and the document is whole again — equal, tree
  and ranges, to a parse of its own text (the test checks both).
- **The pane does not demote itself** (`app.rs`): a tree the tab holds stays the
  pane's tree whatever its node count falls to, so `banded`,
  `json_doc_active` and `current_plan_height` no longer ask whether it is still
  big enough to virtualize — only a *fresh* parse decides whether a document is
  worth keeping. Without that, breaking the root of a 21k-node document dropped
  the tree below the threshold, the pane laid it out whole, and the next
  keystroke re-parsed. The status line says what the tree says: `error: <msg>`
  while some node is broken, empty once none is — including after a resize or a
  fold, which re-plan the held tree rather than reading its status off a parse.
- **The cost gate types a break too** (`perf.rs`): after its keystroke inside a
  string it puts the caret inside a nested array and types a comma, then asserts
  the tree was *patched* and no worker job was started. A re-parse cannot pass
  as a patch, and the assertion is the same one that guards the valid path.
- **Tests**: in `json_tree.rs`, a dangling comma (the array becomes one row, the
  object around it keeps its members, the band paints `! …`), a half-written
  number repaired from `1.` to `1.5` (leaf for leaf, then equal to a full parse),
  two breaks in two places with the first still reported after the other is
  fixed, a key rename that stays valid, and the one refusal left: an edit
  outside every node (trailing content past the root) has nothing to hang on.
  In `app.rs`, a comma typed at the top of a 3 600-line JSON's array patches the
  tree with no worker job, the status line says `error: …`, the band paints the
  row, and backspacing the comma restores the tree a full parse gives and clears
  the status.
- **Validation**: `cargo test` 500 passed, `cargo clippy --all-targets -- -D
  warnings` and `cargo fmt --check` clean, release cost gate green for all six
  kinds at 8 MB.
- **Limitations**: the row is as big as the node the parser lost, so a
  structural break in a big container shows that container as one error row
  until it parses again — honest, and no worse than the whole-pane banner it
  replaced, but it does not keep the container's children on screen. An edit
  outside every node (trailing content past the root) is still refused and
  re-parsed. A broken node is only re-derived by an edit inside its own range,
  which is also the only edit that can repair it. And in Split the two panes
  share one scroll offset, so a tree that collapses under a caret far down the
  document can leave the pane scrolled past its new end — the same is true of
  the error banner this replaces.

## The status line's error is a way to the node

The bar has said `error: <msg>` since an unparseable keystroke stopped costing the
tree, but that only tells a reader *that* something below is broken. The words
are now the way there.

- **The detail is a target only while it names the node** (`app.rs`):
  `status_names_broken_json` asks three cheap questions per paint — a JSON tree
  is on screen, the mode has a reader pane, and the bar's `status_extra` is the
  tree's own message (`error: <msg>`, compared without allocating). A transient
  that has taken the message's place (`reloaded`, `save failed`, `file changed
  on disk`) is a fact again: a word that does not name the node must not offer to
  lead to it. Edit has no reader pane to move, so the same words are a fact
  there too.
- **One walk, on the press**: `Doc::first_broken_slot` numbers the nodes in
  pre-order, which is the plan's slot order, so the first `Broken` leaf's
  position is its slot. Only the press pays for it.
- **The jump centres the row**: `top - (pane_h - h) / 2`, clamped to the
  document. A target at the very top of the pane reads like the start of the
  file; the middle reads like "here it is".
- **Split hands over a fraction, not pixels**: the two panes share one progress,
  and their documents need not be the same height, so a preview pixel becomes
  `target / preview_max` and `set_scroll_ratio` turns it back into the shared
  offset. Writing the pixel count straight into `scroll.offset` there moved the
  pane by that many pixels of whichever document is taller (the test's panes
  disagree by 40 %, and the old code lands at 335 244 instead of 475 008).
- **The band painter's unit was wrong** (`app.rs`): the virtualized branch built
  its band and painted it at `scroll.offset` — the shared progress in the taller
  document's units — while the scrollbar, hit tests, `preview_scroll` and the
  full-list branch all use preview pixels. Whenever the panes' heights differed
  (a folded tree, a JSON with blank lines) Split painted a different slice than
  its own scrollbar described. It reads `preview_scroll()` now, like every other
  reader-pane path. This was found by the jump: without it the pane "jumped" to
  row 2 549 when asked for 1 800.
- **The bar's outputs are one struct** (`ui/statusbar.rs`): the painter had grown
  to eight parameters filling three rects; `Targets { buttons, copy, detail }`
  says what they are — the answer to "where is the pointer?". `detail` is
  `Some` only when the detail is an action, and the hover pill it paints is
  measured from the *fitted* text, so a detail cut short of the buttons cannot
  leave a target reaching past its last glyph. The error's hand cursor comes
  from the same rect (`cursor_at`), so the pointer agrees with the press.
- **Tests**: in `statusbar.rs`, the detail is a target (bar-height rect, error
  ink, hover does not move it) only when it means something; in `app.rs`, a
  document with nothing broken offers no target at all; a comma inside a nested
  array half-way down a 3 600-record JSON (with blank lines between records, so
  the editor pane is much taller than the tree) makes the bar name the break,
  and pressing it centres the broken row — the painted band then contains the
  `! …` row — in Split and again in Preview after the pane is replanned full
  width; `reloaded` in the same slot is a fact, and Edit offers nothing.
- **Validation**: `cargo test` 502 passed, `cargo clippy --all-targets -- -D
  warnings` and `cargo fmt --check` clean, release cost gate 7/7.
- **Limitations**: the target is for a JSON tree the tab holds — a JSONL
  record's error count and a document that never parsed (no tree, the bar says
  `error line:col`) are not links. A broken node hidden inside a collapsed
  parent is not expanded for you: the jump centres the folded row that hides it.
  In Split the panes still share one progress, so the editor half moves with the
  reading half, as it does for every other scroll.

## The typing round: dirty, status counts and find ride the edit (Oct 2026) — 521 tests green
Three backlog items (POLISH.md T4, T5, T7) that all charged the document for a
keystroke even after the last round, each fixed by keeping the running answer
and pricing only what the edit touched.

- **T7 · The dirty compare starts at the edit.** `sync_dirty` used to walk from
  line 0 and stop at the first difference, so a keystroke at the end of an 8 MB
  file compared the file each time. The saved text now keeps its line count
  beside it (`tab.saved_lines`, replacing the per-compare `split('\n').count()`
  that walked every byte first), and the walk starts at the tab's `edited_from`
  watermark — the smallest line any edit since the load or save touched —
  because nothing above it differs. It is a tab field, raised by every edit and
  reset on save/reload, since the edit log is capped and `install_preview` eats
  it. The top-of-file test still compares `dirty` against a full walk and they
  agree.
- **T4 · The status bars' counts are running tallies.** The log status used to
  rescan 20 000 lines and the JSONL status walked every record after each
  keystroke. The log keeps a `LogTally` on the tab — one severity byte per line
  plus error/warn counts over exactly the window the status quotes; one
  localized edit batch patches the byte array and adjusts the counters, and
  anything else (a multi-edit batch, a tally the text outgrew, a wholesale
  replace) falls back to the full scan. JSONL keeps `records_errors` beside the
  records; the JSONL plan's patch already knows the replaced record range, so
  it now returns those records' error delta and the tally moves by it. Both
  statuses equal a recount in tests (a keystroke in a good row, a heal, a
  break).
- **T5 · The find bar's list is patched onto the edit.** A keystroke with the
  bar open used to rescan the whole buffer (`Find::rescan`). `Find::patch` now
  rebuilds the list for one `EditSpan`: matches strictly above the edit keep
  their positions, matches strictly below shift by the edit's line delta (a
  multiline edit shifts by more than one), and only the edited run
  `[start, start+inserted)` is searched anew. `after_edit` feeds it the same
  spans the rest of the pipeline consumed once, so the bar's next relayout only
  re-lands on the caret. Anything a round cannot be localized to — no span, a
  multi-span batch, an edit past the buffer, a lost log — still falls back to
  the full rescan, and the current match is re-landed against the caret either
  way.
- **Tests**: `patch_onto_edits_agrees_with_a_full_rescan` (key typed below the
  caret, a line inserted above it, a deletion across a line break, the edited
  line itself) with a hand-checked endpoints test so the patches are the
  incremental shapes; `patch_falls_back_when_the_batch_cannot_be_localized`; the
  JSONL count test over a 6 000-record virtual file; the log tally compared
  against full scans; the top-of-file dirty oracle for T7. The gate passes
  7/7 in release with the G4 last-caret values unchanged.
- **Measured**: `pipeline_breakdown` on the 8 MB code fixture: find full rescan
  51.4 ms → the edit patch 0.4 ms per keystroke (well under the 10 ms buy),
  giving the bar the same headline as the dirty flag.
- **Validation**: 521 tests pass, `cargo clippy --all-targets -- -D warnings`
  and `cargo fmt --check` clean.
- **Limitations**: find's multi-span batches (a doc-wide replace) and an undo
  across several spans pay the full rescan; the log tally rides the same cap,
  rescanning instead of incrementing on anything wider than one localized
  edit.

## The banding round: the CSV grid and the Markdown band between frames (Oct 2026) — 523 tests green
Row 9 of the order table (POLISH.md S1, S2): the last document-sized pane bands
its rows, and the one band that kept re-rendering holds its ops across frames.

- **S1 · The CSV sheet paints in bands.** `layout::csv` now has a plan: the same
  width scan `layout` always ran, recorded once (column widths, x positions, the
  numeric-right-align decision) plus a `plan::Rows` index over the sheet's units
  — the header band with its rule, one unit per data row, the cap note. A band
  painter lays out only the units a viewport covers, through the same shared
  painter `layout` runs over every unit, so the band and the full list are the
  same ops (a pixel-for-pixel test asserts equality and a mid-sheet subsequence).
  The sheet parses once and stays on the tab beside its revision
  (`csv_sheet`), and `PanePlan::Csv` joins the planned kinds — the banded
  paint path, the replan, and the patch dispatch all gained the CSV arm.
- **T6, answered by the measurement S1 made possible**: a CSV keystroke
  re-parses the capped sheet and replans inline (the patch arm in both patch
  sites), and the gate now records CSV typing. At the gate's view: type 3–4 ms,
  band 314 ops, frame 2.8–3.6 ms. The row-patch alternative would spare only
  the parse of a sheet the parser has already capped at 2 000 rows, so the
  decision is: keep the re-parse, documented here as the intended behavior,
  and revisit only if that parse ever stops being bounded.
- **S2 · The Markdown band is cached between frames.** The tab keeps the last
  band it laid out with the units it covers and what it answered to (buffer
  revision, pane width, zoom, DPI scale). A scroll whose viewport's units all
  lie inside the covered range reuses the ops — the clip does the edges — and
  everything else builds fresh and becomes the next cache. An edit invalidates
  the cache by revision; the counter test proves rebuild-free scrolling inside
  a band, a rebuild past its end, and a rebuild after an edit with the typed
  text visible.
- **Measured** (release gate): csv frame 4.65 → 2.8–3.6 ms, whole-sheet install
  17 011 ops → 314; markdown frame 4.30 → 1.82 ms with the cache holding inside
  a band. Both budget keepers pass, 7/7 gate.
- **Validation**: 523 tests pass (the band-index test exempts CSV like JSONL —
  a planned pane carries no index — and the plan-or-bounded test now counts CSV
  among the planned kinds), `cargo clippy --all-targets -- -D warnings` and
  `cargo fmt --check` clean.
- **Found by the G4 row this round**: code's first far band pays the document
  once. The line plan's lazy checkpoint fill (`state_at`) scans from the last
  known checkpoint when nothing is known between it and the caret — an edit
  at the top resets all of them — so the gate's `end` keystroke measured a
  ~590 ms one-time pass over the document (band otherwise 0.1–0.2 ms). This
  is the T1-era checkpoint design, not the band cache. It was recorded as C1
  in POLISH.md and **landed the following round**: the worker builds with its
  checkpoints filled, a width replan adopts them, a keystroke re-proves them
  with one bounded scan, and where no worker exists (headless opens;
  `--pretty` has since dropped the `App` construction altogether) the plan
  fills at load — the gate's `end` row is band speed and every kind stays
  within budget (the landed note in POLISH.md has the numbers). App-level
  pin: `a_width_change_hands_the_plan_over_and_a_stale_plan_rebuilds`
  drives both halves through the event loop (hand-off on a current plan;
  rebuild on a stale one that matches the edited buffer).
- **Limitations**: the CSV plan's units are one per *physical* row, so a
  sheet whose rows wrap (a narrow pane) re-plans rather than re-flows them;
  the markdown cache serves one viewport-sized range at a time, so a big
  scroll jump pays one rebuild — a second-level cache of a page above and
  below the caret would absorb that too, if the numbers ever ask for it.

### Audit round (Oct 2026) — what a risk sweep asked for and got
- **The worker outcome could go stale twice.** A result landing after the
  view changed was already dropped; but if the worker *died* without sending
  (a panicked closure — sender disconnected), the old band stayed on screen
  with nothing ever marking the layout dirty. `poll_preview_job` now starts
  a rebuild in that arm too.
- **The loopback fetch test had a burnable connection.** Its listener
  accepted exactly one connection; urlmon's failed attempt opens the TCP
  connection and exhausts it, then curl (the next rung) finds the listener
  gone. Served now on an unbounded accept loop; the assertion is unchanged.
- **Tests no longer mistake a locked temp dir for a regression.**
  `testing::temp_dir()` answers once per process: the OS temp dir when a
  probe write lands there, `target/.tmp` otherwise — 100+ fixture writes used
  to fail as `PermissionDenied` en masse on hosts where `CreateFileW` into
  the user temp is refused. Shipped code keeps `std::env::temp_dir()`; the
  net fetch's test-only scratch override does the same job for a live
  download whose scratch file the rung itself must write.
- **The MSRV gate went up and came back down.** The release round first
  named `rust-version = "1.88"` (`slice::as_chunks` sets the floor) and
  ran a `msrv-clean-check` CI job on that exact toolchain from an empty
  target dir — it caught the code's own floor honestly, and locally
  `cargo +1.88.0 test` passed (528 green). The floor was then dropped by
  the owner's call: the gate job failed on runner-day variation an
  updater can outrun, and the product's stance is that whoever compiles
  this keeps the toolchain current. CI still type-checks all three
  platforms on current stable (`cargo clippy --all-targets -- -D
  warnings` in the `test` job), which was the actual guard the gate was
  aiming at; the incident that motivated it — a clean `cargo run`
  rejecting the unstable `if let` guard an incremental cache had been
  quietly tolerating — stays covered by clippy on every OS, not by a
  frozen compiler version.
- **The updater keeps evidence now.** Every check and install step appends a
  timestamped line to `%APPDATA%\Editor\last-update.log`; the install batch
  script logs msiexec's exit code, the wait-timeout path and the relaunch
  attempt itself — that batch runs after the Rust process has exited, so no
  Rust-side logging can see those steps. A new attempt rotates the previous
  one to `last-update.1.log`, so a reproduction cannot destroy the evidence
  it reproduces. Built for the update this round could only reconstruct:
  the user's in-app update closed the app, never relaunched, left no
  report — and nothing in the config dir said what msiexec had done.

### Update relaunch round (Oct 2026) — the installer's exit is not "installed"
- **Report**: installing a new version from the app's own UI closed the app,
  installed the new build, and never brought it back — no editor process
  afterwards, no crash report, nothing to read.
- **What the machine said**: the new version was on disk at the path the
  updater relaunches (`%LOCALAPPDATA%\Programs\Editor\editor.exe`) and its
  Add/Remove row was machine-wide, i.e. that install ran elevated. The two
  mechanisms the relaunch leans on were both exercised here and both work:
  cmd *does* wait for a GUI child inside a batch (a batch's `echo` after
  `winver.exe` only ran once the dialog closed), and `explorer.exe "<exe>"`
  *does* launch a GUI app from a hidden console. What neither of those does is
  prove the install is finished or that the launch happened.
- **Fix**: the batch no longer treats "msiexec returned" as "installed". It
  takes the installed exe's file stamp before the install and waits for it to
  change before reopening anything (bounded, and it stops waiting as soon as no
  msiexec is left, so a refused install opens what is there instead of sitting
  on the clock); it then asks Explorer to open the new build, checks the process
  table, retries that up to `LAUNCH_TRIES`, and falls back to
  `start "" /max` — an explicit show state, so the window cannot come up
  hidden the way `start`'s inherited one did in 0.2.3. Every attempt, the
  stamp-gate outcomes and the fallback reach `last-update.log`, so a report of
  "it didn't come back" now names the step that failed instead of being
  reconstructed from guesswork.
- **Verified**: the generated script was dumped from the real
  `windows_install_script` with a stand-in installer and run under cmd, twice
  — installer changes the exe (stamp gate takes `goto launch`, log shows
  `relaunch attempt 1: explorer …` then `relaunch: editor.exe is running`, no
  "left the exe alone" line) and installer leaves it alone (the escape fires
  immediately instead of waiting out the timeout). With a launch that starts
  nothing, the retry bounded at three and the `start /max` fallback ran, all
  three logged. 529 tests green, clippy and fmt clean; the new pins are the
  stamp capture/gate, the msiexec-presence escape, the verification banner and
  the fallback in `windows_install_waits_then_installs_then_relaunches`.

- **End-to-end (0.2.9)**: the real hop, not a stand-in — 0.2.7 installed from
  the shipped msi, a locally built 0.2.9 msi served as `v0.2.9` over loopback
  through `EDITOR_UPDATE_API`, its consent box accepted — installs the new
  build and reopens it: `msiexec exit 0`, `relaunch attempt 1: explorer …`,
  `relaunch: editor.exe is running`, and the relaunched 0.2.9 writes
  `the next launch read the install result: 0` into `last-update.log`. The hop
  also exposed what the stand-in could not: the settle loop asked `tasklist`
  for *any* `msiexec.exe`, and the Windows Installer service is a permanent
  session-0 one, so a refused install never took the "installer left the exe
  alone" escape and sat out the whole 120-tick timeout — about two minutes of
  no window — before reopening what was always going to be the old build. That
  check is now scoped to this session (`SESSIONNAME`), and the script test pins
  the filter.


### Blank editor after a mode switch, and a wheel that barely moved (Oct 2026) — 534 tests green
- **Report**: scroll a preview to the bottom, switch to the editor, and the
  editor is blank until the view is toggled away and back (Split suspected
  too); and the mouse wheel moves so little that the page needs a lot of
  spinning, while a finger on the glass is smooth with its momentum.
- **What the tests showed**: the editor paints a band — a screenful of rows
  built at the offset the layout ran at. `editor_band_stale` answered with the
  test for *does the band still cover the view?*, the negated question the
  paint path reads as stale, so a covered band was rebuilt every frame while a
  band the view had jumped out of was never rebuilt at all — and a mode switch,
  which settles the offset against the editor's own height *after* the range was
  measured against the preview's, left an empty inverted range painting nothing.
  That check also reused the band builder's slack-widened range, so every
  downward scroll counted as stale and rebuilt. The paint path then asked the
  question *after* its relayout decision, so a band that had gone stale was
  painted once more (clipped away, i.e. blank) and repaired only by whatever
  frame came next — which, after a click-driven mode switch, is nothing.
- **Fix**: the staleness test is the negation it should have been, asked against
  the rows the pane actually paints (not the builder's slack) and answered
  *before* the paint decides whether to relayout; a relayout whose settle moved
  the view gets one more pass from the settled offset, so a switch shows its text
  on the frame it paints. The wheel was a fixed 48 px per notch — 2.2 lines at
  the stock font, fewer as soon as the user zoomed — with X11 sending 120, so one
  wheel moved three distances on three platforms: a notch is now
  `WHEEL_LINES_PER_NOTCH` (3, Windows' own default) times `Handler::line_step_px`,
  the pane's line height, so it is three lines at any DPI or zoom and the same
  three lines everywhere.
- **Verified**: `switching_from_a_scrolled_preview_paints_the_editor_at_once`
  (one frame after the switch: text ops present, band covering, canvas painted),
  `the_editor_band_is_stale_only_once_the_view_leaves_it` (covered at rest and
  two rows down, stale at the end of the document),
  `the_split_editor_pane_stays_painted_wherever_the_progress_lands` (0, 35, 90
  and 100% of the shared progress) and
  `a_wheel_notch_steps_by_three_lines_of_the_current_text` (the step is the
  pane's line height, and doubles with a 2x zoom).


## Remaining responsiveness work

The product is frozen, so what is left is making the window answer faster. The
measurements, the workstreams and their acceptance tests are in
[`POLISH.md`](POLISH.md). Landed so far, per its notes: the gate's honesty set
(G1–G4), the unifications (R1–R5 — `app.rs` is `app/` now), the editor band
(T1), Markdown's block-splice typing (T2), the running status counts and find
patch (T4/T5), the dirty watermark (T7), CSV's plan/band (S1), the Markdown
band cache (S2), and code's tint-checkpoint fill (C1). Typing in every
planned kind is band speed except JSON's ~119 ms, which is T3's walking pace
while its slot-move suspect waits to be measured (the probe exists,
`pipeline_breakdown`).

Still open, in the order their numbers argue for it: **T3** (attribute and cut
JSON's keystroke; the gate measures it at both ends today, the attribution
probe is written), **S3** (a test proving five resizes during one worker job
end on the last size's plan — the coalescing has no proof in the suite),
**S4** (the streaming pump's per-tick cost, now measured only indirectly
through `readable`), and the P5 budget keepers **B1/B2** (typing is measured
at both positions but not budget-enforced; `pane_ops` bounds the planned
panes, the editor band has no op budget). Each carries its numbers in
POLISH.md; the ordering and the shortcuts not to take are its sections 5–8.
