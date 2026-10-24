# Windows packaging

Two supported shapes:

* `editor.wxs` — a per-user **MSI installer** built in CI (`release.yml`,
  `wix build`, pinned WiX v7 + the UI extension). Double-click installs through
  a two-page wizard: a welcome page whose three checkboxes choose which
  file-type groups to associate (documents/configs, code/markup) and whether to
  put a shortcut on the Desktop, then the stock progress bar and a "Setup
  complete" page whose checked-by-default **Launch Editor** box opens the app
  when Finish is pressed. Every box is ticked by
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

  The install creates **no System Restore point** (`MSIFASTINSTALL=1`). Windows
  Installer snapshots the system before copying files whenever the product is
  not already installed in that context, and that silent `SRSetRestorePoint`
  call is where the progress bar used to sit: a fresh install of this package
  measured 9.6-13.4 s wall, 12.4 s of it inside the restore-point call, while
  the same install with the property set took 0.9 s. Reinstalls over an
  existing version skip the snapshot, which is why the stall looked
  intermittent. Editor installs one exe and a set of HKCU rows, and the
  updater installs often, so the checkpoint bought nothing a re-run of the MSI
  does not; drop the property (or set it to 0) to get it back.

  An update ends with the app running again — but that relaunch is **not** in
  this package. It started there (`RestartEditor`, a deferred `asyncNoWait`
  action before `InstallFinalize`) and had to be removed: a major upgrade runs
  the execute sequence twice, so the action fired twice and the second Editor
  hung on the first one's single-instance mutex, while the process it left in
  the transaction could hold a file the install wanted to replace — which is
  how an update ends in a scheduled restart (a real reboot, seen once). The
  updater now owns it, from a staged `install.bat` that: waits for the running
  Editor to leave the process table, runs `msiexec /i <msi> /passive
  REBOOT=ReallySuppress`, waits for the installed exe's file stamp to change
  (msiexec returning is not the same as the new build being on disk: an
  elevated pass is not the process the batch started and can still be
  finishing, and reopening early starts the old build, which the upgrade then
  closes), and only then reopens it through Explorer. Explorer is asked,
  the process table is checked, and the request is retried; `start "" /max` is
  the last resort, because `start` inherits a hidden console's show state and
  would otherwise open the window invisibly. Every step lands in
  `%APPDATA%\Editor\last-update.log`. `REBOOT=ReallySuppress` means no update
  can reboot the machine. Windows Installer alone does *not* relaunch anything:
  Restart Manager closes the window whose exe it must replace and leaves it
  closed (measured: a `/qn` install over a running Editor ended with no Editor
  at all), which is why the updater does it. A scripted `/qn` or `/passive`
  install shows no page and so opens nothing, which is what a deploy wants.

  A **wizard** install does open the app, from the completion page: Finish
  publishes `LaunchEditor` (an immediate, `asyncNoWait`, impersonated action on
  the installed exe) when the **Launch Editor** box is ticked, and the action
  runs *after* `InstallFinalize`, outside the transaction. That placement is
  the whole point — the earlier `RestartEditor` action was deferred and fired
  before finalize, which is what double-launched during a major upgrade and
  left a process holding the file during finalisation. From the finish dialog
  it is one user-pressed action on a finished install. If an Editor is somehow
  already running, the new process finds its single-instance mutex, hands the
  launch over and exits, so no second window appears.
  Argument-less is deliberate: the relaunched app restores the session it saved
  on the way out, which is the last file and the window place.

  The uninstall sequence runs `editor.exe --uninstall` (a deferred custom
  action, `REMOVE="ALL"` only) before it removes the files. That call removes
  the file associations the package wrote *and*, the part Windows Installer
  cannot see, the staged update downloads under `%TEMP%`
  (`editor-update-<tag>\` — the updater's own directory; the released 0.2.1
  predates this and leaves its msi there). `Return="ignore"`, so a locked or
  already-deleted exe never blocks the removal itself. User data
  (`%APPDATA%\Editor`: settings, session, crash reports) is deliberately kept.

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

The exe's own icon and version stamp come from `build.rs`: it assembles
`assets/icon.ico` into a `.res` by hand (RT_ICON + RT_GROUP_ICON + RT_VERSION,
no crates) and hands it to the MSVC linker, so `DefaultIcon =
editor.exe,0` and Explorer's Properties pane both have something real to read.
That script only runs for a `windows`/`msvc` target — a GNU host links
nothing, so those builds keep an iconless, unstamped exe. The installer's own
Icon table still uses `assets/icon.ico` for the shortcuts and Add/Remove
Programs.
