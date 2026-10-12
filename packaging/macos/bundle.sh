#!/bin/sh
set -e
cargo build --release
APP=target/Readitor.app
rm -rf "$APP"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/readitor "$APP/Contents/MacOS/readitor"
cp packaging/macos/Info.plist "$APP/Contents/Info.plist"
# TODO: icon (.icns) into Resources
echo "Built $APP"
