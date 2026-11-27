#!/bin/sh
# Builds the universal macOS release and everything the auto-updater needs, into
# release/v<version>/:
#   LvHu_<v>_universal.dmg              installer for new users
#   LvHu_<v>_universal.app.tar.gz       update package
#   LvHu_<v>_universal.app.tar.gz.sig   its signature
#   latest.json                                what installed apps check for updates
# Upload all four to the GitHub Release v<version> (not a draft, not a pre-release:
# apps look at releases/latest/download/latest.json).
#
# Signing key: ~/.tauri/web-manager.key (override with TAURI_SIGNING_PRIVATE_KEY);
# its password is read from the login Keychain item "web-manager-updater-key"
# (override with TAURI_SIGNING_PRIVATE_KEY_PASSWORD).
#
#   scripts/release-mac.sh "更新说明，会显示在更新提示里"
set -eu
cd "$(dirname "$0")/.."

notes=${1:?usage: scripts/release-mac.sh "release notes"}
version=$(node -p 'require("./src-tauri/tauri.conf.json").version')
repo=https://github.com/mozm-ctrl/web-manager

: "${TAURI_SIGNING_PRIVATE_KEY:=$(cat ~/.tauri/web-manager.key)}"
: "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:=$(security find-generic-password -s web-manager-updater-key -w)}"
export TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD

pnpm tauri build --target universal-apple-darwin

bundle=src-tauri/target/universal-apple-darwin/release/bundle
out=release/v$version
product=$(node -p 'require("./src-tauri/tauri.conf.json").productName')
# ASCII file names: GitHub rewrites non-ASCII release asset names.
name=LvHu_${version}_universal
mkdir -p "$out"
cp "$bundle/dmg/${product}_${version}_universal.dmg" "$out/$name.dmg"
cp "$bundle/macos/$product.app.tar.gz" "$out/$name.app.tar.gz"
cp "$bundle/macos/$product.app.tar.gz.sig" "$out/$name.app.tar.gz.sig"

# One universal package serves both Apple silicon and Intel Macs.
VERSION=$version NOTES=$notes URL="$repo/releases/download/v$version/$name.app.tar.gz" \
SIG=$(cat "$out/$name.app.tar.gz.sig") node -e '
const e = process.env;
const platform = { signature: e.SIG, url: e.URL };
const json = {
  version: e.VERSION,
  notes: e.NOTES,
  pub_date: new Date().toISOString().replace(/\.\d+Z$/, "Z"),
  platforms: { "darwin-aarch64": platform, "darwin-x86_64": platform },
};
require("fs").writeFileSync(process.argv[1], JSON.stringify(json, null, 2) + "\n");
' "$out/latest.json"

ls -l "$out"
