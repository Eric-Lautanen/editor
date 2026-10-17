<p align="center">
  <img src="assets/icon.svg" width="76" alt="Editor icon">
</p>

<h1 align="center">Editor</h1>

<p align="center">
  <b>A small, native viewer and light editor for Markdown, JSON, CSV, configs, logs and code.</b><br>
  <sub>One Rust binary per OS. No frameworks, no runtime, no dependencies.</sub>
</p>

<p align="center">
  <a href="https://github.com/Eric-Lautanen/editor/releases/latest"><img src="https://img.shields.io/github/v/release/Eric-Lautanen/editor?label=release&amp;sort=semver" alt="Latest release"></a>
  <a href="https://github.com/Eric-Lautanen/editor/actions/workflows/ci.yml"><img src="https://github.com/Eric-Lautanen/editor/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-informational" alt="Windows, macOS and Linux">
  <img src="https://img.shields.io/badge/dependencies-none-brightgreen" alt="No dependencies">
</p>

<p align="center">
  <img src="assets/screenshots/markdown-dark.png" width="100%" alt="Editor rendering samples/sample.md in the dark theme: a rendered heading, paragraphs with inline styles, a link and a table of contents of the file">
</p>

Editor opens a file, renders it the way you want to read it, and gets out of
the way. Everything is drawn by its own software renderer onto a single
window: no web view, no toolkit, no runtime to install. `Ctrl+E` cycles
**Preview → Edit → Split**, so the same tab is both the document and the
source.

## Why this one

- **Preview first.** Documents open as clean rendered previews — headings and
  tables for Markdown, a collapsible tree for JSON, a real table for CSV —
  with the raw text one keystroke away.
- **Native.** A hand-drawn title bar with tabs, overlay scrollbars that stay
  out of the way, a line-number gutter, per-file drag and drop, and dark and
  light themes that follow your OS.
- **Truly dependency-free.** `[dependencies]` in `Cargo.toml` is empty and
  stays that way: the Markdown, JSON and CSV parsers, the layout engine, a
  TrueType rasterizer with Unicode fallback, the software canvas, the SHA-256
  that verifies downloads, and the window and event layer for all three
  platforms (hand-bound Win32, AppKit and Xlib FFI) all live in this
  repository.
- **Small and safe.** A single binary per OS, a per-user install that needs no
  admin rights, and updates that are checksum-verified before anything runs.

## Views

**Split** — the rendered document and its source on one screen, scrolled
from the same offset, each with its own scrollbar:

<img src="assets/screenshots/split-dark.png" width="100%" alt="Split view: rendered Markdown on the left, the raw source with a line-number gutter on the right">

**Edit** — the raw file, no wrapping, with a line-number gutter and a
horizontal scrollbar for the lines that run long:

<img src="assets/screenshots/edit-light.png" width="100%" alt="Edit view in the light theme: raw Markdown with line numbers and no wrapping">

## What it renders

One renderer for every kind of file — each of these is a real file from
[`samples/`](samples), opened by the app and rendered headlessly:

<table>
  <tr>
    <td width="50%" align="center">
      <img src="assets/screenshots/json-tree.png" alt="A JSON document as a collapsible tree"><br>
      <sub><b>JSON</b> — a collapsible tree (<code>[-]</code> / <code>[+]</code>), one per line for JSONL</sub>
    </td>
    <td width="50%" align="center">
      <img src="assets/screenshots/csv.png" alt="A CSV file laid out as a column table with a header band"><br>
      <sub><b>CSV / TSV</b> — columns, a header band, measured cells</sub>
    </td>
  </tr>
  <tr>
    <td width="50%" align="center">
      <img src="assets/screenshots/markdown-light.png" alt="Markdown rendered in the light theme, including fenced code and a table"><br>
      <sub><b>Markdown</b> — headings, lists, quotes, tables, code, links</sub>
    </td>
    <td width="50%" align="center">
      <img src="assets/screenshots/code-dark.png" alt="Rust source with a light syntax tint"><br>
      <sub><b>Code</b> — rs, py, js, ts, c, go, sh and friends, tinted</sub>
    </td>
  </tr>
  <tr>
    <td width="50%" align="center">
      <img src="assets/screenshots/config-light.png" alt="A TOML config rendered in the light theme"><br>
      <sub><b>Configs</b> — YAML, TOML, INI/CFG/CONF</sub>
    </td>
    <td width="50%" align="center">
      <img src="assets/screenshots/log-dark.png" alt="A log file with ERROR, WARN, INFO and DEBUG levels tinted"><br>
      <sub><b>Logs</b> — levels tinted, plain text everything else</sub>
    </td>
  </tr>
</table>

Every screenshot above was drawn headlessly by the app itself
(`editor --render` on the files in [`samples/`](samples)) and converted to PNG
by [`tools/readme_shots.py`](tools/readme_shots.py) — there is no screenshot
tool in the loop, so the pictures cannot drift from what the renderer draws.

## Download

Grab the latest build from
[GitHub Releases](https://github.com/Eric-Lautanen/editor/releases/latest):

| File | For |
|---|---|
| `editor-<tag>-windows-x86_64.msi` | Windows 10/11 — the recommended install |
| `editor-<tag>-windows-x86_64.zip` | Windows — portable (no install) |
| `editor-<tag>-linux-x86_64.tar.gz` | Linux (X11/XWayland) |
| `editor-<tag>-macos.zip` | macOS |

Every release publishes a `SHA256SUMS` manifest listing the SHA-256 of each
asset; verify the file you download against it if you want to check it yourself.
The Windows build is Authenticode-signed and the macOS bundle is signed and
notarized when the release workflow has its signing secrets configured (see
[packaging/SIGNING.md](packaging/SIGNING.md)); without them the binaries are
unsigned and the OS will show its usual unknown-publisher warning.

## Install

### Windows

**Run the `.msi`** — that is the whole install. It is per-user, so there is no
admin prompt, and the wizard is one page with three checkboxes, all ticked:

| | |
|---|---|
| Documents and config files | md, markdown, json, jsonl, ndjson, txt, log, csv, tsv, yaml, yml, toml, ini, cfg, conf |
| Code and markup files | rs, py, js, ts, c, h, cpp, hpp, cs, java, go, rb, php, css, html, xml, sh |
| Create a shortcut on the Desktop | a Start menu entry is always added |

These only *offer* Editor in the Explorer "Open with" menu — a file type you
have already assigned to another program keeps that program. The install also
adds an entry to Installed apps, and running the installer again with
different boxes ticked changes your choices.

The portable zip is the alternative: unzip it wherever you like and run
`editor --install` to register the same "Open with" entries (still per-user,
still no admin, and no shortcuts — the zip stays where you put it).
`editor --uninstall` removes them.

### Linux

Unpack the tarball and run `packaging/linux/install.sh`. It installs the
binary to `~/.local/bin`, adds a launcher to your applications menu **and a
shortcut on your Desktop**, installs the launcher icons, and makes Editor the
default app for Markdown, JSON, NDJSON, plain text, CSV, YAML and TOML (Linux
has no "Open with" list to be offered in, so there it does take the defaults).
Set `EDITOR_NO_DESKTOP_SHORTCUT=1` to leave the Desktop alone.

To undo all of it, delete `~/.local/bin/editor`,
`~/.local/share/applications/editor.desktop`, `~/Desktop/editor.desktop` and
`~/.local/share/icons/hicolor/*/apps/editor.png`.

### macOS

Unzip, then drag `Editor.app` into `/Applications`. The bundle declares the
same document types, so Finder's "Open With" offers it. There is no Desktop
shortcut to add here — the Dock and Spotlight are how an app is launched.

The bundle is not signed or notarized yet, so Gatekeeper refuses the first
launch of a downloaded copy: right-click the app and choose **Open** once
(or run `xattr -d com.apple.quarantine /Applications/Editor.app`).

### From source

```
cargo run -- samples/sample.md
cargo test
```

Release-mode binaries for your own OS come from `cargo build --release`.

## Updating

Editor checks GitHub for a newer release **at most twice a day**, while the
app is running. There is nothing to configure: the check is silent, and when
it finds a newer version it asks first — nothing is downloaded before you
click "Yes". Downloads are then verified against the release's checksum
manifest before the installer runs, so a truncated or tampered download is
refused, never executed.

You can turn the automatic check off in the app: **F2 (About) → uncheck
"Check for updates automatically"**. Manual updates stay available:

```
editor --update      # check GitHub and install the newest release now
--no-update          # skip the automatic check for one launch
```

A newer install always replaces the older one in place, so file associations
keep working — re-running `--install` is only needed if you *move* the
executable. `editor --version` prints what you have.

## Using it

Every shortcut the app knows, generated from `src/ui/keymap.rs` — the F1
overlay paints the same table, and `cargo test` refuses a stale paste here or
a row whose chord no longer works:

<!-- keymap:start -->
| Keys | Action |
|---|---|
| `Ctrl+E` | cycle Preview → Edit → Split |
| `Ctrl+O` | open a file in a new tab |
| `drag & drop` | drop files onto the window to open them (Windows, macOS) |
| `Ctrl+Tab` | next tab (Shift = previous) |
| `Ctrl+R` | reload the active file from disk |
| `Ctrl+W` | close tab (press twice if unsaved) |
| `Ctrl+S` | save |
| `type` | edit the document |
| `arrows` | move the caret (Shift = select) |
| `mouse drag` | select text |
| `Ctrl+A` | select all |
| `Ctrl+C / X / V` | copy / cut / paste |
| `Ctrl+Z / Y` | undo / redo |
| `Tab` | insert four spaces |
| `wheel, arrows` | scroll |
| `double-click` | select a word in the preview; a third click takes the row |
| `Ctrl+F` | find bar (Enter/F3 next, Shift back) |
| `Ctrl + / - / 0` | zoom in / out / reset |
| `Ctrl+T` | light / dark theme |
| `click [+]` | fold / unfold JSON nodes |
| `hover a link` | show its address in the status bar; click to open it |
| `F1` | this help |
| `F2` | about editor |
| `right-click` | context menu |
| `Ctrl+Q` | quit |
<!-- keymap:end -->

A file also opens by double-clicking it or from right-click → Recent. Tabs hold
one file each, and a second launch of Editor hands its file to the window
already open (Windows today). The status bar carries the same mode, theme and
zoom controls. Quit from `Ctrl+Q` or the window's ×.

Your session — theme, view mode, open tabs, window size and position — is
restored on the next start.

## License

MIT. The bundled Liberation Sans & Mono fonts are SIL OFL 1.1.
