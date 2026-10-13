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

### Memory round (Oct 2026) — fallback fonts cost the whole file, so map them

Baseline, release build, `samples/sample.md` (1100×800): **41.7 MB working set,
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
  `Get-Process` on the live window): `samples/sample.md` live heap **22.9 MB →
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
- **`--cfg readitor_check_linux` works from Windows** (the symmetric of the
  existing Win32-from-Linux check): the Win32 module is gated on
  `any(target_os = "windows", readitor_check_win)` instead of excluding
  `readitor_check_linux`, and both cfgs are declared in `Cargo.toml`. The X11
  backend type-checked for the first time under that flag (a stray brace and a
  duplicated `intern` signature were blocking it).
- **Samples are long enough to scroll** (`samples/sample.md`, `sample.txt`,
  `sample.jsonl`, `sample.json`): the old samples fit on one screen, which is
  why no scrollbar ever appeared in a window or a `--render` snapshot.

### M8 round (Oct 2026) — Linux X11 backend lands (type-checked), 229 tests green
- **`src/platform/linux.rs`** (~1250 lines, full Xlib FFI, zero crates):
  borderless window (`_MOTIF_WM_HINTS` decorations off) with the app-drawn
  title bar, **manual move** (title-bar `Drag` region) and **manual edge
  resize** (6px bands, min 200×100), full event translation — keys with
  Ctrl/Shift/Alt chords + `XLookupString` text, mouse, wheel/tilt (buttons
  4–7 → `Event::Scroll`), `ConfigureNotify` → `Resized`, `WM_DELETE_WINDOW`
  → `CloseRequested` — canvas blit via a hand-built `XImage` header
  (ZPixmap, depth 24, 4 bytes/px — no `XCreateImage`/`XDestroyImage` needed),
  **clipboard** through the CLIPBOARD/PRIMARY selections (we own the
  selection; `SelectionRequest` answered with UTF8_STRING/STRING/TARGETS,
  reads spin the queue with a deferred-event replay), **file dialogs** via
  `zenity`/`kdialog`, `xdg-open` for links, `_NET_WM_ICON` from the portable
  icon renderer, `_NET_WM_STATE` toggle for maximize, `XIconifyWindow`,
  dark preference via `gsettings`, centered on the primary screen, 40 ms
  poll-loop `Tick` (XPending + sleep), arrow/xterm/hand2 cursors.
- **`readitor_check_linux`** dev cfg: type-checks the X11 backend from
  Windows/macOS (`RUSTFLAGS="--cfg readitor_check_linux" cargo check`) —
  the mirror of `readitor_check_win`. `platform/mod.rs` + `install.rs` cfg
  gating updated so exactly one backend is `use`d per configuration;
  `system_prefers_dark` now routes to the Linux backend on Linux.
- **Not yet on Linux**: XDND drag-and-drop (the one parity gap, tracked for
  the M8 follow-up), XIM/UTF-8 dead-key compose input (`XLookupString`
  covers Latin-1 today), DPI scale is fixed at 1.0.
- Runtime verification needs a real X server (X11/XWayland) — CI
  (`ci.yml`) compiles + runs the headless suite on ubuntu-latest from the
  first push; the window itself needs a human or Xvfb pass.

### Cross-OS verification round (Oct 2026) — the macOS CI job would have failed on push

The M8 work was re-checked against what CI actually runs by *simulating each
foreign backend's values* on Windows (temporary env-var toggles, since removed)
rather than waiting for a runner. Three defects, all fixed:
- **The macOS release zip unpacked wrong**: `ditto -c -k --sequesterRsrc
target/Readitor.app out.zip` without `--keepParent` archives the bundle's
*contents*, so the download extracts a stray `Contents/` directory instead of
`Readitor.app` — "drag it to /Applications" could not work. `--keepParent` is
what puts the bundle itself at the archive root.
- **`readitor_check_macos` could not be run from Linux**: the macOS arm of
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

**Verified with no Linux or macOS machine**: `READITOR_TEST_BAND=0` (macOS
band), `=6` (X11), `=12`, and font mapping forced off (macOS, pre-M9) each run
the full suite green — 234 tests, where `=0` was 233 passed / 1 failed before the
test fix. With mapping off, 15 headless renders (four samples + a
CJK/emoji/symbols document × three size/scale combinations) are byte-identical
to the mapped renders, so the macOS read-through path draws the same pixels.
`cargo test`, `cargo clippy --all-targets` (with and without `mem-census`),
`cargo fmt --check`, and `cargo check` under all three `readitor_check_*` cfgs
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
  --check` and `cargo check` under all three `readitor_check_*` cfgs clean.
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
  harness drove `readitor.exe` with `APPDATA` pointed at a temp dir — moved it to
  `200,150 900x700` and closed → the session file held exactly that; moved to
  `240,180 880x660`, maximized and closed → the file kept that rect with
  `maximized 1`; the next launch came up maximized and `SW_RESTORE` landed back
  at `240,180 880x660`. A session hand-edited to `window -5000 -5000` opened
  centered (`410,200 1100x800`) instead. Clicking the title bar's × with a
  synthetic client click quit the app and wrote the new rect.
- 246 tests (10 new, all headless: the normal-rect rule, the restore request,
  quitting, the settings round trip, and the placement guard); `cargo clippy
  --all-targets -- -D warnings`, `cargo fmt --check` and `cargo check` under all
  three `readitor_check_*` cfgs clean.
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
  under all three `readitor_check_*` cfgs clean.
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
  `src/settings.rs`/`src/app.rs` clean; `readitor_check_win`/`_linux` clean. At the time of
  writing, crate-wide `cargo fmt --check` and `readitor_check_macos` fail on another agent's
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
- **Measured** (`readitor --render`, same file, 1920×1168): code panel 550..1369 with text
  overflowing to 1778 **before** → panel 215..1704 with no ink past it **after**; at 1100 px the
  column is 838 (the old measure, i.e. small windows are untouched); at 2560 it stops at the
  1500 px ceiling.
- **Verified on the real window**: maximized `readitor.exe` captured with `PrintWindow`
  (1936×1168): text ink 218..1717, i.e. a 1500 px column centered with 218 px margins on both
  sides — 1.83× the old measure — and nothing drawn outside it.
- 5 new tests (the column grows and stops at the ceiling; stays centered and inside a narrow
  window; scales with zoom; an 800-char code line folds into rows that fit the panel; the app's
  preview column grows when the window does). A negative control — the cap back at 820 — fails
  the two growth tests and only those (the theme file was restored byte-identical afterwards).
- 258 tests; `cargo clippy --all-targets -- -D warnings`, `rustfmt --check` on every touched file
  and `readitor_check_win`/`_linux` clean (`readitor_check_macos` still fails on another agent's
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
  `readitor_check_win`/`_linux` clean.
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
- **`readitor_check_macos`** dev cfg: the module/install/Cargo plumbing landed with the
  memory round; this round added the missing `not(readitor_check_macos)` arms in
  `install.rs` and the `print_*` dispatches, and collapsed `system_prefers_dark` to
  `backend::system_prefers_dark()` now that all three backends have it.
  `RUSTFLAGS="--cfg readitor_check_macos" cargo check` type-checks the AppKit backend from
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
  `readitor_check_win`/`_linux`/`_macos` all check clean.

### Scroll-smoothness round (Oct 2026) — a wheel notch glides instead of jumping, 269 tests green
- **The report**: scrolling "seems a little choppy or lagging just a little". Measured first, and the
  frame was never the problem: a full repaint of `sample.md` at 1100×800 costs ~1 ms in release
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
  `readitor_check_win` / `readitor_check_linux` 0.
- **Not verified / limits**: the macOS backend implements no frame clock (the `set_animating`
  default), so its glide would advance on the 40 ms heartbeat alone — its file was another agent's
  in-flight work this round and was left untouched; the X11 frame clock is type-checked
  (`readitor_check_linux`) but cannot be run from this machine; and the glide's tail is deliberate
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
- Still open in M10: find (`Ctrl+F`) and large-file handling.

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