#!/bin/sh
# Builds Spectra.app, with its icon, around the release binary, and zips it.
#
#   packaging/macos/build-app.sh [--skip-build]
#
# Writes target/release/Spectra.app and target/release/Spectra-macos.zip.
set -eu
cd "$(dirname "$0")/../.."

[ "${1:-}" = "--skip-build" ] || cargo build --release --locked

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
app=target/release/Spectra.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/spectra "$app/Contents/MacOS/Spectra"
cp packaging/macos/Spectra.icns "$app/Contents/Resources/Spectra.icns"
sed "s/{{VERSION}}/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"

# An ad-hoc signature: Apple silicon won't run an unsigned app. It isn't
# notarised, so the first launch is from right-click > Open.
codesign --force --sign - "$app"

# ditto keeps the bundle's permissions and signature, which a plain zip loses.
ditto -c -k --keepParent "$app" target/release/Spectra-macos.zip
echo "$app"
