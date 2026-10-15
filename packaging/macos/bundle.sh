#!/bin/sh
# Build the macOS app bundle: Editor.app with the binary, the app icon, and an
# Info.plist whose version is filled in from Cargo.toml.
#
# `packaging/macos/Info.plist` is a TEMPLATE: it carries the literal token
# __VERSION__ where the version goes, so the bundle version cannot drift from
# the version the binary reports for itself. Every placeholder is substituted
# here; the resulting Info.plist inside the bundle always has a real version.
set -e

cargo build --release

version=$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -1)
if [ -z "$version" ]; then
    echo "bundle.sh: no version found in Cargo.toml" >&2
    exit 1
fi

APP=target/Editor.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/editor "$APP/Contents/MacOS/editor"
sed "s/__VERSION__/$version/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
# CFBundleIconFile names this file; without it the Finder draws a generic
# application icon.
cp assets/icon.icns "$APP/Contents/Resources/Editor.icns"

echo "Built $APP (version $version)"
