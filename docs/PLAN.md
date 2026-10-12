# Readitor — Plan

> **Readitor** = Read + Editor.

A small, native, installable document reader/editor for text-style files:
**Markdown, JSON, JSONL/NDJSON, plain text/logs.** Beautiful preview first,
light editing second. Not a code editor.

## Hard constraints

| Constraint | Consequence |
|---|---|
| Rust **std only**, zero crates | No GUI toolkit, no font lib, no parser libs. We write them. |
| Native app, **not** browser/webview | Own window + own renderer. |
| Windows / macOS / Linux | One portable core + three thin platform backends. |
| Installable with file associations | Per-OS packaging + registration. |

**What "std only" means here:** no crates.io dependencies. We *do* declare
`extern "C"` bindings to the OS's own system libraries (user32/gdi32, AppKit via
the Objective-C runtime, libX11). That is the only way any program opens a window;
it is FFI, not a dependency.

## Architecture

```
          ┌────────────────────────── portable (pure std, unit-testable) ──────────────────────────┐
 file ──► │ core::Buffer ─► parse::{markdown,json,jsonl} ─► layout (blocks, wrap, theme) ─► DisplayList │
          └───────────────────────────────────────────────────┬─────────────────────────────────────┘
                                                              ▼
                                  gfx::Canvas (software RGBA framebuffer) ◄── gfx::font (bitmap → TTF)
                                                              ▼
                      platform::Window  (events in, framebuffer blit out)
                      ├─ windows.rs  Win32: CreateWindowExW, WndProc, StretchDIBits
                      ├─ macos.rs    AppKit via objc_msgSend, NSView draw → CGImage
                      └─ linux.rs    Xlib: XCreateWindow, XPutImage (works on Wayland via XWayland)
```

Design rule: **everything above `platform/` never touches the OS.** The platform layer
only provides: create window, deliver `Event`s, present a pixel buffer, clipboard,
open/save dialogs, set title, set cursor shape, window drag/resize hit-testing,
drag-and-drop. Everything else is portable and testable headlessly
(render to a buffer, compare to expected output).

**Feature-parity rule (Oct 2026):** every user-facing feature — custom title bar,
tabs, cursors, file dialogs, drag-drop, help — must work on all three OSes.
Windows lands first (M6); the Linux (M8) and macOS (M9) backends must implement the
same `Host`/`Handler` surface before their milestone closes.

### Rendering model
Layout produces a `DisplayList` (rects, rounded rects, text runs, lines). The canvas
rasterizes it to RGBA. Redraw only on events (no busy loop). Dirty-rect optimization later.

### Fonts (the hard part, staged)
1. **Phase 1 ✅:** embedded **pre-rasterized anti-aliased atlas** (Liberation Sans ×4 styles + Liberation Mono, SIL OFL,
   Latin-1 + common punctuation/arrows, 36px source). Generated once by `tools/gen_font.py` into
   `assets/readitor-fonts.bin` (~550 KB) and pulled in with `include_bytes!`: zero runtime deps. Glyphs are
   area-averaged down to the requested size and cached. Fallback glyph for anything missing.
2. **Phase 2:** own TrueType parser (`glyf`, `cmap`, `hmtx`, `loca`, `head`) + scanline
   anti-aliased rasterizer, loading system fonts (Segoe UI / SF / DejaVu or Liberation) at runtime.
3. **Phase 3:** kerning, bold/italic via font variants, glyph cache, basic fallback.

## Scope

### File types
| Type | Preview | Notes |
|---|---|---|
| `.md` `.markdown` | Rendered document | CommonMark subset (below) |
| `.json` | Collapsible tree + pretty-printed view, syntax tinted | Error location on invalid JSON |
| `.jsonl` `.ndjson` | Record list, each record collapsible/pretty | Per-line error markers, streams large files |
| `.txt` `.log` | Clean wrapped text | Light level/timestamp tinting for `.log` (later) |

### Markdown subset (v1)
Headings, paragraphs, emphasis/strong/inline code, links, blockquotes, ordered/unordered/nested
lists, task lists, fenced code blocks, horizontal rules, tables (GFM), images (placeholder → later PNG decoder).
v2: footnotes, own PNG (inflate) decoder, HTML ignored.

### UI (deliberately small)
- Modes: **Preview** (default) · **Edit** · **Split**. `Ctrl/Cmd+E` toggles; the status bar always names the current mode.
- Tabs live in a **custom-drawn title bar** (all three OSes): small tabs share the
  strip with window buttons we draw ourselves (minimize / maximize / close); moving,
### UI (deliberately small)
- **Custom title bar**, drawn by us on all three OSes: Readitor name, a small tab per
  open file, and window buttons (minimize / maximize / close). Drag the bar to move
  the window, drag its edges to resize (thin platform hit-test hooks; all paint code
  is portable). *(Scope change, Oct 2026: tabs moved here from Non-goals and live in
  the title bar.)*
- Tabs: click to switch, `×` or `Ctrl+W` closes (dirty tabs warn once before
  discarding), `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle. `Ctrl+O` and drag-drop open new
  tabs; re-opening a path focuses its existing tab.
- Modes: **Preview** (default) · **Edit** · **Split**. `Ctrl/Cmd+E` toggles; the status
  bar always names the current mode.
- Help: `F1` toggles an in-app legend — modes, every shortcut, supported file types,
  how to select text and copy. Right-click opens a small context menu
  (copy/cut/paste/select all, copy-all in Preview, open/save/close/help) so the
  permanent chrome stays minimal.
- Cursors: I-beam over editable text, pointing hand over links, tree toggles, tabs
  and window buttons, arrow elsewhere.
- Plain text editor: caret, selection, copy/paste, undo/redo, find. No syntax highlighting, no indent magic.
- Light/dark theme following OS; adjustable zoom (`Ctrl +/-`).
- Open file / recent / drag-and-drop / reload-on-external-change.
- One window, one tab per file, opened via association, CLI, `Ctrl+O` or drag-drop: `readitor path/to/file.md`.

### Non-goals
Plugins, LSP, syntax highlighting for code, git, themes marketplace, collaboration.

## Repo layout
```
readitor/
├─ Cargo.toml                no [dependencies], ever
├─ docs/PLAN.md
├─ src/
│  ├─ main.rs                entry, CLI dispatch
│  ├─ cli.rs                 args: <file>, --install, --uninstall, --version, --pretty, --render
│  ├─ app.rs                 App state, event handling, mode switching
│  ├─ error.rs
│  ├─ core/    buffer.rs  undo.rs  file_kind.rs
│  ├─ parse/   markdown/{ast,block,inline}.rs  json/{value,lexer,parser,pretty}.rs  jsonl.rs
│  ├─ layout/  theme.rs  display_list.rs  text.rs  markdown.rs  json_tree.rs
│  ├─ gfx/     color.rs  geom.rs  canvas.rs  font/{mod,atlas,ttf}.rs
│  ├─ ui/      event.rs  scroll.rs  editor_view.rs  preview_view.rs  statusbar.rs
│  └─ platform/ mod.rs  windows.rs  macos.rs  linux.rs  install.rs
├─ assets/     readitor-fonts.bin  FONT-LICENSE.txt   (generated atlas + OFL notice)
├─ tools/      gen_font.py                            (dev-only; needs Pillow + fontTools)
├─ packaging/
│  ├─ windows/  build_installer notes (registry keys written by `readitor --install`)
│  ├─ macos/    Info.plist (CFBundleDocumentTypes), bundle.sh
│  └─ linux/    readitor.desktop, readitor-mime.xml, install.sh
└─ samples/    sample.md sample.json sample.jsonl
```

## File associations & install
- **Windows:** `readitor --install` writes `HKCU\Software\Classes` (ProgID `Readitor.Document`, extensions,
  `shell\open\command`, icon) via advapi32 FFI, then `SHChangeNotify`. No admin needed. `--uninstall` reverses it.
  Windows 10/11 may still require the user to confirm default app in Settings; we register as an option.
- **macOS:** `.app` bundle with `Info.plist` `CFBundleDocumentTypes` + UTIs; `bundle.sh` assembles it.
  File-open arrives via `application:openFile:` delegate, not argv.
- **Linux:** `.desktop` entry + shared-mime-info XML, `update-desktop-database`, `xdg-mime default`.

## Milestones
1. **M0 – Skeleton** ✅ tree compiles, stubs, samples, packaging files.
2. **M1 – Windows window:** ✅ Win32 window, resize, blit, key/mouse/wheel events, per-monitor DPI.
   First real run on Windows 11 (Oct 2026): `cargo run -- samples\sample.json` opens and idles
   in the message loop with no crash. Manual checks (crisp paint, wheel/keys, multi-monitor
   DPI, clean exit) still want a human pass — see acceptance checklist below.
3. **M2 – Text on screen:** ✅ font atlas, canvas primitives (clip, alpha, rounded rects, text), wrapped plain-text preview, scrollbar, status bar, zoom, light/dark, headless `--render`.
4. **M3 – JSON/JSONL:** ✅ strict lexer (positions, `\u`/surrogates, BOM) + depth-limited parser +
   2-space pretty-printer (`--pretty`) + collapsible tinted tree layout with error banners.
5. **M4 – Markdown:** ✅ block parser (ATX/setext heads, fences, quotes, nested/task lists, rules, GFM
   tables) + inline parser (emphasis, code, links, strike, breaks) + full document layout.
6. **M5 – Editing:** ✅ caret/selection movement, typing, clipboard, undo/redo, save, split view live re-render.
7. **M6 – Windows polish + multi-file:** ✅ custom title bar with file tabs, cursors (I-beam/hand),
   `Ctrl+O` open dialog, drag-drop anywhere on the window, `F1` help legend, right-click context
   menu, `--install`, zoom. Still open: window icon, OS-theme auto-follow, GUI subsystem.
   Linux/macOS parity for all of it lands with M8/M9.
8. **M7 – TTF fonts:** parser + AA rasterizer; swap in for bitmap.
9. **M8 – Linux (X11)** then **M9 – macOS (AppKit FFI)** backends + packaging.
10. **M10 – Large-file handling, find, recent files, reload-on-change.**

## Risks & mitigations
- **Font rendering from scratch** → staged; bitmap first so everything else isn't blocked.
- **macOS FFI via objc_msgSend is fiddly** (variadic ABI, struct returns) → keep it last; isolate in `macos.rs`.
- **Wayland-only Linux** → XWayland initially; native Wayland deferred (needs protocol marshalling by hand).
- **Can't test all three OSes locally** → portable core has headless tests; CI per-OS later.
- **IME / complex scripts / RTL** → explicitly out of scope for v1.

## Testing
Unit tests for parsers and buffer; golden-image tests rendering `samples/*` to a buffer
(hash compare) so layout regressions are caught without a window.

## Progress log
- **Scope change (Oct 2026, user):** file tabs promoted from non-goal to feature and
  live in a custom-drawn title bar (all three OSes); custom cursors (I-beam/hand),
  an `F1` help legend and a right-click context menu join M6. Feature parity across
  OSes is now a rule — see the architecture section.
- **M0** skeleton, 5 unit tests.
- **M1** `platform/windows.rs` (hand-written FFI to user32/gdi32/kernel32). FFI uses Rust-style
  names with `link_name` pointing at the real Win32 symbols, so `cargo clippy` stays clean with
  **zero `allow` attributes in `src/`** (project rule: fix lints, don't silence them).
  Type-checked from other OSes via `RUSTFLAGS="--cfg readitor_check_win" cargo check`
  (declared in `[lints.rust] check-cfg` so the cfg never warns).

### M1 acceptance checklist (run on Windows)
- [x] `cargo run -- samples\sample.json` starts and idles without crashing (Oct 2026 smoke run)
- [ ] Window titled `sample.md — Readitor`, crisp paint, resize without flicker
- [ ] Mouse wheel, ↑/↓, PgUp/PgDn, Home/End scroll; DPI sane across monitors
- [ ] `Ctrl+Q` / `Ctrl+W` / the close button exit cleanly
- [ ] Known limitation: release build still shows a console window (fixed in M6 with the GUI subsystem + AttachConsole)

- **M2** font atlas + `FontSystem`, `Canvas` (clip/alpha/AA rounded rect/text), `layout::text` wrapping, `PreviewView`
  painting + scrollbar, status bar, zoom, theme toggle, `--render` headless snapshots.

### Shortcuts
`↑ ↓ PgUp PgDn Home End` / wheel scroll (arrows move the caret in Edit/Split) ·
`Ctrl +` `Ctrl -` `Ctrl 0` zoom · `Ctrl T` light/dark ·
`Ctrl E` Preview → Edit → Split · `Ctrl S` save ·
`Ctrl O` open file (new tab) · `Ctrl Tab` / `Ctrl Shift Tab` cycle tabs ·
`Ctrl W` close tab (twice if unsaved) · `Ctrl Q` quit ·
`Ctrl C/X/V` copy/cut/paste · `Ctrl A` select all · `Ctrl Z/Y` undo/redo ·
`F1` help overlay · right-click context menu · drag files onto the window to open them.

### Dev: headless snapshots (no window needed, works on any OS)
```
readitor --render out.ppm --size 900x520 [--dark] [--scale 1.5] [--scroll 300] samples/sample.txt
readitor --pretty samples/sample.json        # canonical 2-space JSON to stdout
```

- **M3** strict JSON front end + tree preview, 35 unit tests, `cargo fmt --check` / `cargo clippy
  --all-targets` / `cargo build` all warning-free with zero `allow`s:
  - `parse/json/lexer.rs`: tokens with 1-based positions; strict numbers (no leading zeros, kept as
    source text); full string escapes incl. `\uXXXX` + surrogate pairs; BOM skipped; every failure is
    `Error::Parse { line, col, msg }`.
  - `parse/json/parser.rs`: recursive descent, any top-level value, key order + duplicates preserved,
    trailing content rejected, nesting capped at 128 with a positioned error instead of a crash.
  - `parse/json/pretty.rs`: canonical 2-space printer, `{}`/`[]` inline, round-trips through the parser.
  - `parse/jsonl.rs`: one record per non-blank line with physical line numbers; a bad line becomes a
    per-line error, never a file failure.
  - `layout/json_tree.rs`: tinted rows (theme `json_*`), word-wrapped with hanging indent, `[+]`/`[-]`
    toggles via `HitAction::ToggleNode` (click in Preview/Split-top), stable pre-order node ids,
    error banners with `line:col`, message, source snippet + caret; JSONL gets `#N` headers + rule separators.
  - `app.rs`: kind-dispatched relayout, `collapsed` set, click-to-toggle, JSON status in the status bar
    (`error 2:5`, `3 records, 1 error`); `--pretty` re-anchors JSONL errors to file lines.

### M3 acceptance checklist (all verified headlessly; window pass still manual)
- [x] `samples/sample.json` renders a tinted tree; `--pretty` output re-parses identically
- [x] Invalid JSON shows a banner (`! line:col: msg` + snippet + caret) and a status error
- [x] `samples/sample.jsonl` (incl. its broken line 3) shows 2 trees + 1 error banner + separators
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
  scripted `readitor --version` looks silent — the output lands after the
  script moved on (interactive use is fine; `Start-Process -Wait` to verify).
- M6 is now feature-complete; what remains is the user's real-window pass.

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
