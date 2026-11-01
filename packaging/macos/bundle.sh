#!/bin/sh
# Build the macOS app bundle: Editor.app with the binary, the app icon, and an
# Info.plist whose version is filled in from Cargo.toml.
#
# `packaging/macos/Info.plist` is a TEMPLATE: it carries the literal token
# __VERSION__ where the version goes, so the bundle version cannot drift from
# the version the binary reports for itself. Every placeholder is substituted
# here; the resulting Info.plist inside the bundle always has a real version.
#
# The bundle is UNIVERSAL (Intel + Apple silicon): both slices are built here
# and `lipo -create` merges them into the one executable before the zip the
# release workflow uploads is made.
# Two pins make that honest:
#
#   MACOSX_DEPLOYMENT_TARGET is set identically for both slices. Without it
#   rustc picks a per-target default (12.0 now for arm64), the older x86_64
#   minimum would end up above the newer slice's floor, and the plist's
#   LSMinimumSystemVersion could only claim whichever lie was smaller.
#
#   rustup target add is idempotent and cheap when the target is present, so
#   it protects the checkout path too: a from-source `bundle.sh` run on an
#   Intel Mac or in CI must not depend on which targets rustup happens to
#   have installed.
#
set -e

rustup target add x86_64-apple-darwin aarch64-apple-darwin
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-12.0}

cargo build --release --target x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin

version=$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -1)
if [ -z "$version" ]; then
    echo "bundle.sh: no version found in Cargo.toml" >&2
    exit 1
fi

APP=target/Editor.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create \
    -output target/Editor-universal \
    target/x86_64-apple-darwin/release/editor \
    target/aarch64-apple-darwin/release/editor
cp target/Editor-universal "$APP/Contents/MacOS/editor"
sed "s/__VERSION__/$version/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
# CFBundleIconFile names this file; without it the Finder draws a generic
# application icon.
cp assets/icon.icns "$APP/Contents/Resources/Editor.icns"

echo "Built $APP (version $version)"
# The universal claim is verified, not assumed: this prints
#   Architectures in the fat file: ... are: x86_64 arm64
# and exits nonzero if the merge did not produce a two-slice binary.
lipo -info "$APP/Contents/MacOS/editor"
lipo -info "$APP/Contents/MacOS/editor" | grep -q \
    -e 'x86_64[[:space:]]*arm64' -e 'arm64[[:space:]]*x86_64' \
    || { echo "bundle.sh: the bundle is not universal (x86_64 + arm64)" >&2; exit 1; }
