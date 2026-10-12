# Readitor

Native, dependency-free (std-only) Rust app for reading and lightly editing
Markdown, JSON, JSONL and text files. See [docs/PLAN.md](docs/PLAN.md).

```
cargo run -- samples/sample.md
cargo test
readitor --install      # register file associations (Windows: HKCU, no admin)
```

Status: **M6 – Windows polish + multi-file** (in progress). Custom title bar with one tab per open
file, `Ctrl+O` open dialog, drag-drop anywhere, `F1` help legend, right-click context menu,
clickable mode buttons in the status bar, preview drag-selection with copy, I-beam/hand
cursors, touch panning, `--install` file associations. M7 (TTF fonts) and M8/M9
(Linux/macOS backends with feature parity) are next.

Dev tip: type-check the Win32 backend from any OS with
`RUSTFLAGS="--cfg readitor_check_win" cargo check`.
