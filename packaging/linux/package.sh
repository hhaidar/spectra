#!/bin/sh
# Packages Spectra for Linux: the binary, its desktop entry and its icons, laid
# out like an install prefix.
#
#   packaging/linux/package.sh [--skip-build]
#
# Writes target/release/spectra-linux.tar.gz; to install it for yourself:
#
#   tar -xzf spectra-linux.tar.gz -C ~/.local
set -eu
cd "$(dirname "$0")/../.."

[ "${1:-}" = "--skip-build" ] || cargo build --release --locked

root=target/release/spectra-linux
rm -rf "$root"
mkdir -p "$root/bin"
install -m 755 target/release/spectra "$root/bin/spectra"
cp -R packaging/linux/share "$root/share"
tar -czf target/release/spectra-linux.tar.gz -C "$root" bin share
echo target/release/spectra-linux.tar.gz
