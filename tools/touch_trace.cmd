@echo off
rem Touch diagnostics: launch this repo's built Editor with EDITOR_TOUCH_TRACE=1,
rem so every touch phase (contact, moves, lift velocity, PanEnd, coast, frames)
rem is appended to %TEMP%\editor_touch_trace.log while a real finger works.
rem
rem Flick the document once, close Editor, then read the log — notepad opens it
rem here. If neither target path has an exe, build first (cargo build).

set EDITOR_TOUCH_TRACE=1

if exist "%~dp0..\target\release\editor.exe" (
  start "" "%~dp0..\target\release\editor.exe"
) else if exist "%~dp0..\target\debug\editor.exe" (
  start "" "%~dp0..\target\debug\editor.exe"
) else (
  echo No built editor.exe found - run "cargo build" first.
  exit /b 1
)

timeout /t 3 >nul
notepad "%TEMP%\editor_touch_trace.log"
