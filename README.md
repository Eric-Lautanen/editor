# Editor

Native, dependency-free (std-only) Rust app for reading and lightly editing
Markdown, JSON, JSONL, CSV, configs (YAML/TOML/INI), logs, code and text
files. See [docs/PLAN.md](docs/PLAN.md).

```
cargo run -- samples/sample.md
cargo test
editor --install      # register file associations (Windows: HKCU, no admin)
```

Status: **M7 done (live TTF fonts) — Windows feature-complete; Linux/macOS
backends (M8/M9) in progress.** Custom title bar with one tab per open file,
`Ctrl+O` open dialog, drag-drop anywhere, `F1` help legend, right-click
context menu, clickable mode buttons in the status bar, preview drag-selection
with copy, working overlay scrollbars, line-number gutter, single/double/
triple-click caret/word/line selection, session persistence (theme, mode,
open tabs, window size/place/maximized), `--install` file associations.

## Install & file associations

| OS | How |
|---|---|
| Windows | **Run the release `.msi`** — no admin, no command line: per-user install, Start Menu shortcut, Add/Remove Programs entry, and a wizard (Customize page) where you choose which file types to associate (documents/configs vs code; uncheck either). A newer MSI replaces the installed one automatically, and the app checks GitHub for updates in the background — `editor --update` does the same from a terminal, `--no-update` opts a launch out. The zip stays for portable use: unzip anywhere, run `editor.exe --install` once for all supported extensions under `HKCU` (no admin), `--uninstall` removes them. |
| Linux | `packaging/linux/install.sh` — installs to `~/.local/bin`, a `.desktop` entry, a MIME package for `.jsonl`/`.ndjson`, and sets `xdg-mime` defaults for markdown/JSON/NDJSON/plain text/CSV/YAML/TOML. `editor --update` can also fetch and self-replace a newer release. |
| macOS | `Editor.app` (built by `packaging/macos/bundle.sh`) declares `CFBundleDocumentTypes` for the same extensions, so it appears in "Open With". `editor --update` swaps the bundle for a newer release. |

## Updating

There is no in-app updater: releases are version-numbered overwrites. Replace
the binary (Windows/Linux) or the whole `Editor.app` (macOS) with the newer
one — associations keep pointing at the same path, so nothing needs
re-registering unless you *move* the executable (then re-run `--install` /
`install.sh`). `editor --version` prints what you have.

## CI & releases (GitHub Actions)

- `.github/workflows/ci.yml` — `cargo test` + `cargo clippy` on
  **windows-latest, ubuntu-latest, macos-latest** on every push/PR (the whole
  suite is headless, so the portable core is exercised on all three OSes),
  plus `cargo fmt --check`.
- `.github/workflows/release.yml` — pushing a tag (`v0.1.0`, …) builds and
  tests on all three OSes, packages per-OS archives (Windows zip, Linux
  tar.gz with the install script, macOS `.app` zip) and publishes them to a
  GitHub Release.

Dev tip: type-check the Win32 backend from any OS with
`RUSTFLAGS="--cfg editor_check_win" cargo check`. Allocation census:
`cargo run --features mem-census -- --mem-report FILE`.
