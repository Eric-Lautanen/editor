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
# Needs an X server: Xvfb is started here when DISPLAY is unset (xvfb-run,
# xdotool and xclip must be installed — the workflows do that).
set -u

mode=edit
arg1=${1:-""}
case "$arg1" in --mode) mode=$2; shift 2 ;; esac
bin=${1:-./target/release/editor}
# The script's own directory, so the sample resolves beside it however the
# script was started (`set -u` makes an undefined `here` a hard error, which
# is how this line was caught).
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
sample="$here/../samples/sample_markdown.md"
log=/tmp/editor-window-smoke.log

# The chord proof (F1) needs a file the app is allowed to write over: the
# sample in the tree is opened on a writable copy.
work=$(mktemp -d /tmp/editor-smoke-XXXXXX)
trap 'rm -rf "$work"' EXIT
cp "$sample" "$work/file.md"

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
# Edit mode for the F1/F6 keystroke proofs regardless of the restored
# session view: pastes and copies must land in the buffer, not a preview
# pane's read-only selection.
"$bin" --no-update --mode "$mode" "$work/file.md" >"$log" 2>&1 &
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
    # F16: the shell matches this window to editor.desktop by WM_CLASS; a
    # generic-icon window would still be "a window named Editor", so the
    # title check above cannot catch a missing class. The class half
    # ("Editor") is what StartupWMClass in editor.desktop names.
    # `xdotool getwindowproperty` is not in the 3.2016 package Ubuntu LTS
    # ships, where asking for it just prints "unknown command" — which the
    # old form reported as MISSING and failed a window that was correct.
    # `getwindowclassname` and `xprop` both predate that, so try each.
    wmclass=$(
        xdotool getwindowclassname "$first" 2>/dev/null \
            || xprop -id "$first" WM_CLASS 2>/dev/null \
            || echo MISSING
    )
    echo "WM_CLASS: $wmclass"
    case "$wmclass" in
        *Editor*) ;;
        *)
            echo "FAIL: WM_CLASS does not name Editor (desktop cannot match it)"
            exit 1
            ;;
    esac
    # Ctrl chords (F1): with ControlMask set, libX11 converts the buffer
    # `XLookupString` fills into an ASCII control character, so a backend
    # that derives its keys from that buffer dispatches nothing at all for
    # Ctrl+letter — the window opens, typing works, save never fires. The
    # proof is observable after the fact: send `ctrl+s` at the live window
    # and the file on disk must actually change.
    xdotool windowfocus "$first" 2>/dev/null
    # Content, not mtime: the save lands in the same second the window mapped,
    # and `stat -c %Y` only counts whole seconds — a correct save read as
    # "unchanged" and failed the probe.
    before=$(cksum <"$work/file.md")
    xdotool type "x"
    xdotool key ctrl+s
    sleep 1
    after=$(cksum <"$work/file.md")
    if [ "$after" = "$before" ]; then
        echo "FAIL: Ctrl+S did not save the open file (content unchanged: $before)"
        echo "its output was:"
        cat "$log"
        exit 1
    fi
    echo "OK: Ctrl+S saved (content changed)"

    # Clipboard across the max-request boundary (F6): one X protocol request
    # is capped at about 256 KiB, so an owner that answers a selection
    # request with one big XChangeProperty dies with BadLength and the
    # pasting client gets nothing. An 8 MB document therefore travels as an
    # INCR announcement plus 64 KiB chunks on PropertyNotify, in both
    # directions, and the proof is a second client — xclip — exchanging the
    # full byte count through it:
    #   1. xclip owns an 8 MB CLIPBOARD; the editor pastes it (our read side
    #      consumes an INCR ownership and reassembles) and Ctrl+S must leave
    #      exactly that byte count on disk.
    #   2. Ctrl+A + Ctrl+C make the editor own that text; `xclip -o` (our
    #      write side announced INCR) must read out the same byte count.
    if ! command -v xclip >/dev/null 2>&1; then
        echo "FAIL: xclip not installed (needed as the second clipboard client)"
        exit 1
    fi
    big="$work/big.txt"
    yes 01234567 | head -1048576 >"$big"
    expect=$(wc -c <"$big")
    echo "clipboard round trip: $expect bytes"
    xclip -selection clipboard -i "$big"
    sleep 1
    xdotool key ctrl+a
    sleep 0.3
    xdotool key ctrl+v
    sleep 1
    xdotool key ctrl+s
    sleep 1
    got=$(wc -c <"$work/file.md")
    if [ "$got" != "$expect" ]; then
        echo "FAIL: pasting the 8 MB selection did not reassemble it (file is $got bytes, selection is $expect)"
        echo "its output was:"
        cat "$log"
        exit 1
    fi
    echo "OK: pasted the whole $expect-byte selection in"
    xdotool key ctrl+a
    sleep 0.3
    xdotool key ctrl+c
    sleep 1
    out=$(xclip -selection clipboard -o | wc -c)
    if [ "$out" != "$expect" ]; then
        echo "FAIL: the copy out of the editor reached xclip at $out bytes of $expect (the owner or requestor wrote through the request-size cap?)"
        echo "its output was:"
        cat "$log"
        exit 1
    fi
    echo "OK: xclip read all $expect bytes of the copy back"
    # The app has to stay up for the clipboard leg above: a dead owner answers
    # no selection request. It goes now that both proofs are done.
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
