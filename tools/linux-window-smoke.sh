#!/bin/sh
# Start the X11 backend and prove it put a window on screen.
#
# Compiling and passing the headless suite says nothing about this file's real
# subject: the first time anything ran the X11 backend it died with a bare
# SIGSEGV before its window appeared (a hand-built XImage with a null
# `f.put_image`), and only a pass that actually opens the window can catch
# that. `ci.yml` runs this on every push and `release.yml` runs it on the
# binary it is about to publish, so the two can never disagree about what
# "the Linux build works" means.
#
#   sh tools/linux-window-smoke.sh [path-to-editor]
#
# Needs an X server: Xvfb is started here when DISPLAY is unset (xvfb-run and
# xdotool must be installed — the workflows do that).
set -u

here=$(cd "$(dirname "$0")" && pwd)
bin=${1:-./target/release/editor}
sample="$here/../samples/sample_markdown.md"
log=/tmp/editor-window-smoke.log

if [ ! -x "$bin" ]; then
    echo "FAIL: no executable at $bin (build it first)"
    exit 1
fi

# A caller with a display (a developer's session, a runner that already has
# Xvfb) is left alone; otherwise take a free one.
if [ -z "${DISPLAY:-}" ]; then
    if ! command -v xvfb-run >/dev/null 2>&1; then
        echo "FAIL: no DISPLAY and no xvfb-run to provide one"
        exit 1
    fi
    # `sh "$0"` rather than `"$0"`: xvfb-run executes what it is given, so
    # going through the shell a second time keeps the script working however
    # it was checked out (the mode is lost by more than one tool).
    exec xvfb-run -a -s "-screen 0 1280x800x24" sh "$0" "$@"
fi

echo "binary: $("$bin" --version)"
echo "DISPLAY=$DISPLAY"
"$bin" --no-update "$sample" >"$log" 2>&1 &
pid=$!
ids=""
gone=""
i=0
while [ "$i" -lt 60 ]; do
    ids=$(xdotool search --name "Editor" 2>/dev/null || true)
    if [ -n "$ids" ]; then
        break
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
        # Record *how* it died: 139 is the segfault, 134 an abort.
        wait "$pid" 2>/dev/null || gone=$?
        break
    fi
    i=$((i + 1))
    sleep 0.25
done

if [ -n "$ids" ]; then
    first=$(printf "%s\n" "$ids" | head -1)
    echo "mapped window: $(xdotool getwindowname "$first")"
    xdotool getwindowgeometry "$first"
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
else
    echo "the app is gone before any window: status ${gone:-?} (139 segv, 134 abort)"
    echo "its output was $(wc -c <"$log") bytes:"
    cat "$log"
    echo "windows xdotool can see at all:"
    xdotool search --onlyvisible --name "" 2>&1 | head -10
fi

if [ -z "$ids" ]; then
    echo "FAIL: no X window named *Editor* appeared"
    exit 1
fi
if grep -q "panicked at" "$log"; then
    echo "FAIL: the backend panicked"
    cat "$log"
    exit 1
fi
echo "OK: the X11 backend opened its window"
