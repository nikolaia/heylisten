#!/usr/bin/env bash
# Builds target/heyListen.app (the tray app with the CLI next to it), ad-hoc signed,
# and zips it as target/heyListen-macos-arm64.zip. Used by the release workflow.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release --locked --features tray

app=target/heyListen.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/heylisten-tray target/release/heylisten "$app/Contents/MacOS/"
sed "s/VERSION/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
codesign --force --deep --sign - "$app"

rm -f target/heyListen-macos-arm64.zip
ditto -c -k --keepParent "$app" target/heyListen-macos-arm64.zip
echo "Built $app ($version) and target/heyListen-macos-arm64.zip"
