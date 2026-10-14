# Windows packaging

Two supported shapes:

* `editor.wxs` — a per-user **MSI installer** built in CI (`release.yml`,
  `wix build`, pinned WiX v7 + the UI extension). Double-click installs with a
  wizard whose Customize page lets the user choose which file-type groups to
  associate (documents/configs vs code files); a silent run (from
  `editor --update` → `msiexec /passive`) takes both groups at their default
  level and major-upgrades any previous MSI in place, no admin anywhere
  (`Scope="perUser"`, HKCU associations only). `editor.exe --uninstall`
  remains the *portable* uninstall path; MSI-installed builds uninstall
  through Add/Remove Programs.
* Portable zip/`--install`: copy `editor.exe` anywhere and run the portable
  install flag (writes the same HKCU associations, no admin); the portable
  uninstall flag removes them.

Release notes: `.github/workflows/release.yml` builds
`editor-<tag>-windows-x86_64.msi` (user-facing installer) next to the zip; the
updater (`src/update.rs`) downloads the MSI by exactly that name.

Icon/manifest embedding without crates is tracked in docs/PLAN.md (M6):
needs `rc.exe`/`windres` or a hand-built `.res` linked via `build.rs`
(std only); the shortcut/ARP icon comes from `assets/icon.ico` meanwhile.
