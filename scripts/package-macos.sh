#!/usr/bin/env bash
# Builds a universal BarSQL.app, the updater zips and the dmg in target/package/dist.
# BARSQL_NATIVE_ONLY=1 builds only this Mac's architecture and skips the amd64 zip.
set -euo pipefail

root_dir="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$root_dir/Cargo.toml" | head -n1)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "no X.Y.Z version in Cargo.toml" >&2; exit 1; }
out="$root_dir/target/package/macos"
dist="$root_dir/target/package/dist"
app="$out/BarSQL.app"

targets=(aarch64-apple-darwin x86_64-apple-darwin)
if [ "${BARSQL_NATIVE_ONLY:-}" = 1 ]; then
  targets=("$(uname -m | sed 's/arm64/aarch64/')-apple-darwin")
elif ! rustup target list --installed | grep -q '^x86_64-apple-darwin$'; then
  echo "the universal app needs: rustup target add x86_64-apple-darwin (or BARSQL_NATIVE_ONLY=1)" >&2
  exit 1
fi

export MACOSX_DEPLOYMENT_TARGET=12.0
binaries=()
for target in "${targets[@]}"; do
  cargo build --manifest-path "$root_dir/Cargo.toml" --release --locked -p barsql --features production --target "$target"
  binaries+=("$root_dir/target/$target/release/BarSQL")
done

rm -rf "$out"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$dist"
lipo -create -output "$app/Contents/MacOS/BarSQL" "${binaries[@]}"
cp "$root_dir/packaging/macos/icons.icns" "$app/Contents/Resources/icons.icns"
cp "$root_dir/packaging/macos/Info.plist" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy \
  -c "Set :CFBundleShortVersionString $version" \
  -c "Set :CFBundleVersion $version" \
  "$app/Contents/Info.plist"
codesign --force --deep --sign - "$app"

rm -f "$dist"/BarSQL-darwin-*.zip "$dist/BarSQL-macos-universal.dmg"
(cd "$out" && zip -r -y -X "$dist/BarSQL-darwin-arm64.zip" "BarSQL.app" >/dev/null)
if [ "${#targets[@]}" -gt 1 ]; then
  cp "$dist/BarSQL-darwin-arm64.zip" "$dist/BarSQL-darwin-amd64.zip"
fi

# No platform or arch in the dmg name, so the updater never picks it.
staging="$out/dmg"
mkdir -p "$staging"
ditto "$app" "$staging/BarSQL.app"
ln -s /Applications "$staging/Applications"
size_mb=$(($(du -sAm "$staging" | cut -f1) * 2))
for attempt in 1 2 3; do
  hdiutil create -size "${size_mb}m" -volname "BarSQL" -srcfolder "$staging" -ov -format UDZO \
    "$dist/BarSQL-macos-universal.dmg" >/dev/null && break
  [ "$attempt" = 3 ] && { df -h "$dist" >&2; exit 1; }
  sleep 10
done
rm -rf "$staging"

echo "BarSQL $version: $(lipo -archs "$app/Contents/MacOS/BarSQL")"
ls -l "$dist"
