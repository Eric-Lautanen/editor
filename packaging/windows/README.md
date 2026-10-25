# Windows packaging

Two supported shapes:

* `editor.wxs` — a per-user **MSI installer** built in CI (`release.yml`,
  `wix build`, pinned WiX v7 + the UI extension). Double-click installs through
  a two-page wizard: a welcome page whose four checkboxes choose which
  file-type groups to associate (documents/configs, code/markup,
  dotfiles/tool config) and whether to put a shortcut on the Desktop, then the
  stock progress bar and a "Setup complete" page whose checked-by-default
  **Launch Editor** box opens the app when Finish is pressed. Every box is
  ticked by default and each one owns a feature — `DocumentTypes`,
  `CodeTypes`, `DotfileTypes`, `DesktopShortcutFeature` — so the state of a
  checkbox is the state of that group. The dotfile group registers
  `.gitignore`-style names as extension keys (`Software\Classes\.gitignore`),
  which is the only shape Windows accepts for them; bare names such as
  `Dockerfile` have no per-filename association and stay **Open with** choices.
  A silent `/qn` run shows no UI and lands on the same four defaults; an
  `editor --update` run (`msiexec /passive`) shows a progress bar and passes
  `EDITOR_SKIP_LAUNCH=1`, so the package launches nothing and the staged
  helper reopens the app (see below). Either way the install
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

  An update ends with the app running again exactly once. The hand-off is a
  copy of the running exe staged in the payload directory and invoked as
  `editor --apply-update <msi> <exe> <pid> <result> <log>`: separate argv
  strings, no shell, no batch language, and no quoting rules to get wrong. It
  waits for the old Editor to leave the process table *and* for
  `last-install.approve` beside `last-install.txt` — the token the window
  writes in the same tick it saves its session and quits. Removing that token
  is the claim, so two helpers racing over one update cannot both install; a
  window closed by the user, or one that stayed open over a dirty buffer,
  leaves none, and then nothing is installed. `last-install.cancel` stops a
  waiting helper immediately when the editor decides to stay.

  With the token claimed, the helper runs `msiexec /i <msi> /passive
  REBOOT=ReallySuppress MSIDISABLERMRESTART=1 MSIRESTARTMANAGERCONTROL=Disable
  EDITOR_SKIP_LAUNCH=1`, writes msiexec's exit code to `last-install.txt`, and
  waits for the installed exe's file stamp to change. That wait is not
  optional: msiexec returning is not the same as the new build being on disk
  (an elevated pass is not the process the helper started and can still be
  finishing), and reopening early starts the old build, which the upgrade then
  closes. Only then does it relaunch, through `ShellExecuteW` with an explicit
  `SW_SHOWNORMAL` — the shell's launch path does not inherit the helper's
  `STARTUPINFO`, which is what made `start` open an invisible window — with a
  direct spawn as the fallback.

  `EDITOR_SKIP_LAUNCH=1` disables the package's own launch actions
  (`LaunchTargetPath` / `LaunchEditorPassive`), so an update has exactly one
  relauncher. They stay in the package for updaters that shipped before the
  property existed: those pass `/passive REBOOT=ReallySuppress` and no
  `EDITOR_SKIP_LAUNCH`, so the package launches the app for them, which is
  what rescued the hop into the first release that had the action. Silent
  (`/qn`, `UILevel = 2`) deploys still open nothing, and a double-clicked
  install opens the app only from the finish page's tick box.

  The helper is a copy of the exe, but it still lives in `%TEMP%`, so the
  outgoing build's uninstall step (a major upgrade removes the old product
  from *inside* the new install's transaction) can still delete the directory
  it is running from. Two things keep that from killing the update, and both
  are needed because an installed package cannot change what the build it
  replaces does: this package skips that call during an upgrade (`NOT
  UPGRADINGPRODUCTCODE`), and the payload sweep keeps anything younger than
  two minutes (`clean_payloads` in `src/update.rs`). (Measured on the 0.2.11
  → 0.2.12 hop with the old batch hand-off: the payload directory was gone
  after a `/passive` install and the batch never logged another line.)

  Two placements of that action were measured and rejected, and both failures
  are worth keeping in mind before moving it:

  * **Before `InstallFinalize`** (the removed `RestartEditor`): a child process
    inside the transaction, which is the scheduled restart this package once
    caused, plus a second instance on the extra execute pass of an upgrade.
  * **After `InstallFinalize` naming a `FileRef` or a `Directory`**: both fail
    there, because the transaction took the file costing and the directory's
    run-time state with it — `2753` ("the File 'EditorExe' is not marked for
    installation") and `1631`. What survives is a property, so
    `LaunchTargetPath` sets `EDITOR_LAUNCH` to `[INSTALLFOLDER]editor.exe` and
    the launch runs that.

  Windows Installer alone does *not* relaunch anything otherwise: Restart
  Manager closes the window whose exe it must replace and leaves it closed
  (measured: a `/qn` install over a running Editor ended with no Editor at
  all). That is why the update hand-off belongs to the helper, and why the
  package's own launch actions exist at all.

  Both user-facing dialogs carry `NoMinimize`: WiX writes the Dialog table's
  Minimize attribute by default (it is what a progress bar wants), and a
  dialog that keeps it can be shown as a minimized taskbar window — the report
  "the install options start minimized". The built table is
  `EditorWelcomeDlg`/`EditorDoneDlg` `attr=3` (visible, modal), not `7`.

  The uninstall sequence runs `editor.exe --uninstall` (a deferred custom
  action, `REMOVE="ALL"` and `NOT UPGRADINGPRODUCTCODE`) before it removes
  the files. That call removes the file associations the package wrote *and*,
  the part Windows Installer cannot see, the staged update downloads under
  `%TEMP%` (`editor-update-<tag>\` — the updater's own directory; the released
  0.2.1 predates this and leaves its msi there). `Return="ignore"`, so a
  locked or already-deleted exe never blocks the removal itself, and the sweep
  keeps payloads younger than two minutes, because the update in flight *is*
  one of them. A real uninstall still runs it; an upgrade does not, since the
  new package writes the same associations and any payload left behind is
  swept by the first launch of the new build. User data (`%APPDATA%\Editor`:
  settings, session, crash reports) is deliberately kept.

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
