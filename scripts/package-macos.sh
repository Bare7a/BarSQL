#!/usr/bin/env bash
# Builds the updater zips, each with BarSQL.app for one architecture, and the universal dmg in target/package/dist.
# BARSQL_NATIVE_ONLY=1 builds only this Mac's architecture and its zip.
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

make_app() {
  local dest="$1"
  shift
  mkdir -p "$dest/Contents/MacOS" "$dest/Contents/Resources"
  lipo -create -output "$dest/Contents/MacOS/BarSQL" "$@"
  cp "$root_dir/packaging/macos/icons.icns" "$dest/Contents/Resources/icons.icns"
  cp "$root_dir/packaging/macos/Info.plist" "$dest/Contents/Info.plist"
  /usr/libexec/PlistBuddy \
    -c "Set :CFBundleShortVersionString $version" \
    -c "Set :CFBundleVersion $version" \
    "$dest/Contents/Info.plist"
  codesign --deep --sign - "$dest"
}

rm -rf "$out"
mkdir -p "$dist"
make_app "$app" "${binaries[@]}"

# The updater downloads the zip for its own architecture, so each zip carries that one alone.
rm -f "$dist"/BarSQL-darwin-*.zip "$dist/BarSQL-macos-universal.dmg"
for binary in "${binaries[@]}"; do
  arch="$(lipo -archs "$binary" | sed 's/x86_64/amd64/')"
  make_app "$out/$arch/BarSQL.app" "$binary"
  (cd "$out/$arch" && zip -r -y -X "$dist/BarSQL-darwin-$arch.zip" "BarSQL.app" >/dev/null)
done

# No platform or arch in the dmg name, so the updater never picks it.
dmg="$dist/BarSQL-macos-universal.dmg"
staging="$out/dmg"
mkdir -p "$staging"
ditto "$app" "$staging/BarSQL.app"
ln -s /Applications "$staging/Applications"

if [ "$(sw_vers -productVersion | cut -d. -f1)" -ge 27 ]; then
  make_dmg=(diskutil image create from --format ULMO --volumeName "BarSQL" "$staging" "$dmg")
else
  size_mb=$(($(du -sAm "$staging" | cut -f1) * 2))
  make_dmg=(hdiutil create -size "${size_mb}m" -volname "BarSQL" -srcfolder "$staging" -ov -format ULMO "$dmg")
fi
for attempt in 1 2 3; do
  "${make_dmg[@]}" >/dev/null && break
  [ "$attempt" = 3 ] && { df -h "$dist" >&2; exit 1; }
  rm -f "$dmg"
  sleep 10
done
rm -rf "$staging"

echo "BarSQL $version: $(lipo -archs "$app/Contents/MacOS/BarSQL")"
ls -l "$dist"
