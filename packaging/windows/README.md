# Windows packaging
v1: copy `editor.exe` anywhere and run `editor.exe --install` (writes HKCU file associations, no admin).
`editor.exe --uninstall` removes them. Icon/manifest embedding without crates is tracked in docs/PLAN.md (M6):
needs `rc.exe`/`windres` or a hand-built .res linked via build.rs (std only).
