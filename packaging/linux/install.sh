#!/bin/sh
# Install Editor for the current user: binary, .desktop entry, MIME package
# for .jsonl/.ndjson, and xdg-mime defaults.
#
# Two layouts are supported, because the release tarball is not a source
# checkout and the local one is not a tarball:
#
#   tarball   ./editor  +  ./packaging/linux/install.sh   (prebuilt binary,
#             no Cargo.toml anywhere — so it must never try to build)
#   checkout  ./Cargo.toml + ./target/release/editor      (build if needed)
#
# Run it as `sh packaging/linux/install.sh` from wherever you unpacked.
set -e

here=$(dirname "$0")
bin=""
for candidate in "$here/../../editor" "$here/editor" ./editor target/release/editor; do
    if [ -x "$candidate" ]; then
        bin=$candidate
        break
    fi
done
if [ -z "$bin" ]; then
    if [ -f Cargo.toml ]; then
        echo "no binary found; building from source"
        cargo build --release
        bin=target/release/editor
    else
        echo "editor: no binary to install next to $here (and no source to build)" >&2
        exit 1
    fi
fi

# `install -D` is GNU coreutils; this script is Linux-only by design (macOS
# uses packaging/macos/bundle.sh).
install -Dm755 "$bin" "$HOME/.local/bin/editor"
install -Dm644 "$here/editor.desktop" "$HOME/.local/share/applications/editor.desktop"
install -Dm644 "$here/editor-mime.xml" "$HOME/.local/share/mime/packages/editor-mime.xml"
update-mime-database "$HOME/.local/share/mime" || true
update-desktop-database "$HOME/.local/share/applications" || true
for t in text/markdown application/json application/x-ndjson text/plain text/csv text/x-yaml application/toml; do
    xdg-mime default editor.desktop "$t" || true
done

# Resolve the source to an absolute path for the message: the candidate may be
# relative (`packaging/linux/../../editor`), which says nothing useful.
abs=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
echo "Installed $abs -> ~/.local/bin/editor"
echo "Registered the .desktop entry and the MIME package for .jsonl/.ndjson"
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) echo "note: ~/.local/bin is not on your PATH — add it to use 'editor' directly" ;;
esac
