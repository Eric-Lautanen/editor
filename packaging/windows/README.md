# Windows packaging

Two supported shapes:

* `editor.wxs` — a per-user **MSI installer** built in CI (`release.yml`,
  `wix build`, pinned WiX v7 + the UI extension). Double-click installs through
  a two-page wizard: a welcome page whose three checkboxes choose which
  file-type groups to associate (documents/configs, code/markup) and whether to
  put a shortcut on the Desktop, then the stock progress bar and a "Setup
  complete" page whose Finish button closes the wizard. Every box is ticked by
  default and each one owns a feature — `DocumentTypes`, `CodeTypes`,
  `DesktopShortcutFeature` — so the state of a checkbox is the state of that
  group, and a silent run (from `editor --update` → `msiexec /passive`) runs no
  UI and lands on the same three defaults. Either way the install
  major-upgrades any previous MSI in place, and the Start menu entry is not
  optional: it is how an app is expected to be reachable afterwards. Per-user is the preferred scope
  (`Scope="perUser"` plus `ALLUSERS=2` / `MSIINSTALLPERUSER=1`), so a standard
  user installs entirely inside their own profile with no UAC prompt;
  associations are always HKCU, and only an administrator running the MSI from
  an already-elevated session gets the machine-wide Add/Remove Programs row
  Windows Installer defaults to in that case. `editor.exe --uninstall` remains
  the *portable* uninstall path; MSI-installed builds uninstall through
  Add/Remove Programs.

  Build defines: `Version`, `PublishDir` (where `editor.exe` lives), `Icon`
  (`assets/icon.ico` — both shortcuts' and Add/Remove Programs' icon) and
  `DialogIcon` (`assets/icon-setup.ico` — the header icon the dialogs draw).

  `assets/icon-setup.ico` exists because MSI's Icon control renders the *first*
  frame of an icon stretched to the control's pixel size; `icon.ico` leads with
  its 16px frame, which is what made the header icon a blurry block. The dialog
  asset leads with the 48px frame instead. Build a dialog control at 32x32
  dialog units (48px at 100% scaling) to keep it 1:1. `icon-setup.ico` is
  derived - regenerate it with `python tools/gen_setup_icon.py` whenever
  `icon.ico` (rasterized from `assets/icon.svg`) is rebuilt.

* Portable zip/`--install`: copy `editor.exe` anywhere and run the portable
  install flag (writes the same HKCU associations, no admin); the portable
  uninstall flag removes them.

Release notes: `.github/workflows/release.yml` builds
`editor-<tag>-windows-x86_64.msi` (user-facing installer) next to the zip; the
updater (`src/update.rs`) downloads the MSI by exactly that name.

Icon/manifest embedding without crates is tracked in docs/PLAN.md (M6):
needs `rc.exe`/`windres` or a hand-built `.res` linked via `build.rs`
(std only); the shortcut/ARP icon comes from `assets/icon.ico` meanwhile.
