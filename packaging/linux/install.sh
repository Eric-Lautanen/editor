#!/bin/sh
set -e
cargo build --release
install -Dm755 target/release/readitor "$HOME/.local/bin/readitor"
install -Dm644 packaging/linux/readitor.desktop "$HOME/.local/share/applications/readitor.desktop"
install -Dm644 packaging/linux/readitor-mime.xml "$HOME/.local/share/mime/packages/readitor-mime.xml"
update-mime-database "$HOME/.local/share/mime" || true
update-desktop-database "$HOME/.local/share/applications" || true
for t in text/markdown application/json application/x-ndjson; do xdg-mime default readitor.desktop "$t" || true; done
echo "Installed."
