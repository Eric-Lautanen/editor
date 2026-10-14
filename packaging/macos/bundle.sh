#!/bin/sh
set -e
cargo build --release
APP=target/Editor.app
rm -rf "$APP"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/editor "$APP/Contents/MacOS/editor"
cp packaging/macos/Info.plist "$APP/Contents/Info.plist"
# TODO: icon (.icns) into Resources
echo "Built $APP"
