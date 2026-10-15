# Editor

A small, native viewer and light editor for Markdown, JSON, JSONL, CSV,
configs (YAML, TOML, INI), logs, code and plain text — built in Rust with
nothing but the standard library: no frameworks, no runtime, no dependencies.

- **Preview first.** Documents open as clean rendered previews; a status-bar
  toggle (or the editable pane) gets you to the raw text.
- **Native.** Hand-drawn title bar with tabs, real overlay scrollbars, line
  numbers, drag-and-drop, dark and light themes that follow your OS.
- **Small and safe.** A single binary per OS, per-user install with no admin
  rights, and updates that are checksum-verified before anything runs.

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

## Install

### Windows

**Run the `.msi`** — that's the whole install. Per-user (no admin, no UAC
prompt): it adds a Start Menu shortcut, an Add/Remove Programs entry, and
registers Editor in Explorer's "Open with" menu for the file types it
supports. The setup is one page with two checkboxes — the file-type groups to
associate, both checked by default:

- *Associate document and config files* — md, markdown, json, jsonl, ndjson,
  txt, log, csv, tsv, yaml, yml, toml, ini, cfg, conf
- *Associate code files* — rs, py, js, ts, c, h, cpp, hpp, cs, java, go, rb,
  php, css, html, xml, sh

Adding a type only *offers* Editor in the "Open with" menu; it never takes
over a file type you've already assigned to another program.

The portable zip is the alternative: unzip it anywhere and run
`editor --install` (still per-user, still no admin) to register the same
associations. `editor --uninstall` removes them.

### Linux

Unpack the tarball and run `packaging/linux/install.sh`. It installs the
binary to `~/.local/bin`, creates a desktop entry that points at that binary
by absolute path (so it starts even when `~/.local/bin` is not on `PATH`),
installs the launcher icons into the user's `hicolor` theme, and sets the
default application for Markdown, JSON, NDJSON, plain text, CSV, YAML and
TOML.

### macOS

Unzip, then drag `Editor.app` into `/Applications`. The bundle declares the
same document types, so Finder's "Open With" offers it.

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

| | |
|---|---|
| Open a file | double-click it, drag it onto the window, `Ctrl+O`, or a right-click → Recent |
| Preview / Edit / Split | status bar buttons or `Ctrl+E` cycles them |
| Find | `Ctrl+F` |
| Theme | `Ctrl+T` or the status-bar toggle (persists) |
| About / help | `F2` / `F1` |
| Tabs | one per open file; a second launch of Editor hands its file to the window already open (Windows today) |
| Quit | `Ctrl+Q` or the window's × |

Your session — theme, view mode, open tabs, window size and position — is
restored on the next start.

## License

MIT. The bundled Liberation Sans & Mono fonts are SIL OFL 1.1.
