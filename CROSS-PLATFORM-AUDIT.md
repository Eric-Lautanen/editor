# Cross-platform audit — Windows, Linux, macOS

Scope: every OS-specific path in the tree — the three backends, install/uninstall,
file associations, the updater, the network fetch, packaging and the release
pipeline — checked against what each OS requires in late 2026.

Method: read the code and packaging artifacts in full; researched the platform
requirements against primary sources (man pages, libX11 source, Microsoft Learn,
Apple/FireGiant documentation, GitHub runner docs); no OS was available to run
the GUI on, so **every finding below states whether it was read from source or
executed**, and the "Proof" line under each one is the check that would settle
it. Nothing in this audit has been applied to the code.

Read the severity table first, then the finding you care about. Rows are in
finding order, not severity order — the Severity column is the sort. The review
requests at the end are addressed to whoever picks up a finding, not to a
specific tool; "How to use this document" says how to work through all of them.

---

## Severity index

| # | Finding | OS | Severity | Status |
|---|---|---|---|---|
| F1 | Every `Ctrl`+letter shortcut is dropped by the X11 backend | Linux | **P0** | Read from libX11 source + code path; needs one probe |
| F2 | The macOS release artifact is Apple-silicon only | macOS | **P0** | Read from the runner label + missing `--target` |
| F3 | `.yaml` is not associated on any modern Linux distribution | Linux | **P0** | Read from shared-mime-info NEWS + RFC 9512 |
| F4 | Non-UTF-8 files (UTF-16, CP1252) are read as garbage and destroyed on save | all | **P1** | Read from both load paths |
| F5 | The macOS updater deletes the bundle before it can replace it | macOS | **P1** | Read from `install_macos` |
| F6 | X11 clipboard cannot carry a large copy (no `INCR`) | Linux | **P1** | Read from `on_selection_request` |
| F7 | The published signing policy described a pipeline that was not implemented | Windows | **P1** | Resolved — signing removed on the owner's call (2026-11-01) |
| F8 | The MSI is never built in CI | Windows | **P1** | Read from `ci.yml` |
| F9 | Saving a CRLF file rewrites every line ending | all | **P2** | Read from `Buffer::text` + `save` |
| F10 | A non-UTF-8 argument panics at startup on Unix | Linux/macOS | **P2** | Read from `main.rs` |
| F11 | X11 dead keys and Compose are silently discarded; no IME | Linux | **P2** | Read from Xlib docs + source |
| F12 | macOS has no main menu; activation call is deprecated | macOS | **P2** | Read from `macos.rs` |
| F13 | macOS trackpad scrolling double-applies inertia | macOS | **P2** | Read from `view_scroll_wheel` |
| F14 | Windows has no application manifest | Windows | **P2** | Read from `build.rs` |
| F15 | Windows CJK input gap is undocumented (same class as macOS/Linux) | Windows | **P2** | Read from the message loop |
| F16 | Linux publishes no `WM_CLASS`; desktop entry has no `StartupWMClass` | Linux | **P2** | Read from `linux.rs` + `.desktop` |
| F17 | `curl -L` escapes the download host allowlist | all | **P2** | Read from `net.rs` |
| F18 | No single-instance handoff on Linux; session file has no lock | Linux | **P2** | Read from `platform/mod.rs` |
| F19 | Font fallback is a hardcoded path list → tofu on other distros | all | **P3** | Read from `fallback.rs` |
| F20 | macOS: first click on an inactive window is swallowed; double restoration | macOS | **P3** | Read from `macos.rs` |
| F21 | Windows 11 Snap Layouts never appear | Windows | **P3** | Read from `WM_NCHITTEST` |
| F22 | `MSIDISABLERMRESTART=1` is a documented no-op in this package | Windows | **P3** | Read from Microsoft docs |
| F23 | Stale comments, a broken doc line, unexercised cross-checks, no appdata | all | **P3** | Read from the tree |
| F24 | `--install` / `--uninstall` error out off Windows while documented as portable | Linux/macOS | **P2** | Read from `install.rs` + `cli.rs` + README |
| F25 | Update consent needs a helper that may be absent; macOS uses a script host | Linux/macOS | **P2** | Read from `update::ask_install` |
| F26 | No ARM artifacts; the updater is inert on them | all | **P3** | Read from `asset_name` + the matrix |
| F27 | Preview selection copies *rendered* text; `Ctrl`+`A` is inert in Preview | all | **P2** | Requested behaviour, scoped against the code |

Accepted gaps already recorded in `docs/PLAN.md` (XDND drag-and-drop on Linux,
IME on macOS/Linux, single-instance on Linux, a real-wheel pass on macOS) are not
re-reported as findings; where a finding *extends* one of them, it says so.

---

## How to use this document

This is a review-and-fix brief, not a plan. Work through the findings in the
order under "Suggested order of work", and for each one:

1. **Confirm it first.** Every finding names the check that would settle it. If
   it does not reproduce, do not invent a fix — record what you observed under
   Revisions and move on. Three findings (F12, F13, F20) rest on Apple
   documentation rather than a running Mac, and F1 rests on libX11's source
   rather than a running X server; those are the ones to check before trusting.
2. **Fix it inside the constraint.** `[dependencies]` in `Cargo.toml` stays
   empty. Every fix here is std plus hand-written FFI; a fix that needs a crate
   is the wrong fix, and saying so is a valid outcome.
3. **Make the proof permanent.** Each finding has a "Proof" line. Prefer a test,
   a smoke-script assertion or a CI job over a command run once by hand — the
   suite is headless, so `cargo test` passing is never evidence that a backend
   works on its OS (that is how F1 stayed green).
4. **Record it.** Add a Revisions line: what changed, and what you measured. Keep
   the finding text — a reader needs to know what the defect was, not only that
   it is gone.
5. **One finding, one change, one proof.** Do not batch unrelated findings, and
   do not refactor around a fix.

Where a finding needs a decision rather than a fix (F24's wording, F26's
architecture coverage, and F27's word-selection rule — F7 and F27's
Split-precedence are resolved), ask before implementing. Policy choices for the
owner, not judgement calls for a reviewer.

The seven review assignments (R1–R7) group the findings by area. Use them as the
reading order for a single area, or as the split if several agents work in
parallel; the findings themselves are numbered independently so a partial pass is
still coherent.

---

## P0

### F1 — Every `Ctrl`+letter shortcut is dropped by the X11 backend

**Evidence.** `XLookupString` converts its *returned buffer* to an ASCII control
character whenever `ControlMask` is set; the returned keysym is untouched:

```c
if (mods & ControlMask) {
    if (n == 1) { buffer[0] = XkbToControl(buffer[0]); ... return 1; }
}
/* XkbToControl: if ((c >= '@' && c < '\177') || c == ' ') c &= 0x1F; */
```

(`libX11` `src/xkb/XKBBind.c`; `XLookupString(3)`: "the KeySym is mapped … (if the
Control modifier is on) to an ASCII control character".)

The only dispatch site is `src/platform/linux.rs` in `handle_event`, `KEY_PRESS`:

```rust
let n = XLookupString(kev, buf…, &mut keysym, null_mut());
if let Some(key) = key_of(keysym) { dispatch(KeyDown { key, mods }); }
else if n > 0 {
    if let Some(c) = text.chars().next() {
        if !c.is_control() {
            dispatch(KeyDown { key: Key::Char(c), mods });
            if !mods.ctrl && !mods.alt { dispatch(Event::Text) }
        }
    }
}
```

`key_of` maps only Backspace/Tab/Return/Escape/Home/End/arrows/PgUp/PgDn/Delete/
F1–F24. So `Ctrl+S` → `keysym == XK_s` → `None`; `buf == [0x13]` →
`'\u{13}'.is_control()` → **nothing is dispatched**.

**Why it matters.** Dead on X11: `Ctrl`+ **E O R W S A C X V Z Y F T Q** —
every letter chord in the legend. Survivors, for completeness: `Ctrl+Tab` and
`Ctrl+↑↓` (real keysyms, so `key_of` catches them), and `Ctrl`+punctuation or
digits outside `XkbToControl`'s ranges (`-`, `=`, `0`, `1`, `9`) — so
`Ctrl`+`-`, `Ctrl`+`+` (which is `Ctrl+Shift+'='` on a US layout) and `Ctrl+0`
still zoom. `Ctrl+2`…`Ctrl+8` are converted to terminal control codes too, but
the app binds none of them. The Linux build opens, renders, scrolls, drags — and
cannot save, open or find.

**Why the suite is green.** `every_probed_chord_changes_something` fires
`Event::KeyDown` straight into `App`, never through a backend.
`tools/linux-window-smoke.sh` only asks `xdotool` whether a window exists.

**Fix (std only).** Derive chords from **keysym + state**, never from the buffer.
This is not a new pattern — it is the one the other two backends already ship.
Windows' `key_of` has exactly the missing arm:

```rust
0x30..=0x39 | 0x41..=0x5A if mods.ctrl || mods.alt => {
    Key::Char((vk as u8 as char).to_ascii_lowercase())
}
VK_OEM_PLUS if mods.ctrl => Key::Char('+'),
```

and macOS derives the key from the keycode and the text from `characters`.
So: add printable Latin-1 keysyms to the X11 `key_of` (`0x20..=0x7e` and
`0xa0..=0xff` → `Key::Char`), keep the `is_control` guard on `buf`, and take
`Event::Text` only when `!ctrl && !alt`. No double-insert is possible:
`app::on_key` ends in `_ => return`, so a bare `Key::Char` inserts nothing — text
only ever arrives through `Event::Text`.

**Proof.** Under Xvfb: `xdotool type "hello"`, `xdotool key ctrl+s` on a known
file, assert the mtime changed. Add it to `linux-window-smoke.sh`, which already
has the display and the binary.

**Resolved while checking.** `app::on_key` matches `Key::Char('+' | '=')` for
zoom-in, precisely because a US layout produces `'='` with Shift, so the
punctuation chords keep working after the change and no dispatcher edit is
needed.

---

### F2 — The macOS release artifact is Apple-silicon only

**Evidence.** `.github/workflows/release.yml` builds on `macos-latest` with no
`--target`, so Cargo builds for the host: `aarch64-apple-darwin`. That label is
macOS 26 on **arm64** (GitHub's runner table; the rollover completed in July
2026). Meanwhile `packaging/macos/Info.plist` claims
`LSMinimumSystemVersion 11.0`, and `asset_name()` in `src/update.rs` matches
`("macos", _)` — arch-agnostic.

**Why it matters.** An Intel Mac cannot run an arm64 binary at all, and Intel
Macs are still in the supported matrix (macOS 26 runs on some, security-only).
The download table says "macOS", and the updater will hand an Intel user the
same zip.

**Fix (no crate).** Build both slices on the arm64 runner
(`rustup target add x86_64-apple-darwin aarch64-apple-darwin`,
`cargo build --release --target …` twice), `lipo -create` the two Mach-O files
into `Editor.app/Contents/MacOS/editor`, pin
`MACOSX_DEPLOYMENT_TARGET` identically for both slices, and raise
`LSMinimumSystemVersion` to match (12.0 is the honest floor; `x86_64-apple-darwin`
is Tier 2 since Rust 1.90 but still builds).

**Proof.** `lipo -info` prints `x86_64 arm64`; `file` reports a Mach-O for the
combined binary; CI compiles the x86_64 slice on every push (today nothing ever
compiles it).

---

### F3 — `.yaml` is not associated on any modern Linux distribution

**Evidence.** shared-mime-info 2.3 (Oct 2023) switched the canonical YAML type to
the IANA registered `application/yaml`; `text/x-yaml` became an `<alias>`
(RFC 9512, Feb 2024). `mimeapps.list` lookups key on the **primary** type. Both
places in this tree use the old spelling:

- `packaging/linux/editor.desktop`: `MimeType=…;text/x-yaml;…`
- `packaging/linux/install.sh`: `xdg-mime default editor.desktop … text/x-yaml …`

`application/toml`, `text/csv`, `text/markdown` and `application/json` are still
correct. `application/x-ndjson` does not appear in the shared database (see the
caveat under "Unverified" — the file list is large and was read in part), so the
package's own definition of it is the right call and should stay.

**Why it matters.** README promises Editor becomes the default handler for YAML;
on Ubuntu 24.04+, Fedora 39+ and anything else with shared-mime-info ≥ 2.3 it
does not, silently.

**Fix.** Declare and default **both** `application/yaml` and `text/x-yaml`.

**Proof.** Extend the `packaging-linux` CI job (which already installs into a
throwaway `HOME`): `xdg-mime query default application/yaml` must name
`editor.desktop`.

---

## P1

### F4 — Non-UTF-8 files are read as garbage and destroyed on save

**Evidence.** Both load paths decode with `String::from_utf8_lossy` and nothing
else: `src/load.rs` (`load`, whole file) and `Loader::drain` (streamed). No BOM
check, no UTF-16 branch, no encoding record anywhere in the tree. `save` writes
`buffer.text()` straight back.

**Why it matters.** The affected files are in the app's own association list:

- **UTF-16LE** — Notepad's "Unicode", and `>` redirection in **Windows
  PowerShell 5.1**, which is the default `.txt`/`.log` producer on a stock
  Windows box. Decodes to `h\0e\0l\0l\0o\0…`: NULs in the preview.
- **CP1252/Latin-1** `.ini`/`.csv`/`.log` — every byte ≥ 0x80 becomes U+FFFD.

One edit and one save writes that lossy decode back over the original bytes. That
is silent data loss on an ordinary action.

**Fix (std only).** Sniff on open: `EF BB BF` (UTF-8, strip), `FF FE`/`FE FF`
(UTF-16LE/BE → `String::from_utf16_lossy` over `u16::from_le_bytes`/
`from_be_bytes`), else UTF-8 plus a "not valid UTF-8" flag. Store
`(encoding, valid)` on the tab; on save re-encode in kind
(`str::encode_utf16`). For an unknown encoding, refuse to save and say so in the
status bar instead of rewriting the file.

**Proof.** Two fixtures in `samples/` (one CRLF, one UTF-16LE with a BOM), a
round-trip test per encoding, and a test that a lossy tab refuses to save.

---

### F5 — The macOS updater deletes the bundle before it can replace it

**Evidence.** `src/update.rs`, `install_macos`:

```rust
std::fs::remove_dir_all(&bundle)?;
std::fs::rename(&fresh, &bundle)?;
```

**Why it matters.** `std::fs::rename` on macOS is `rename(2)`: same filesystem
only, `EXDEV` across volumes, no std fallback. The staging dir is `$TMPDIR`,
which is *usually* on the boot volume and so usually the same volume as
`/Applications` — but not when the app lives on an external disk, a mounted
image, or a second volume. In that window the old bundle is gone and the new one
cannot land. There is also no rollback if anything after the delete fails, and
the delete itself is not all-or-nothing: `remove_dir_all` that fails partway
(permissions, an immutable file, a volume going away) leaves a *partially deleted*
bundle, and the next launch has no app to start.

The Linux path deliberately avoids this: it copies to a sidecar **next to the
target** first, then renames within that directory (`install_linux`).

**Fix (std only).** Stage inside the destination's parent directory (so the final
move is same-volume by construction), then swap:
`rename(bundle → bundle.old)`, `rename(fresh → bundle)`, remove `.old`;
restore `.old` on failure. Check writability before touching anything.

**Proof.** Unit-test the swap against a staging dir on a different filesystem
(`/dev/shm` on Linux, a small `hdiutil` volume on macOS) so `rename` fails
deterministically, and assert the bundle is intact afterwards.

---

### F6 — X11 clipboard cannot carry a large copy

**Evidence.** `src/platform/linux.rs`, `on_selection_request` answers a
`SelectionRequest` with one `XChangeProperty` of the whole string. No `INCR`
transfer, no `TIMESTAMP`/`MULTIPLE` targets.

**Why it matters.** An X protocol request is capped at 262 140 bytes (16 MiB with
BIG-REQUESTS). This app advertises multi-megabyte files; a multi-megabyte
selection is rejected by the server, Xlib prints the error, and the paste gets
nothing with no explanation.

**Fix (std only).** Implement the `INCR` handshake: announce the total size,
write 64 KiB-ish chunks on `PropertyNotify` until done. Alternatively cap and
tell the user, which is worse but honest.

**Proof.** A headless Xvfb test that copies ~8 MB and pastes it through a second
client.

---

### F7 — The published signing policy described a pipeline that was not implemented

**Resolved (owner, 2026-11-01).** The README described a signing pipeline the
release workflow did not run. The owner's call was to take signing out of the
project entirely: the release workflow carries no signing steps for any OS, the
signing guides and certificate secrets are gone, the README documents no
signing policy, and the binaries a release publishes are verified by the
`SHA256SUMS` manifest the updater checks before any install.

**Proof.** One reading of README, `release.yml` and `packaging/` describes the
pipeline, and the README's macOS install section states the unsigned-bundle
behavior a user actually meets (Gatekeeper's first-launch refusal). The
inbox-persistent check: `grep -ri` finds no signing policy claims in the tree.

---

### F8 — The MSI is never built in CI

**Evidence.** `packaging/windows/editor.wxs` is compiled only in `release.yml`,
on a tag. `ci.yml` has `packaging-linux` and `packaging-macos` jobs and no
Windows equivalent.

**Why it matters.** The `.wxs` is the riskiest XML in the repository — major-upgrade
rows written by hand, custom-action sequencing, `PerUserProgramFilesFolder`,
dialog tables. A schema error, a renamed component or a missing `Icon` source is
discovered *after* tagging, in the one build users install. WiX v7 also enforces
EULA acceptance (new in v7; v6 had no EULA id), which the workflow does handle
via `-acceptEula wix7` — exactly the sort of thing that only breaks on a tag.

`PerUserProgramFilesFolder` is not in WiX's documented standard-directory list;
the code comment says it was verified the hard way (error 1303 otherwise). Keep
it, but turn the empirical result into a check.

**Fix.** A `packaging-windows` job: install WiX, build unsigned with
`-d Version=0.0.0`, assert the payload, the ARP properties and both dialog tables
exist. No signing, no publish.

---

## P2

### F9 — Saving a CRLF file rewrites every line ending

**Evidence.** `Buffer::from_text` strips `\r`; `Buffer::text()` joins on `\n`;
`save` writes that. The dirty check *does* normalise (`same_lines` trims `\r` on
the saved side), so there is no spurious "unsaved changes" — the write path is
the only place the original convention is lost.

**Why it matters.** Any CRLF file becomes LF on save: a whole-file diff in git,
and a change to files that legacy Windows tooling expects in CRLF. This
repository's own `README.md`, `packaging/windows/README.md` and `editor.wxs` are
CRLF, so opening one and saving it rewrites every line.

**Fix (std only).** Record the dominant EOL on load (count `\r\n` versus bare
`\n` in the head, remember "mixed"), store it per tab, re-apply on save. Same
tab-level record as F4 — do them together.

---

### F10 — A non-UTF-8 argument panics at startup on Unix

**Evidence.** `src/main.rs`: `cli::run(std::env::args().skip(1).collect())`.
`std::env::args()` panics on invalid UTF-8; Unix argv is arbitrary bytes.

**Why it matters.** A filename that is not valid UTF-8 is legal on ext4 and
common from archive extraction; `editor <that file>` crashes before the window
exists. Related: the session file writes paths with `Path::display()`
(`src/settings.rs`), which is lossy, so a non-UTF-8 path in `session.txt` comes
back as a different, unopenable path.

**Fix (std only).** `args_os()` → keep `OsString`, `to_str()` only for flag
matching, `PathBuf` for operands. Windows is unaffected (argv is UTF-16).

---

### F11 — X11 dead keys and Compose are silently discarded; no IME

**Evidence.** `XLookupString` performs no compose processing — `(3)` says its
status argument is implementation-private and "a portable program must pass
NULL", and the source ignores it. So on a US layout `dead_acute` then `e`
produces `XK_dead_acute` (0 bytes) then `'e'`: you get `e` and the accent is
lost. Compose sequences likewise. AltGr / level-3 **does** work (XKB resolves the
level from the group), so Polish `ą` and German AltGr-`@` are fine.

This extends the "IME/XIM pending" line already in `docs/PLAN.md`: it is not only
CJK, it is every European dead-key and Compose sequence.

**Fix (std-only FFI).** The XIM *library model* handles compose and dead keys
in-process with no ibus/fcitx running, and upgrades to CJK when one is. Ten
symbols:

```
XSetLocaleModifiers("")                       /* "" not "@im=none" — see below */
XOpenIM(dpy, NULL, NULL, NULL)
XGetIMValues(im, XNQueryInputStyle, &styles)  /* XCreateIC fails on an unoffered style */
XCreateIC(im, XNInputStyle, XIMPreeditNothing|XIMStatusNothing,
              XNClientWindow, win, XNFocusWindow, win)
XGetICValues(ic, XNFilterEvents, &mask)       /* OR into XSelectInput, after the IC exists */
FocusIn → XSetICFocus / FocusOut → XUnsetICFocus
every event:  if (XFilterEvent(&ev, win)) continue;
KeyPress:     Xutf8LookupString(ic, …) for text; XLookupString when ic == NULL
```

Three documented traps: `XNFilterEvents` is read-only (passing it to
`XSetICValues` fails) and skipping it breaks Backspace/Enter/arrows inside
preedit; `"@im=none"` **overrides the user's `XMODIFIERS`** and disables
ibus/fcitx (use `""`); and `XRegisterIMInstantiateCallback` is how an IM that
starts after the app is picked up. XIM still works under XWayland as long as
`XMODIFIERS` is set.

### F12 — macOS has no main menu; the activation call is deprecated

**Evidence.** No `NSMenu`/`setMainMenu` anywhere in `src/platform/macos.rs`, and
`activateIgnoringOtherApps:` at the `app_did_finish_launching` site.

**Why it matters.** The app's own key map covers `Cmd`+letter by mapping Command
onto its Ctrl chords (pinned by `command_maps_to_the_apps_ctrl_chords`), so
nothing is dead — but there is no Services menu, no Edit menu (so no system
Character Viewer, spelling, or `validateUserInterfaceItem:` path), no Window
menu, and no standard About/Hide items. Separately, `activateIgnoringOtherApps:`
was deprecated in macOS 14: activation is now cooperative, so it no longer
guarantees the window comes forward — which matters for the open-file path.

**Fix.** A minimal `NSMenu` (App: About/Hide/Quit; Edit: Undo/Redo/Cut/Copy/Paste/
Select All; Window: Minimize/Zoom) set as `NSApp.mainMenu`, and `activate()` +
`makeKeyAndOrderFront:` instead of the deprecated call.

### F13 — macOS trackpad scrolling double-applies inertia

**Evidence.** `view_scroll_wheel` reads `deltaY` for both modes and never checks
`phase` or `momentumPhase`. The app implements its own glide; AppKit delivers
system-generated momentum events for trackpads, so each momentum event starts
another glide. `macos.rs`'s own header already admits "the scroll
direction/multipliers want a real-wheel pass".

**Fix.** Use `scrollingDeltaY` when `hasPreciseScrollingDeltas` is true, ignore
or absorb `momentumPhase != []`, cancel the coast on `phase == .began`, and honour
`isDirectionInvertedFromDevice`. Gate the app's glide on
`hasPreciseScrollingDeltas == false` so it only runs for real wheels.

### F14 — Windows has no application manifest

**Evidence.** `build.rs` embeds icon and `VS_VERSIONINFO` only. `GetVersionEx`
therefore reports 6.2; there is no `<supportedOS>`, no `<trustInfo>`, no
`<dpiAwareness>`; DPI is set at runtime by
`SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`.

**Why it matters.** Nothing in `src/` branches on OS version, so today this costs
only a lie waiting to be used — but Microsoft recommends the manifest over the
API call, and the runtime call is a hard import requiring Windows 10 1703+.

**Fix (no crate, no new link flag).** Extend the `.res` writer that already exists
in `build.rs` with `RT_MANIFEST = 24`, **ID 1** (1 = EXE, 2 = DLL), reusing the
same `entry()` helper and header layout the icon and version resources already
use. Contents that buy something: single `<supportedOS
Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>` (one GUID covers Windows 10 *and*
11), `<dpiAwareness>PerMonitorV2, PerMonitor</dpiAwareness>`, and `<trustInfo>`
with `asInvoker`.

Prefer this over `/MANIFEST:EMBED` + `/MANIFESTINPUT`, because `/MANIFESTINPUT`
is documented as capped at MAX_PATH (a `target/debug/build/editor-<hash>/out/…`
path can exceed it), it *merges* with a linker-generated manifest that injects
its own `asInvoker` fragment unless `/MANIFESTUAC:NO` is also passed, and it
writes a stray `<exe>.manifest` next to the binary.

Consequence to respect: once the manifest exists, the runtime
`SetProcessDpiAwarenessContext` call returns `ERROR_ACCESS_DENIED`. Its result is
discarded today, so that is harmless — keep it discarded, or delete the call.

Skip `<longPathAware>` (inert without the machine-wide `LongPathsEnabled`
registry value) and `<activeCodePage>UTF-8` (changes the ACP process-wide for
`*A` APIs; std uses the W APIs, so it is pure risk).

### F15 — Windows CJK input gap is undocumented

**Evidence.** The message loop handles `WM_CHAR` and never
`WM_IME_STARTCOMPOSITION` / `WM_IME_COMPOSITION` / `WM_IME_SETCONTEXT`, and never
calls `ImmSetCompositionWindow`/`ImmSetCandidateWindow`.

**Why it matters.** Microsoft's documented fallback is that `DefWindowProc`
turns committed IME results into `WM_CHAR`, so **text does arrive** — what is
lost is in-buffer preedit, backspace-to-edit-composition, reconversion, and the
composition/candidate window appears at the window origin instead of the caret.
`docs/PLAN.md` lists IME as pending for macOS and Linux only, and the release
notes imply Windows is fine. Say what is actually true.

**Fix.** Document now; fix alongside F11 as one cross-platform "text input"
package. On Windows: handle `WM_IME_STARTCOMPOSITION`, read the composition with
`ImmGetCompositionString(GCS_COMPSTR/GCS_CURSORPOS)` and the commit with
`GCS_RESULTSTR`, and position with `ImmSetCompositionWindow` +
`ImmSetCandidateWindow` on every caret move.

### F16 — Linux publishes no `WM_CLASS`; desktop entry has no `StartupWMClass`

**Evidence.** No `XSetClassHint` in `src/platform/linux.rs`; no `StartupWMClass`
in `packaging/linux/editor.desktop`. Also absent: `_NET_WM_PID` and
`WM_NORMAL_HINTS` (the WM does not know a minimum size, though the app enforces
one itself).

**Why it matters.** GNOME Shell and KDE match a window to a `.desktop` file
primarily by `WM_CLASS`; without it association falls back to heuristics, so the
dock can show a generic icon and the window may not group or pin. The smoke test
greps only the window *title*, so it cannot notice.

**Fix.** `XSetClassHint` with res_name `editor`, res_class `Editor`; mirror as
`StartupWMClass=Editor`; add `TryExec`/`Keywords`/`GenericName` while there.
Assert the class in `tools/linux-window-smoke.sh`.

### F17 — `curl -L` escapes the download host allowlist

**Evidence.** `net::trusted()` correctly pins the initial URL (and its tests
cover credential smuggling, subdomain spoofing and `file://`). Every curl rung
passes `-L`, which follows redirects to **any host and any scheme**, so the
module's claim that a spoofed API response "cannot turn a fetch into a plaintext
download" is stronger than the code.

**Fix.** `--proto-redir =https --max-redirs 3` (and `--proto =https`) on both curl
rungs. wget and the python rung follow redirects with no equivalent switch — note
that in the module comment rather than rewriting them, since the trust anchor is
the initial TLS request to the GitHub API.

### F18 — No single-instance handoff on Linux; the session file has no lock

**Evidence.** `src/platform/mod.rs`, `forward_to_running`: only the Win32 arm
does anything; the comment still says the other backends will do it "once their
GUI (M8/M9) lands". `settings.rs` writes `session.txt` with no lock.

**Why it matters.** README is accurate ("Windows today"), but on Linux two
windows both persist the session, last writer wins, so an open set of tabs can be
silently lost. macOS gets `application:openFile:` from Finder, but a terminal
launch still starts a second process.

**Fix.** Pure std, no FFI: `File::lock` / `try_lock` / `lock_shared` were
stabilised in Rust 1.89 (August 2025), so a `try_lock` on the session file at
startup is enough to detect a second instance and to serialise the write. A
D-Bus name, or a socket in `$XDG_RUNTIME_DIR`, is what adds the actual handoff
(argv forwarding) on Linux.

### F27 — Preview selection copies rendered text, and `Ctrl`+`A` is inert in Preview

*Resolved (owner).* Preview copy now slices the buffer for the source lines the
selected rows were rendered from (a row's document-space `top` names its unit in
the pane's own plan — a buffer line, a CSV/JSONL record, a JSON node, a Markdown
block — and the unit names its source lines), joined with the tab's line ending.
`Ctrl`+`A` selects the whole range, or copies the whole file where a banded pane
keeps no rows. A double-clicked word takes its whole source line: lossless where
the renderer's token is not. The six steps and the proofs below are the record of
what was wrong, kept for the reasoning.

*Recorded as a behaviour requirement rather than a discovered defect, and scoped
against the code so the estimate is real. It is app-layer, so it behaves
identically on all three OSes — the backends are not involved.*

**The requirement.** In Preview, `Ctrl`+`A` should select everything and paint
the highlight the way Edit does, and copying a preview selection — the drag
selection or a double-clicked word — should put the file's **raw** text on the
clipboard, not the rendered text.

**What happens today.**

| Action | Behaviour | Evidence |
|---|---|---|
| `Ctrl`+`A` in Preview | nothing at all | `on_key`: `Key::Char('a') if mods.ctrl && self.mode != Mode::Preview` |
| drag in Preview | paints a row-granular highlight | `preview_sel: Option<(usize, usize)>` |
| double-click a word | paints and copies the word | `preview_word: WordSel` |
| `Ctrl`+`C` in Preview | copies the **rendered** rows/word | `preview_selection_text()` joins `preview_rows[lo..=hi].text` |
| right-click → Copy all | copies `buffer.text()` — raw, and correct | `Action::CopyAll` |
| right-click menu in Preview | offers *Copy all* only — no Copy, no Select all | `ui::menu`, the `can_edit == false` item list |
| `Action::Copy` (Edit menu) | never consults the preview selection | it calls only `editor.copy` |

**Why it matters.** Paste-from-preview is not a faithful copy of the file, which
is the one thing "copy" should mean in an editor built for reading files you
intend to edit:

- **Markdown** — `**bold**` arrives as `bold`, a heading loses its `#`, a code
  fence loses its fences and gains a language label the file does not contain, a
  table row arrives as rendered cells rather than the `|`-delimited record.
- **JSON** — the `▼`/`▶` markers, the commas and the folded field counts come
  along, and a collapsed subtree is silently absent from the copy.
- **CSV** — what arrives is the aligned column rendering, not the record line.

And `Ctrl`+`A` doing nothing in Preview is the visible half of the same gap: the
mode where copying the whole file is most useful has no select-all.

**Why it is not a one-line change.** `ui::preview_view::Row` carries only
`baseline`, `top`, `bottom`, `x0`, `x1`, `text` — there is no source line or byte
range anywhere in it, so a selection cannot currently be mapped back to the file.
The mapping has to be added at emission:

- **Markdown / text / logs** — rows are emitted per source line; tag each row
  with the source line range it came from as it is built.
- **JSON** — the tree already indexes nodes by source offset, so a row maps to
  its node's range.
- **CSV** — the parser knows each record's source line.

**Design sketch.**

1. Add a source span to each emitted row (or to the `DrawOp::Text` it is grouped
   from) — the only structural change; everything else is plumbing.
2. `preview_selection_text()` becomes `preview_selection_source(&buffer)`: slice
   the buffer's lines for the selected range, joined with the file's own line
   ending (F9), so a copy from Preview and a save agree byte for byte.
3. `Ctrl`+`A` in Preview sets `preview_sel` to the whole row range instead of
   being gated out of the arm; the painter already handles an arbitrary range.
4. Decide, explicitly, what a **double-clicked word** copies: the source token
   (ideal — `**bold**`, not `bold`) or the whole source line (simple, lossless,
   slightly over-large). Do not leave it as "whatever the renderer drew".
5. Decide whether the Preview menu should gain **Copy** and **Select all** once
   they mean something there. Today that menu lists Copy all, Open, Save, Close
   tab, Help and About only, so a preview selection is keyboard-only — which is
   fine as long as it is deliberate.
6. In **Split**, both selections can be live and `Ctrl`+`C` currently prefers the
   preview (`preview_selection_text()` is tried first). Pick a rule — last-clicked
   pane wins is the conventional one — and test it, because today a stale preview
   drag can beat a fresh editor selection.

**Proof.** Four tests, all headless through `NullHost`: select rows over a
`**bold**` line and assert the clipboard equals the source slice; `Ctrl`+`A` then
`Ctrl`+`C` in Preview equals the whole file including its line endings; a
selection over a collapsed JSON node copies the node's source lines; and in Split
the editor selection wins when the last press was in the editor.

### F24 — `--install` / `--uninstall` error out off Windows while documented as portable

**Evidence.** `src/platform/install.rs`: both `install()` and `uninstall()` return
`Err(Error::Platform("use the scripts in packaging/ on this OS"))` for every
non-Windows build. `cli.rs` prints
`--install | --uninstall   register/remove file associations (portable use)`,
and the README's console-flags table lists the pair with no OS caveat.

**Why it matters.** On Linux `editor --install` exits 1 with a message naming a
script, and `--uninstall` cleans staged update payloads *first* and then returns
the same error — so the flag half-works and reports failure. On macOS the
message points at `packaging/macos/bundle.sh`, which *builds* the bundle rather
than registering it, so the advice is wrong there.

**Fix (std only).** Word the usage per OS, and make the behaviour match it: on
macOS say registration is the bundle itself (nothing to install) and exit 0; on
Linux point at `install.sh` for associations but let `--uninstall` exit 0 after
the payload sweep it already performs.

### F25 — Update consent depends on a helper that may be absent; macOS uses a script host

**Evidence.** `update::ask_install` has three shapes: a `MessageBoxW` owned by the
main window on Windows; `osascript` + `display dialog` on macOS; and, on Linux,
`zenity --question` then `kdialog --yesno`, with
`ask_with("kdialog", …).unwrap_or(false)` when neither is installed.

**Why it matters.** On a minimal Linux — a tiling-WM session, a container, a box
without the GNOME/KDE dialog helpers — the background check finds an update and
then silently does nothing, every time, with nothing in `last-update.log` to
explain it. `--update` still works (it skips consent), so the failure is
invisible to anyone who tests the CLI path. On macOS `osascript` is a script
host, which is exactly the shape the Windows path was rewritten to eliminate,
and its dialog is not owned by the app, so it can open behind the window.

**Fix (std only).** Linux: try `/dev/tty` with a plain y/N prompt before giving
up, and always `log_step` the reason a prompt was skipped. macOS: a native
`NSAlert` through the existing objc helpers — a dozen messages, and the last
script host in the updater is gone.

---

## P3

### F19 — Font fallback is a hardcoded path list

`src/gfx/font/fallback.rs` probes a handful of Debian/Fedora-shaped paths
(`/usr/share/fonts/truetype/dejavu`, `/usr/share/fonts/noto`, …) plus bare Windows
basenames. On Arch, openSUSE, NixOS and anything with a different prefix, CJK and
emoji render as tofu. Std-only improvement: on Linux run
`fc-match -f %{file} :charset=<codepoint>` as a child process; on Windows
`std::fs::read_dir(%WINDIR%\Fonts)` to resolve basenames; keep the current list as
the last resort.

### F20 — macOS: swallowed first click, and double restoration

No `acceptsFirstMouse:` override, so the first click on an inactive window is
consumed by activation — clicking a tab or a word in an unfocused Editor does
nothing until the second press. And the window is never marked
`setRestorable:NO`, so AppKit's own restoration can race the session file that
already restores tabs, mode and geometry. Both are one message each.

Not a finding: the `applicationSupportsSecureRestorableState:` warning that
plagues hand-rolled AppKit apps is auto-silenced for anything linked on the
macOS 14+ SDK, which this is.

### F21 — Windows 11 Snap Layouts never appear

`WM_NCHITTEST` returns `HTCLIENT` for everything app-drawn, so the
maximize-hover flyout cannot open. Return `HTMAXBUTTON` for the maximize
button's rect and handle `WM_NCLBUTTONDOWN` there. Convention, not a bug.

### F22 — `MSIDISABLERMRESTART=1` is a no-op in this package

`MSIRESTARTMANAGERCONTROL=Disable` disables Restart Manager, and
`MSIDISABLERMRESTART` is documented as ignored when RM is unavailable *or
disabled*. Harmless — but `editor.wxs` credits it with work it does not do. Fix
the comment; do not change the flags. Every one of them has a measured failure
behind it (see `packaging/windows/README.md`), and the helper already waits for
the old process to exit, so the RM path is dormant rather than wrong.

### F23 — Documentation and cross-check drift

- `src/platform/macos.rs` module doc has a broken line join:
  `` `defaults read -g//! AppleInterfaceStyle` ``.
- `src/platform/mod.rs` still says "macOS open/save dialogs arrive with M9" and
  "once their GUI (M8/M9) lands".
- The `editor_check_win|linux|macos` cfgs are declared in `Cargo.toml` and wired
  through `platform/mod.rs`, but **no CI job sets them**, so the cross-check
  apparatus is unexercised and will rot.
- Release notes still say the missing pieces are "XDND … IME", which understates
  F1, F2, F3, F9, F11 and F16.
- No `appdata/metainfo.xml`, so the app never appears in GNOME Software or KDE
  Discover (cosmetic, optional).
- Download table should name the architecture it ships (F2) and date Windows 10:
  22H2 reached end of servicing 14 Oct 2025, consumer ESU runs to 12 Oct 2027.

### F26 — No ARM artifacts, and the updater is inert on them

**Evidence.** `update::asset_name` is
`("windows", "x86_64") → msi`, `("linux", "x86_64") → tar.gz`,
`("macos", _) → zip`, otherwise `None`. `release.yml` builds one runner per OS
with no `--target`.

**Why it matters.** Linux on aarch64 (Asahi, Snapdragon laptops, Raspberry Pi 5
desktops, Graviton developer boxes) gets no artifact at all, and on such a build
the updater silently does nothing because `asset_name` is `None`. Windows on ARM
runs the x64 build under emulation — usable, untested. macOS is arm64-only today
(F2) and its artifact name does not say so.

**Fix.** Make the coverage explicit rather than implicit. `aarch64-unknown-linux-gnu`
is one more matrix leg and one more `asset_name` arm, but it is the single place
where a *system* dependency is genuinely unavoidable: linking `libX11` for a
foreign target needs a cross toolchain (the std-only rule still holds for the
code). Whatever is decided, the download table should name an architecture per
row.

---

## What is already in good order — do not "fix" these

Reviewers will be tempted to touch these; they are deliberate and tested.

- **`net.rs`'s rung design.** No script hosts on Windows (urlmon in-process,
  `curl.exe` only as a fallback), a host allowlist with tests for spoofing, and a
  test asserting a *failed* rung cannot burn the next one. F17 is a one-flag
  hardening, not a redesign.
- **The Windows update hand-off.** Staged copy of the exe, approval token, cancel
  token, file-stamp wait, `EDITOR_SKIP_LAUNCH`; every rejected alternative is
  recorded with the measurement that rejected it. Do not simplify it.
- **Session persistence.** Temp file + `sync_all` + rename, with tests for an
  interrupted write and an unwritable target.
- **The streamed loader's `scratch` carry.** Bytes that end inside a multi-byte
  UTF-8 sequence are re-fed to the next read, so chunk boundaries cannot produce
  U+FFFD. This is the classic bug in this kind of code and it is not here.
- **Dirty tracking** normalises CRLF on the compare side (`same_lines`), so a
  freshly opened CRLF file is not "dirty". Only the write path (F9) lost the
  convention.
- **Manifest drift is test-pinned.** `core::file_kind` reads both the `.wxs` and
  the `Info.plist` and fails if either disagrees with `EXTENSIONS`.
- **`WM_DPICHANGED`** adopts the OS-suggested rect, which is the part most apps
  get wrong (it prevents the recursive-DPI-change cycle).
- **Shared restore logic** (`open_at` / `title_reachable`) is unit-tested against
  multi-monitor and negative-origin geometry instead of being copied per backend.
- **`install.sh`** is exercised in CI against a throwaway `HOME`, including the
  literal `$HOME` in `XDG_DESKTOP_DIR` and the opt-out switch.

---

## Suggested order of work

Do not batch these; the first one changes what "works on Linux" means.

1. **F1** — Linux chords. Nothing else on Linux is observable until the app can
   save. One backend change plus one smoke assertion.
2. **F2, F3** — release-level breakage; both are pipeline/packaging edits with a
   check that fails today.
3. **F4 + F9** — one change, one tab-level `(encoding, eol)` record, one pair of
   fixtures. Splitting them means touching the load/save seam twice.
4. **F5, F8** — the two "discovered only in production" failures.
5. **F11 + F15** — one cross-platform text-input package (XIM, Win32 IMM, and
   macOS `NSTextInputClient`), with a single acceptance test: type Japanese and
   type `é` via dead keys, in a tab, on each OS.
6. **F12, F13, F20, F21** — platform conventions, in one macOS + one Windows pass.
7. **F14, F17, F19** — hardening and portability.
8. **F27** — preview selection and `Ctrl`+`A`. App-layer, so it is one change for
   all three OSes; it is the only finding here that adds a data path (row →
   source) rather than correcting an existing one, which is why it is scheduled
   after the correctness fixes rather than with them.
9. **F24, F25, F26** — make the messages and the coverage honest: a flag that
   cannot work off Windows should say so, a skipped update prompt should be
   logged, and an unsupported architecture should be named.
10. **F23** — documentation truth pass last, so it describes the final state.

---

## Review requests

Each block is a self-contained assignment: read these files, answer these
questions, prove it this way. Findings that are already conclusive are marked
**confirm**, ones that need a decision are marked **decide**.

### R1 — X11 / Linux backend reviewer

Read `src/platform/linux.rs` (`handle_event`, `key_of`, `on_selection_request`,
the window-creation block), `src/ui/keymap.rs`, `tools/linux-window-smoke.sh`.

1. **Confirm F1.** Run the release binary under Xvfb, `xdotool type "hello"`,
   then `xdotool key ctrl+s` on a known path; assert the file's mtime changed.
   Then `ctrl+e`, `ctrl+f`, `ctrl+w` and check the app reacts. If any of them
   *does* work, find out why before changing anything — the analysis says the
   only dispatch site drops the event.
2. **Confirm F6.** Copy a >8 MB selection and paste it into another X client.
   Does the transfer fail silently, and does Xlib log an error?
3. **Confirm F11** with `dead_acute`+`e` (US layout, `XKB_DEFAULT_LAYOUT=us`) —
   expect `e`, not `é` — and then decide the XIM scope: library model only, or
   full `@im=` negotiation.
4. **Confirm F16** with `xprop WM_CLASS` on the running window (expect "not
   found") and `xprop WM_NORMAL_HINTS`.
5. Decide whether the `XSetSelectionOwner(XA_PRIMARY, …)` on every explicit copy
   (not just on selection) is intentional.

Do not: change `WHEEL_LINES_PER_NOTCH`, the `_MOTIF_WM_HINTS` decoration
stripping, or the manual move/resize drag, without re-reading why they exist.

### R2 — Windows / Win32 and MSI reviewer

Read `src/platform/windows.rs` (message loop, `WM_NCCALCSIZE`, `WM_NCHITTEST`,
DPI init), `src/platform/install.rs`, `packaging/windows/editor.wxs`,
`packaging/windows/README.md`, `build.rs`, `.github/workflows/ci.yml`.

1. **Resolved F7.** The owner took signing out of the repository entirely; the
   assignments left are F8/F14 and the F24 wording below.
2. **Confirm F8** by adding the MSI build to CI and deliberately breaking the
   `.wxs`: does the job go red?
3. **Confirm F14** by adding the manifest to the existing `.res` writer and
   checking with `sigcheck -m` or `mt.exe -inputresource:editor.exe;#1` that it
   landed, and that DPI still resolves to PerMonitorV2.
4. **Confirm F15** with a Japanese IME on Windows 11: text should commit, the
   candidate window should appear at the window origin (not the caret), and no
   preedit should be visible in the buffer.
5. **Decide F21** — is Snap Layouts worth the `HTMAXBUTTON` work on a custom
   title bar that already draws its own maximize button?
6. Check one thing not raised above: the `.wxs` `Manufacturer="Eric-Lautanen"`
   is what `ARP` shows, alongside the resource version stamp `build.rs` writes.
   Verify the strings agree.
7. **Confirm F24**: run `editor --install` and `editor --uninstall` on this OS
   (they are correct here) and read what the non-Windows arms print. The file is
   Windows-first, so the owner of it should decide the wording and exit codes
   for the other two rather than leaving them to whoever touches it next.

### R3 — macOS / AppKit reviewer

Read `src/platform/macos.rs` (callback registration, `run`, `view_scroll_wheel`,
`install` sites), `packaging/macos/Info.plist`, `packaging/macos/bundle.sh`,
`src/update.rs` (`install_macos`), `.github/workflows/release.yml`.

1. **Confirm F2** with `lipo -info` on a built artifact, and on an Intel Mac (or
   a VM) try to launch it. Then implement the universal build and check the
   combined bundle is a real Mach-O that loads (`file`, `otool -h`).
2. **Confirm F5** by pointing the updater at a bundle on a second volume; assert
   the bundle survives a failed `rename`.
3. **Confirm F13** on a trackpad: a single flick should coast once. If it coasts
   twice, F13 is real; if not, find out what already absorbs AppKit's momentum.
4. **Confirm F12** by checking the menu bar under the app: is it empty, and does
   `Cmd+Q` still quit (it should, via the app's own key map)?
5. **Confirm F20** by clicking an inactive Editor window once: does the click
   register or only activate?
6. Decide whether `install_macos` should also verify the new bundle's signature
   after the swap, since a half-copied bundle would otherwise be launched.

Do not: replace the `CGImageCreate` + `CGContextDrawImage` blit with a layer-backed
view, or move drawing off the main thread; the flipped-view blit and the
`objc_msgSend` stret split for x86_64 are load-bearing.

### R4 — Text, encoding and buffer reviewer

Read `src/load.rs`, `src/core/buffer.rs`, `src/app/mod.rs` (`save`, `sync_dirty`,
`same_lines`), `src/app/tab.rs`, `src/settings.rs`, `src/main.rs`.

1. **Confirm F4** with a UTF-16LE fixture (with and without BOM) and a CP1252
   fixture: what does the preview show, and what does a save write back?
2. **Confirm F9** by opening a CRLF file, typing one character and saving:
   `git diff` should show every line changed. Then check the dirty flag was not
   set on open (it should not be — that part is correct).
3. **Confirm F10** with a filename containing `\xff` on ext4:
   `./target/release/editor $'\xff.md'`.
4. Design the tab-level record for F4+F9 together: field shape, where it lives
   (`Tab`), how `saved_text` and `same_lines` interact with it, and what happens
   when a file's convention changes on disk while it is open (the reload path).
5. Check whether `Buffer::from_text`'s CRLF trim is relied on anywhere else
   (it is asserted by an existing test), so the EOL record does not contradict a
   documented invariant.

### R5 — Release and packaging reviewer

Read both workflows, `packaging/macos/bundle.sh`,
`packaging/linux/install.sh` and `editor-mime.xml`, and `src/update.rs`
(`asset_name`, `newest_release`, `verify`, the three install paths).

1. **Confirm F2** end to end: does the artifact name need an architecture suffix
   if the universal build fails? `asset_name()` matches `("macos", _)`, so either
   shape works — but the README table must name it.
2. **Confirm F8** coverage: the Windows job is the only packaging path with no CI
   job. Is there any other artifact that is built only on a tag?
3. **Confirm F26**: decide whether Linux **aarch64** (and Windows ARM64) is in
   scope. `asset_name()` returns `None` for it, so ARM Linux gets no artifact and
   a silently inert updater. If it stays out of scope, say so in the download table
   rather than leaving it implicit.
4. **Confirm F25** on a machine with no `zenity` and no `kdialog`: does the
   background check log anything when it declines to prompt? Then check the macOS
   `osascript` dialog's behaviour when the app is not frontmost.
5. Check the `SHA256SUMS` contract end to end: the publish job runs
   `sha256sum editor-v*`, and the updater refuses a release without it. Does the
   wildcard ever pick up `SHA256SUMS` itself on a re-run?
6. Verify the tag/version guard still runs before any build on every matrix leg.

### R6 — Documentation and claim reviewer

Read `README.md`, `packaging/windows/README.md`,
`docs/PLAN.md`, `docs/POLISH.md`.

1. **F23**: list every README claim that is now false or unqualified — YAML
   default (F3), "macOS" download (F2), "Windows 10/11" (end of servicing
   14 Oct 2025), refine-pending list in the release body (F1, F9, F16).
2. Check the `<!-- keymap:start -->` table still matches `keymap.rs` (a test
   enforces it; confirm the test is the same table).
3. Confirm `docs/PLAN.md` remains the canonical plan and that nothing in this
   audit should be filed as a new root-level plan (this file is an audit, not a
   plan).
4. Note for F27: the F1 legend and the README shortcut table say "select all"
   without a mode, so once `Ctrl`+`A` works in Preview the row needs no change —
   but the right-click menu's Preview variant should say what it now does.

### R7 — Preview, selection and clipboard reviewer

Read `src/app/mod.rs` (`on_key`, `preview_selection_text`,
`paint_preview_selection`, the `menu::Action` dispatch), `src/app/preview.rs`,
`src/ui/preview_view.rs` (`Row`, `WordSel`, `rows()`), `src/layout/markdown.rs`
(the row-emission path), `src/layout/json_tree.rs` (node source ranges),
`src/parse/csv.rs`.

1. **Scope F27** by finding the cheapest place to attach a source span: on
   `DrawOp::Text` at emission, or a parallel `Vec<(u32, u32)>` built beside
   `rows()`. Report which renderers already have the span for free (JSON, CSV)
   and which need it threaded (Markdown, text, logs).
2. **Decided & implemented.** The Split precedence rule is the highlighted
   pane — the one a selection was last pressed in — owns `Ctrl`+`C`, the Copy
   button and the menu's Copy, falling back to the other pane when it has
   nothing selected (`app::SelPane`, pinned by `the_highlighted_pane_owns_ctrl_c`).
   Still an owner decision: what a double-clicked word copies (source token
   vs whole source line).
3. Check `Ctrl`+`A` in Preview does not fight the caret: the editor model has a
   selection of its own, and `follow_caret()` must not scroll the preview to the
   end of the document when the selection is set to all rows.
4. Decide what a selection should copy when it covers rendered-only content that
   has no source equivalent — a Markdown table's drawn rules, a fenced block's
   language label, a JSON fold marker. Raw means those are absent; confirm that
   is the intent rather than a surprise.
5. Confirm the clipboard payload uses the file's own line ending and encoding
   (F9, F4) so a Preview copy pastes back into the same file unchanged.
6. Check `Ctrl`+`A` in Split: which pane does it select, and is that stated
   anywhere a user can see it?

---

## Verification recipes

Everything here is std + shell; no crate is needed for any fix above.

**Linux chords (F1), without a desktop:**

```
cargo build --release
xvfb-run -a -s "-screen 0 1280x800x24" sh -c '
  ./target/release/editor --no-update /tmp/chord.md &
  pid=$!
  sleep 3
  xdotool type "hello"
  xdotool key ctrl+s
  sleep 1
  kill "$pid"
'
stat -c %y /tmp/chord.md    # mtime must have changed
# (`$!` and not `%1`: job control is off in a non-interactive sh.)
```

**X11 key semantics, isolated** (settles F1's premise in ten lines): open a
display, take a `KeyPress` for `XK_s` with `ControlMask`, call `XLookupString`,
and print `n`, `buf[0]` and the keysym. Expected: `1 0x13 0x73`.

**Windows manifest (F14):**

```
mt.exe -inputresource:target\release\editor.exe;#1 -out:extracted.manifest
```

**macOS universal build (F2):**

```
lipo -info target/Editor.app/Contents/MacOS/editor
file target/Editor.app/Contents/MacOS/editor
```

**Linux MIME (F3):**

```
HOME=/tmp/h sh packaging/linux/install.sh
HOME=/tmp/h xdg-mime query filetype /tmp/x.yaml        # application/yaml
HOME=/tmp/h xdg-mime query default application/yaml   # editor.desktop
```

**CRLF round-trip (F9):**

```
printf 'a\r\nb\r\n' > /tmp/crlf.txt
# open, type one char, save
xxd /tmp/crlf.txt | head -2      # expect 0d 0a to still be there
```

---

## Unverified — read these as questions, not results

- **No GUI was executed on any OS.** F1 is read from libX11's source and man
  page and from the single dispatch site, which is decisive as far as static
  reading goes, but it is the claim most worth being wrong about cheaply: run the
  recipe above before changing the backend.
- The AppKit findings (F12, F13, F20) come from documentation and Apple's release
  notes, not from a Mac. In particular, F13's double-coast depends on how the
  app's glide is armed, which was not traced end to end.
- F2 assumes `macos-latest` is arm64 today; that is a moving label. Pin
  `macos-26` (or an explicit `-intel` runner) if reproducibility matters.
- F3 depends on the distribution's shared-mime-info version; the canonical type
  changed in 2.3, so any distro older than that still matches `text/x-yaml`.
  Declaring both is correct either way.
- F6's exact failure threshold depends on whether the X server advertises
  BIG-REQUESTS; the 262 KB protocol limit and the 16 MiB ceiling are the two
  anchors.
- Whether `XLookupString` returns usable bytes for Cyrillic on a UTF-8 locale is
  implementation-dependent and not contractually guaranteed. The keysym-derived
  fix makes the question moot.
- F3's companion claim — that `application/x-ndjson` is absent from
  shared-mime-info — was read from a partially truncated package file list plus
  the upstream NEWS entries. Treat "absent" as very likely, not proven; the fix
  (ship our own definition) is unaffected either way.
- Windows 10 ESU dates beyond the consumer window (12 Oct 2027) were not
  confirmed from a Microsoft page.
- F25's Linux behaviour is read, not observed: `ask_with` returns `None` only
  when the helper is not on `PATH`, which was inferred from the code rather than
  tested on a helper-less system.

---

## Revisions

Corrections made after re-reading this audit against the tree, kept here so a
reader can see what changed rather than having to diff it.

### Application of the findings (2026-11-01)

- **F1 — fixed.** Confirmed statically: the `KEY_PRESS` arm in
  `src/platform/linux.rs` matched this audit's excerpt exactly (`key_of` maps
  only navigation and F-keys, printables fall to the buffer, and with
  `ControlMask` set libX11's `XkbToControl` has already turned the buffer byte
  into a control character, so `Ctrl+S` dispatched nothing). The Xvfb probe
  could not be run on this machine: the checkbox that produced this audit is a
  Windows host with no X server, no `xvfb-run`/`xdotool`, no Docker and no WSL
  image. Fixed per the finding, with one deviation: the printable keysyms map
  to `Key::Char` only while Ctrl or Alt is held (the Windows `map_key` arm this
  finding cites as the pattern), not unconditionally — an unconditional arm
  would have hijacked plain typing too, since on X11 (unlike Windows' WM_CHAR)
  text and keydown come from the one call site and the `Text` event only passes
  through the buffer branch. Shift is handled by lowercasing the mapped char so
  `Ctrl+Shift+S` still matches. `key_of` now takes the modifiers. Proof is
  permanent in two permanent places, neither of which is `cargo test` alone:
  (a) headless `key_of` unit tests inside `src/platform/linux.rs` (run on the
  CI test job's ubuntu runner, which compiles the backend for real), covering
  every Ctrl+letter in the legend, the punctuation chords, the no-chord case
  for plain typing, and the untouched navigation arms; (b) a step in
  `tools/linux-window-smoke.sh` that sends `ctrl+s` at the live window over a
  writable copy of the sample and asserts the file's mtime changed — the probe
  this finding prescribed, now part of the suite instead of a one-off recipe.
  The Linux-backend changes compile via
  `cargo check --target x86_64-unknown-linux-gnu`; the smoke step and the key
  tests own the runtime evidence, and cannot fire until this is next pushed (no
  X server available locally).
- **F2 — fixed.** `packaging/macos/bundle.sh` now builds both slices
  (`x86_64-apple-darwin` and `aarch64-apple-darwin`, `rustup target add` before
  each so a source checkout works too), merges them with `lipo -create` into
  `Contents/MacOS/editor`, and pins
  `MACOSX_DEPLOYMENT_TARGET=12.0` for both slices. `Info.plist`'s
  `LSMinimumSystemVersion` went 11.0 → 12.0 to match the pinned floor; the
  plist comment now ties the two numbers together so one cannot drift.
  Permanent proof: `lipo -info` runs inside `bundle.sh` and greps for both
  architectures (a one-slice bundle is a hard error today, not a release note),
  and the `packaging-macos` CI job asserts the same thing on every push — which
  is the check that fails *before* this change and passes after (no Mac was
  run: the alpha compile happens on the arm64 runner in CI, which is the same
  place it always would have). The release body's download row now names the
  architecture; `src/update.rs`'s `("macos", _)` arm needed no change because a
  universal zip is an arch-agnostic artifact by design.
- **F3 — fixed.** `packaging/linux/editor.desktop` and `install.sh` declare and
  default **both** `application/yaml` and `text/x-yaml` (the alias survives on
  distros whose shared-mime-info predates 2.3). Permanent proof: the
  `packaging-linux` CI job now asserts
  `xdg-mime query default application/yaml` **and** `... text/x-yaml` both name
  `editor.desktop` in the throwaway `HOME` it already sets up — the exact
  check the finding prescribed. No local runner observation was possible (this
  machine has no Linux), which is precisely why the assertion lives in CI.
- **F4 + F9 — fixed, as one change.** Confirmed statically first: both load
  paths (`load`'s whole-file branch and the stream's `drain`) decoded with a
  bare `from_utf8_lossy`, no BOM check, no 16-bit branch, and `save` wrote
  `buffer.text()` straight back — exactly as the finding describes. One tab-
  level record, as these two findings prescribe together: `Tab` now carries
  `encoding: load::Encoding` (`Utf8` / `Utf8Bom` / `Utf16Le` / `Utf16Be` /
  `Unknown`) and `eol: load::Eol` (`Lf` / `Crlf` / `Mixed`), decided at open:
  the whole-file path sniffs the BOM and a strict UTF-8 verdict (`decode`);
  the streamed path sniffs the BOM at open and tracks per-chunk validity
  through the existing scratch carry, settling its record at the seal where
  the whole text is in memory (`sealed_info`). Save re-applies the convention
  and re-encodes in kind (`load::encode`) — a `
` file save is its own
  bytes again — and an `Unsafe` tab is refused with a status message
  (“save refused: …”) instead of writing the lossy decode over the file. Two
  corners recorded `Unknown` even though they carry a BOM: an odd-length
  16-bit file and one with lone surrogates — neither is reproducible, and
  reproducibility is the bar. Mixed-EOL files record `Mixed` and save in the
  normalized LF form (documented in the enum). Reload (`Ctrl+R`) now goes
  through the same load path instead of `read_to_string`, so a file whose
  encoding changed on disk updates the tab's record. `saved_text` and
  `same_lines` are untouched (the audit's note that the dirty side is already
  EOL-tolerant held: all 591 tests in the suite pass unchanged). Permanent
  proofs: headless tests for every encoding round trip and every refusal
  (CP1252 bytes, odd-length UTF-16, lone surrogate — the refusal test asserts
  the file bytes and the status message), the EOL census (`Lf`/`Crlf`/`Mixed`),
  a CRLF open-edit-save that asserts the exact bytes after one keystroke, and
  a streamed-load record test — plus two fixtures in `samples/`
  (`sample_utf16le.txt`, `sample_crlf.txt`) that are both the tests' data and
  a human-readable demonstration of what the app now survives. What is NOT
  provided deliberately: a streamed (1 MB+) *16-bit* file cannot be opened
  faithfully by the stream's bytes-to-UTF-8 decode, so its tab is recorded
  `Unknown` and refuses to save — the lossy decode written back over the
  file would be exactly the data loss the finding is about.
- **F5 — fixed.** `install_macos` no longer deletes the bundle first, and no
  longer stages in `$TMPDIR`. The swap (`stage_and_swap` in `src/update.rs`)
  copies the fresh bundle from the staging archive into the *destination's own
  parent directory* under a `.update-new` name — so every move in the install
  is a rename between two entries of the same directory, same-volume by
  construction, and the `EXDEV` failure the finding measured becomes
  impossible. The old bundle is renamed aside (`.update-old`) rather than
  deleted, the fresh copy is renamed in, and a second-rename failure renames
  the aside copy back. A writability probe (create + remove a throwaway file
  next to the bundle) refuses installs whose directory cannot take the result
  *before* anything is staged, and a failed `copy_dir` cleans its half-written
  stage and leaves the running bundle byte-identical. Permanent proofs: three
  tests in `src/update.rs` — the success swap (new content in place, both
  sidecar names gone), a failing copy (error names the fresh source, the old
  bundle's bytes are asserted untouched, no `.update-new`/`.update-old` left)
  and the refused parent (the probe refused before `copy_dir` staged anything,
  the fresh bundle untouched). What is *not* testable on this machine: the
  second-rename-failure rollback itself — forcing a rename to fail after the
  aside-move succeeded needs either a foreign volume at the destination
  (impossible by the same-parent construction that removed `EXDEV`) or root,
  and this host is a Windows box with no macOS. That path is reviewed by
  reading, not by a test, and is noted here so the gap is explicit.

- **F8 — fixed, and validated on this machine (it is a Windows host with
  WiX 7.0.0 + WixToolset.UI.wixext already installed).** `ci.yml` gains a
  `packaging-windows` job mirroring `release.yml`'s exact WiX pairing (7.0.0,
  `eula accept wix7`, UI extension) and build flags: `-d
  Version=0.0.0`, no artifact upload. The
  "assert the built package" part is a new checker, `tools/check-msi.ps1`,
  which opens the MSI's Windows Installer database via late-bound COM (no
  WiX v3 `msidb.exe` dependency) and asserts the three things the finding
  names: the payload (`File` table carries `editor.exe`), the ARP properties
  (`ProductName`/`ProductVersion`/`Manufacturer`/`UpgradeCode`/
  `ARPPRODUCTICON`/`ARPCOMMENTS`, with the version pinned to the 0.0.0 this
  job stamps), and both of the package's own dialog tables
  (`EditorWelcomeDlg`, `EditorDoneDlg`) plus the `ProgressDlg` the UI
  extension supplies. The three checks were measured against an MSI built
  with the exact CI flags, with the checker passing all three.
  (The empirical note inside the checker: a `.wxs` text check cannot see
  what WiX linked — quoting the database, not the file, is what makes the
  check worth a CI job.) The build and the checker were exercised locally on
  this Windows host — a 1.7 MB MSI built with the exact CI flags, the
  checker run over it once and reporting payload OK / ARP OK / dialogs OK /
  MSI OK — so the job's commands and assertion script are not going into CI
  untested; the CI job itself runs on the runner, and its result there is
  the first thing to watch on the next push.
- **F21 — fixed.** Confirmed statically: `WM_NCHITTEST` returned `HTCLIENT` for
  every title-bar point the app draws, so Win11's Snap Layouts flyout — which
  keys on a hover reported as `HTMAXBUTTON` — was silently unavailable, with
  no error anywhere. The fix is the convention the finding names: the
  maximize button's hit now reports `HTMAXBUTTON` (decided by a small pure
  mapper, `nc_code`, which leaves `Drag`/`Client` undecided so the resize
  band still tests first and keeps the other controls at `HTCLIENT`); the
  click arrives as `WM_NCLBUTTONDOWN` with that code and is converted back
  into the same client-space `MouseDown` a client click carries — the app's
  own maximize toggle stays the only mover, buttons act on press so no `Up`
  pair is owed — and `WM_NCMOUSEMOVE` is converted and dispatched as the
  ordinary `MouseMove` so the title bar's hover highlight keeps repainting
  over a control the client `WM_MOUSEMOVE` no longer reaches (the default
  proc still gets it, so its NC hover tracking is untouched). Proof: a pinned
  unit test — `HTMAXBUTTON` must be Win32's own 9, `Maximize` must map to it
  and nothing else, and the enumerate of the other symbols pins the rest at
  `HTCLIENT` with `Drag`/`Client` undecided. What a headless suite cannot
  show: the flyout itself — hovering the maximize button on a real Windows 11
  desktop and seeing the layouts popup is the one manual pass left (the
  audit's Windows runtime checks are host-dependent; this machine builds but
  does not show a Snap flyout in CI). Not changed: the maximize button's
  painted look, its click semantics, or any other hit code.
- **F13 — fixed.** Confirmed statically: every momentum event re-entered the
  app as the same kind as a wheel notch, so each one accumulated another
  glide target on a view the pad's own inertia was already driving — two
  drivers on one pane, exactly the double-apply the finding describes. The
  fix is the split the finding scopes: `Event::Scroll` now carries
  `precise` (`src/ui/event.rs`) — false for every discrete wheel (Win32
  wheel/tilt, X11 buttons, macOS line-mode deltas — glides untouched for
  all of them), true from AppKit's `hasPreciseScrollingDeltas`. The app
  applies true ones on both axes through the direct paths (`scroll_direct`,
  `pan_horizontal_direct_at`) and never accumulates a glide behind them;
  `phase`-began/`mayBegin` is forwarded as a zero-delta precise Scroll —
  exactly the direct path where a running ride is killed — so a finger laid
  on the glass takes a wheel glide away. One deliberate deviation, recorded:
  the finding offered "ignore or absorb" for momentum events; these are
  absorbed, not dropped — forwarded as precise steps (never glide-starting,
  so the double-driver defect is still gone) and the ride stays the pad's
  own, which is what flicks on a real trackpad expect.
  `isDirectionInvertedFromDevice` is honored the same way the existing
  line-mode path is: AppKit has already baked the user's scrolling
  preference into the sign, so nothing here flips it again. Proof: the
  headless wheel test is unchanged, and a new pinned test drives the app —
  a notch glides over frames, a zero-delta precise event (the phase-began)
  cancels a wheel ride in flight, and a precise delta lands on its own
  frame with no glide target and no animating clock. Not verifiable on this
  machine: actual trackpad feel — the phase bits and momentum chain on real
  macOS hardware needs a finger on the glass; recorded as review-verified
  pending CI.
- **F20 — fixed.** Confirmed statically, both halves. The view never
  overrode `acceptsFirstMouse:`, so AppKit's default `NO` ate the first
  click of an inactive window — a tab or a word in an unfocused Editor
  needed a second press; and the window was never marked
  `setRestorable:NO`, so AppKit's restoration could race the session file
  that already restores tabs, mode and geometry. The fix is the two
  messages the finding scopes: `view_accepts_first_mouse` returns 1 —
  every press (including right-clicks, whose menu should open on the
  first press too) is delivered to the view and handled by the handlers
  that already own the click's job — registered on the view class as
  `acceptsFirstMouse:` (`B@:@`); and `app_did_finish_launching` sends
  `setRestorable:NO` before making the window key, with the reason (the
  session file is the one restorer) in the comment.
  Proof: the cfg-gated method table and the launch callback are exercised
  by the real thing CI runs — `smoke-macos` starts the actual app, whose
  `make_class` calls build the view class with the new method and the
  window whose restoration flag is set before it shows; a malformed
  method table or a broken selector there is a startup failure, which the
  smoke fails on. Not verifiable headlessly on this machine: actual
  click-through (an unfocused window, one press on a tab) and observing
  that no duplicate restoration happens need real hardware and a real
  relaunch — recorded as review-verified pending CI.
- **F12 — fixed.** Confirmed statically, both halves: no `NSMenu` existed
  anywhere in the backend, and the launch callback sent the
  macOS-14-deprecated `activateIgnoringOtherApps:`. The fix, as scoped:
  a minimal menu is built at launch from the backend's own ObjC helpers
  (`msg`/`msg1`…, new `msg3` for the three-argument
  `initWithTitle:action:keyEquivalent:`) and set as `NSApp.mainMenu` —
  App menu (About/Hide/Quit), Edit (Undo/Redo with shift-command masks,
  Cut/Copy/Paste/Select All), Window (Minimize/Zoom, per the finding's
  scope); every leaf is a standard AppKit action, so the responder chain
  delivers them and inert ones gray out rather than lie (routing menu
  actions into the app's own command machinery is noted as follow-up in
  the helper's comment). Activation is now probed:
  `respondsToSelector:` gates `activate` (which does not exist before
  macOS 14 — an unrecognized selector is an ObjC exception there, and the
  release still supports 12) with `activateIgnoringOtherApps:` carrying
  older systems, then `makeKeyAndOrderFront:` in either case.
  Proof: the launch path is exercised by the real thing CI runs —
  `smoke-macos` starts the app, which must succeed in building the menu
  and both activation branches to put a window on screen. Not verifiable
  headlessly on this machine: the menu appearing on a real screen (and
  shift-Z redo) needs eyes on macOS; recorded as review-verified pending
  CI.
- **F14 — fixed.** Confirmed statically: `build.rs` embedded icon and
  `VS_VERSIONINFO` only, so no manifest — `GetVersionEx` lied (6.2), no
  `<supportedOS>`, no `<trustInfo>`, no `<dpiAwareness>`, DPI carried by
  the runtime call alone. The fix extends the existing `.res` writer with
  `RT_MANIFEST = 24` at **id 1**, reusing the same `entry()` helper and
  header layout: one `<supportedOS
  Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>` (the one GUID covering
  Windows 10 *and* 11), `<trustInfo>`otyping asInvoker, and
  `<dpiAwareness>PerMonitorV2, PerMonitor</dpiAwareness>` — exactly the
  contents the finding scopes, with `longPathAware` and
  `activeCodePage=UTF-8` left out for the reasons it gives. The linker
  route (`/MANIFEST:EMBED` + `/MANIFESTINPUT`) is avoided as the finding
  directs (MAX_PATH cap, merging, the stray `<exe>.manifest`). The
  finding's consequence is respected in code: the runtime
  `SetProcessDpiAwarenessContext` call keeps its result discarded, now
  with a comment saying the manifest is the declaration and the call only
  the old-OS fallback.
  Proof: a build-time assertion inside `build.rs` — after the entries are
  written, the stream is parsed back (`find_entry`, the same ordinal
  layout `entry()` emits) and the manifest at (24, 1) must be byte-equal;
  a broken entry layout is a build failure, and a build script runs on
  every MSVC build, so the check is permanent. It was verified on this
  machine: the Windows-MSVC target build ran the script, wrote
  `editor.res`, and the assertion passed; the generated file was then
  inspected independently, showing the GUID, `asInvoker` and
  PerMonitorV2 exactly once each. Not verifiable on this machine: an
  actual `link.exe` fold into the running image (a smoke on a Windows
  runner covers it in CI); recorded.
- **F17 — fixed.** Confirmed statically: both curl rungs passed bare
  `-L`, which follows redirects to any host and any scheme, so the
  module comment's "cannot turn a fetch into a plaintext download" was
  stronger than the code. Fix, as scoped: both rungs now carry
  `--proto =https --proto-redir =https --max-redirs 3` — the audit's
  exact flags — built in one shared `curl_args` (the URL is appended at
  the rung sites), so the two rungs cannot drift and the test covers
  every curl on every host. The module comment is updated to say what
  the pins pin and to name the caveat the finding notes: wget and the
  python rung still follow redirects with no portable equivalent, so the
  trust anchor stays the initial TLS request to the GitHub API.
  Proof: `curl_rungs_keep_redirects_on_https` pins the values of all
  three flags on every curl helper (`--proto`/`--proto-redir` must be
  `=https`, `--max-redirs` must be 3); and against the suite's
  loopback-HTTP fixture the real curl now refuses with exit 1 —
  `every_available_rung_works_on_its_own` was updated so a curl refusal
  on a plaintext fixture is expected as the pin proving itself (while
  wget/python still download there, keeping `worked >= 1` meaningful —
  6 net tests pass). Not verifiable headlessly here: a cross-host https
  redirect chain follow (the flags are curl's documented behavior).
- **F19 — fixed, as scoped per the finding's three parts.** Confirmed
  statically: the Linux candidates are Debian/Fedora-shaped prefixes, the
  Windows ones are bare basenames拼 against `%WINDIR%\Fonts`, so on Arch,
  openSUSE, NixOS and any different layout CJK and emoji rendered as
  tofu. (1) Linux: `fc-match -f %{file} :charset=<cp>` runs as a child
  process (`fc_match_file`) before the static lists — with the
  best-guess caveat handled correctly: a returned file that does not
  really carry the codepoint fails the scan and falls through, so the
  static list stays the last resort exactly as the finding scopes, and
  the static walk cursors are untouched by an fc-match load. The charset
  syntax was verified live: hex codepoints, `%{file}`, BMP and astral
  alike (the probe on this host returned `seguisym.ttf` for U+1F600 and
  `msgothic.ttc` for U+4E2D — both codepoints this list's Linux paths
  would have covered only by luck). (2) Windows: `%WINDIR%\Fonts` is
  `read_dir`-listed and every basename (both candidate lists, including
  the emoji face) resolves case-blindly against what the directory
  actually holds, with the constructed path as the last resort. (3) The
  current lists are untouched as the final fallback. Proof: pinned head
  tests (hex charset formatting — a wrong base would ask for another
  character silently; basename matching incl. case-blind hit, miss and
  empty-directory fallback) and — because this session's host IS
  Windows — the read_dir resolution is live in the test run, not merely
  compiled; the Linux half is runtime-gated and dead here, so it is
  review-verified pending CI.
- **F11 + F15 — the text-input package, as scoped per finding.** F11's fix is
  exactly the audit's ten-symbol XIM recipe in `src/platform/linux.rs`:
  `XSetLocaleModifiers("")` (empty — the `@im=none` override would have
  disabled the user's ibus/fcitx), `XOpenIM`, `XGetIMValues` style query,
  `XCreateIC` on the window with `XIMPreeditNothing|XIMStatusNothing` (chosen
  by `pick_style`, the one hostable style; the audit's `XCreateIC`-fails rule
  enforced), `XGetICValues(XNFilterEvents)` OR-ed into `XSelectInput`,
  `XSetICFocus`/`XUnsetICFocus` on `FOCUS_IN`/`FOCUS_OUT` (a `FOCUS_IN` arm
  is new — the backend never subscribed to it), `XFilterEvent` before
  `handle_event` and inside both clipboard-spool loops (filtered events are
  consumed, never replayed — their raw form must not reach the app twice),
  `Xutf8LookupString` in `KEY_PRESS` with the bare `XLookupString` kept as
  the fallback when there is no IM or no usable style, and
  `XRegisterIMInstantiateCallback` for an IM that starts after the app (its
  callback stores one process-global flag; the loop rebuilds the IC on the
  next iteration). Confirmed statically against `key_of`/the `KEY_PRESS` arm:
  the audit's two failure modes were exactly as described — `dead_acute`
  produced a zero-byte buffer and an unmapped keysym, so the accent vanished.
  Proof: three headless unit tests over `pick_style` (hostable style chosen
  among ranged offers; a styleless IM anchors to null; an empty list anchors
  to null) plus compile-time inclusion of the fallback branch. Not verifiable
  on this machine: the audit's own acceptance test (type Japanese, type `é`
  via dead keys, in a tab) needs a live IM or a compose-capable X server; Xvfb
  has neither, so the `tools/linux-window-smoke.sh` proof stays the F1 chord
  one and this is recorded as review-verified pending CI. F15's fix is the
  document-now half it scopes: `docs/PLAN.md`'s one-line "XIM pending" note
  is replaced by a per-platform text-input state table — Linux XIM wired
  (with the honest caveat that no live ibus has composed through it yet),
  Windows stating precisely what works (`DefWindowProc`'s committed `WM_CHAR`
  fallback) and what does not (in-buffer preedit, backspace-into-composition,
  reconversion, at-caret candidates — named as the documented Microsoft
  fallback, with the `WM_IME_*`/`ImmSetCompositionWindow` fix recipe carried
  forward), and macOS saying what `keyDown`-only means. The top-of-file module
  comment in `linux.rs` matches. The in-repo README has no IME claim to
  correct; the GitHub release text is owner-facing and outside the tree.
- **F10 — fixed.** Confirmed by reading `src/main.rs`: the startup path was
  `std::env::args().skip(1).collect()`, and `env::args()` intrinsically
  panics on argv bytes that are not UTF-8 (`InvalidUnicode` panic, before
  any window exists). `main` now collects `args_os()`, and `cli::run`/
  `cli::parse` take `OsString` throughout: flags are matched only when the
  argument is convertible (`to_str()` — flags are ASCII by definition, and
  a non-UTF-8 argument falls through to `files` exactly like a bare
  operand, it cannot *be* a flag), operand paths are built with
  `PathBuf::from(OsString)` so the bytes the shell passed are the bytes
  that reach `Tab`/load, and the value-taking flags (`--size`, `--scale`,
  `--scroll`, `--mode`) refuse their value through the same `to_str()`
  fallback the existing bad-value arms use. `update::apply_update` keeps
  its `&[String]` signature: it is dispatched only by `update.rs` itself,
  on Windows, where argv arrives as UTF-16 and the conversion is exact —
  a comment says so, so a future reader does not "fix" the wrong end.
  Proof: unit tests in `cli.rs` — a non-UTF-8 operand survives `parse` as
  the exact same `OsString` (`OsStr::eq` over the raw bytes),
  a non-UTF-8 first argument is taken as a file and not as an unknown-flag
  error, and the `--text`-value flags refuse a non-UTF-8 value with their
  existing message instead of a flag-unknown.  Windows-side verification is
  by construction (argv is UTF-16 here; the Linux crash needs the bytes at
  all, which this host cannot produce — the audit's Linux CI runner runs
  these tests against a real Unix `args_os`).
- **F6 — fixed.** Confirmed statically first: the old selection-request arm
  answered every client with one `XChangeProperty` of the whole `clip_text`,
  and a large paste crosses both the ~262 KB single-request cap on any server
  (`BadLength`, requestor gets nothing) and any small `XGetWindowProperty`
  window on the read side (silently truncated). The owner now answers with an
  INCR announcement (the `INCR` atom as the 32-bit property type) when the
  text exceeds `100_000` bytes and the target is `UTF8_STRING`, converts each
  `PropertyNotify` into the next 64 KiB chunk (delete the property, write,
  repeat), and terminates with a zero-length property; `SELECTION_CLEAR` drops
  the stream state, so a new requestor starts fresh. The read side sees the
  INCR announce, converts in incremental mode, accumulates every chunk via
  repeated `XGetWindowProperty` calls, and completes on the zero-length
  property, feeding the assembled text into the same entry point a small
  paste uses — the app never sees partial data, only the finished String.
  Proof is permanent, not a run-once recipe: `tools/linux-window-smoke.sh`
  gains a ~9 MB round trip — a 9437184-byte selection is owned by `xclip -i`
  (the second client the finding asks for), the editor pastes it (`Ctrl+A`,
  `Ctrl+V`, `Ctrl+S`) and the saved file must hold exactly that byte count;
  then the editor selects all and copies (`Ctrl+A`, `Ctrl+C`) and
  `xclip -selection clipboard -o` must read out the same byte count through
  the INCR handshake the write side implements. The script now launches the
  binary with `--mode edit` so those keystrokes land in the buffer rather
  than a preview pane's read-only selection, and `ci.yml`/`release.yml`
  install `xclip` next to `xdotool` — no second client, no proof (a shell
  cannot signal an X selection owner by itself). Both directions run
  through the same handshake: the paste exercises the editor as requestor,
  the `xclip -o` read-back exercises it as owner. What the round trip does
  not cover: a requestor replaced mid-transfer, and large non-`UTF8_STRING`
  targets (no second target is offered). This Windows host cannot run it
  (no X server; F1's caveat applies again), but both CI smoke paths run it
  and fail before publication.
- **F18 — fixed, the lock and the collision half; the handoff stays open.**
  Confirmed statically first: `forward_to_running` was exactly the excerpt —
  the Win32 arm forwarded, every other arm fell through with "once their GUI
  (M8/M9) lands" — and `settings.save` staged `session.txt` through one
  shared scratch name, so two instances writing at once could commit a
  mid-write snapshot. Fix, pure std:
  (a) collision half — the scratch stage is now `session.tmp.<pid>`, so two
  processes cannot meet on one staged file and every rename commits a
  complete text;
  (b) detection half — `settings::try_hold_session` takes an advisory
  `File::try_lock` on a separate `session.lock` (the lock cannot live on
  `session.txt` itself: the rename replaces the inode a flock would guard)
  and leaks the handle on success, so the lease holds for the winner's whole
  run; a filesystem that refuses advisory locks is treated the same as EOS
  contention, never blocking a launch. The non-Windows arm of
  `forward_to_running` now reports through it, so a later launch already
  knows one is running — but on this host the change is exercised by tests
  on Linux/macOS CI, not by a live multi-window observation here.
  Measured: 600/600 tests green; in-process lease test (first holder wins,
  second refusal on the same directory) and a pin on the exact per-pid
  scratch name, both in `settings.rs`. What stays open is the argv handoff
  itself — a D-Bus name or a socket in `$XDG_RUNTIME_DIR` carrying paths —
  which still has no listener; the finding's convenience half and its
  Windows half (which is already real) stand as before.
- **F16 — fixed, runtime proof in the smoke suite.** Confirmed statically:
  no `XSetClassHint` anywhere in `src/platform/linux.rs` and no
  `StartupWMClass` in `packaging/linux/editor.desktop` — exactly the
  excerpt. Fix: `set_class` publishes `WM_CLASS` (res_name `editor`,
  res_class `Editor`) through a hand-written `XClassHint`/`XSetClassHint`
  binding right after the icon lands, with the convention comment tying
  both spellings to the desktop key; `editor.desktop` gains
  `StartupWMClass=Editor`, `TryExec=editor`, `GenericName` and `Keywords`;
  while editing, one duplicated `application/x-ndjson` MIME segment was
  removed. Two permanent proofs, one on each side of the pair the shell
  matches on: `tools/linux-window-smoke.sh` asserts the live window's
  `WM_CLASS` names `Editor` (xdotool getwindowproperty, hard-fail before the
  add), and the `packaging-linux` CI job now runs `desktop-file-validate`
  on the *installed* entry and pins `StartupWMClass=Editor` +
  `TryExec=editor` — so the static and runtime halves cannot drift apart
  unnoticed. Measured locally: `cargo check` on the Linux target green and
  the smoke script parses (`sh -n`); the Xvfb run itself still waits for CI
  on this host, no X server here, as with F1.
- **F22 — fixed, comment only, with one correction to the finding's
  evidence.** Probed first: `packaging/windows/editor.wxs` names none of the
  three RM/reboot properties outside the launch-condition references
  (`REBOOT="ReallySuppress"` as a condition) — the comment that credited
  `MSIDISABLERMRESTART=1` with helping close the reboot door lives in
  `src/update.rs`'s `run_msi` doc, which said all three properties "close
  that door for good". That comment now credits only
  `REBOOT=ReallySuppress` + `MSIRESTARTMANAGERCONTROL=Disable`, and carries
  a paragraph stating what `MSIDISABLERMRESTART=1` actually does (only
  meaningful when the Restart Manager runs; documented as ignored when RM
  is unavailable or disabled, which is exactly this install's state) and
  why it deliberately stays in the argv. No flag changed — the pinning test
  on the msiexec argv (`update::tests`, 22/22 green after the edit) guards
  that. `README.md`'s flag list stays untouched: it lists the command as
  shipped rather than attributing the no-reboot guarantee to one flag.
- **F25 — fixed, both halves, proof by compile and gate rather than by
  click (this host has no Linux or macOS GUI surface).** Confirmed the
  evidence first: `ask_install` was exactly the three shapes, and the
  Linux tail really was `ask_with("kdialog", …).unwrap_or(false)`. Fix:
  (a) Linux — after zenity *and* kdialog are absent, a plain y/N is asked
  on `/dev/tty` (open read+write, echo left on, EOF = decline), and every
  skip reason is `log_step`'d into `last-update.log` — helper absent,
  terminal refused, EOF, read error — so the invisible decline is gone;
  the answer itself still reaches the existing "the offer was declined"
  line the caller logs.
  (b) macOS — `ask_install` now draws a real `NSAlert`
  (`platform::macos::native_alert`, ~20 lines over the module's existing
  objc vocabulary: alloc/init, message + informative text, two buttons,
  `runModal`), owned by this process like the Windows box; the last
  script host in the updater is gone. First button = `NSModalResponseOK`
  (1), second button = the precomposed 1000+ range — the decline.
  `platform::forward_to_running` also takes the session lease before the
  backend gate now (F18's `try_hold_session` was unreachable on the
  Windows build, which clippy had just started showing as dead code); the
  mutex handoff stays the Windows authority. Measured: 600/600 tests,
  clippy silent on all targets, both foreign targets compile, fmt clean.
  Not directly observed on this machine (no Linux/macOS GUI): the tty
  answer and the alert actually popping; both are exercised by their OS's
  CI compile only. The finding's own note about F25 being "read, not
  observed" still applies to the runtime shapes.
- **F23 — fixed, bullet by bullet, with two scope notes.** Every claim
  was first re-read against the tree:
  (a) `macos.rs`'s broken `` `defaults read -g//! AppleInterfaceStyle` ``
  join — now cleanly split in the module doc;
  (b) `platform/mod.rs`'s "open/save dialogs arrive with M9" on the
  `Host` trait — the sentence now simply states all three backends ship
  dialogs; the F18 arm comment had already lost its M8/M9 line;
  (c) the `editor_check_*` cfgs had no CI exercise — a `cross-check` job
  now runs them on every push, with an honest comment about what is and
  is not possible: `editor_check_linux` from the Windows runner is the
  true foreign-backend check (`linux.rs`'s `not(unix)` seam was built
  for exactly that); `editor_check_win` still carries unseamed
  `std::os::windows::ffi` uses and `editor_check_macos` rejects any
  non-Apple target (`link(kind = "framework")` = E0455 plus the
  `std::os::unix` fd in `map_file`), so those two run on their home
  runners as wiring pins; making them truly foreign needs the same seam
  work `linux.rs` got, and is recorded as follow-up scope;
  (d) the release note's "what is still missing" was rewritten to the
  post-this-round truth — XDND and the Linux argv handoff for a second
  instance, IME and live theme changes on macOS, no native ARM assets —
  and it names the audit document for cross-reference;
  (e) `README.md`'s download table now names every asset's architecture,
  macOS 12 as the universal bundle, and carries the exact Windows 10
  support dates (22H2 ended servicing 14 Oct 2025; consumer ESU to
  12 Oct 2027);
  (f) the optional `appdata` piece *was* added rather than skipped —
  `packaging/linux/editor.metainfo.xml` (desktop-application component,
  id `dev.editor.app`, launchable pointing at `editor.desktop`, the
  bundle id macOS already uses), installed by `install.sh` into
  `~/.local/share/metainfo/` and asserted by the `packaging-linux` CI
  job, so GNOME Software / KDE Discover list the app. Measured: YAML
  parses (`yaml.safe_load`), `sh -n` on install.sh, `cargo check` and
  `cargo fmt --check` clean after the doc-comment edits. Not measured:
  `appstreamcli validate` does not exist on this host, so the metainfo's
  validity is by construction, not by linter.
- **Decisions now resolved (2026-11-01): F7 and F27's Split precedence.**
  F7: the owner took code signing out of the project entirely — no signing
  steps in any workflow, no signing guides, no signing policy section in the
  README, no certificate secrets; the release publishes unsigned binaries
  checked by the `SHA256SUMS` manifest the updater already requires.
  F27 (half): "the pane that is highlighted owns Ctrl+C" — implemented as
  `app::SelPane`, the pane selection was last gestured in: reader presses,
  editor presses (caret placements included), keyboard caret movement and
  touch-selection arming all update it; a copy asks the owner first and
  falls back to the other pane when it has nothing selected. Mode switches
  re-point it (Preview→reader, Edit→editor; Split keeps it until the next
  press — both halves are on screen there). Pinned by
  `the_highlighted_pane_owns_ctrl_c` (both directions, through the real
  mouse/keyboard event path). What still waits for the owner: what a
  double-clicked word copies (source token vs whole source line), and F27's
  remaining requirement — raw-text copy and `Ctrl`+`A` in Preview.
- **Decisions the owner still owes — recorded, not implemented (F24, F26).**
  `--install`/`--uninstall` still exit non-zero off Windows (Linux
  half-completes the `--uninstall` cleanup first; macOS points at a script
  that does not register anything) **(F24)** — a two-line wording change
  plus a `0`-exit on language-specific macros is what is scoped; it alters
  published CLI behavior and error codes, so it is owner-shown. No ARM
  release leg exists **(F26)** — the trade is real work on CI runners and a
  cross-linking question for Linux's `libX11`, so the coverage stays
  explicit-on-purpose until the owner says aarch64 is supported.
- **F1** — the first draft listed `Ctrl+/` as a lost binding. There is no such
  binding; the zoom chord is `Ctrl+'+'`, which is `Ctrl+Shift+'='` on a US layout
  and therefore survives. The survivor list was corrected (`-`, `=`, `0`, `1`,
  `9`), and the suggested fix now cites the Windows backend's existing
  `0x30..=0x39 | 0x41..=0x5A if mods.ctrl || mods.alt` arm as the pattern to port
  rather than presenting the change as novel.
- **F1** — the open question about `Ctrl+'+'` was resolved by reading
  `app::on_key`, which already matches `Key::Char('+' | '=')`. It is no longer a
  question for a reviewer.
- **F18** — the first draft said a session lock needed `flock`/`LockFileEx` FFI.
  Wrong: `File::lock` and `try_lock` were stabilised in Rust 1.89, so the fix is
  pure std.
- **F5** — a garbled sentence about permission failures was replaced with what
  actually goes wrong: `remove_dir_all` failing partway leaves a partially
  deleted bundle with no rollback.
- **F4** — "turns the lossy decode into the file's real content" was ambiguous;
  it now says what happens (the lossy decode is written back over the original
  bytes).
- **F3** — the `application/x-ndjson` claim was softened and the caveat added to
  "Unverified", matching the strength of the evidence.
- **Added F24, F25, F26** — CLI parity off Windows, update-consent helpers, and
  ARM coverage were found on re-read and were only mentioned in passing before.
- The Xvfb recipe used `kill %1`, which fails without job control in a
  non-interactive shell; it now captures `$!`.
- **Added F27** — preview selection and `Ctrl`+`A` semantics. Not a discovered
  defect: it is a requested behaviour, scoped against the code so the "not a
  one-liner" claim is evidenced (rows carry no source span) rather than assumed.
- **Added "How to use this document"** — the confirm-first rule, the std-only
  constraint, the "make the proof permanent" rule, and the four findings that
  need an owner's decision rather than a reviewer's judgement.
