#!/usr/bin/env bash
# Builds the updater zips, each with BarSQL.app for one architecture, and the universal dmg in target/package/dist.
# BARSQL_NATIVE_ONLY=1 builds only this Mac's architecture and its zip.
# BARSQL_MACOS_IDENTITY, the name or SHA-1 hash of a Developer ID Application identity in the keychain, signs with the
# hardened runtime instead of ad-hoc. Adding BARSQL_NOTARY_KEY (an App Store Connect API key's .p8 file), BARSQL_NOTARY_KEY_ID and
# BARSQL_NOTARY_ISSUER has Apple notarize the apps and the dmg, and staples their tickets.
set -euo pipefail

root_dir="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$root_dir/Cargo.toml" | head -n1)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "no X.Y.Z version in Cargo.toml" >&2; exit 1; }
out="$root_dir/target/package/macos"
dist="$root_dir/target/package/dist"
app="$out/BarSQL.app"

identity="${BARSQL_MACOS_IDENTITY:-}"
notarizing=""
if [ -n "$identity" ] && [ -n "${BARSQL_NOTARY_KEY:-}" ]; then
  notarizing=1
  notary=(
    --key "$BARSQL_NOTARY_KEY"
    --key-id "${BARSQL_NOTARY_KEY_ID:?set it to the Key ID of the BARSQL_NOTARY_KEY API key}"
    --issuer "${BARSQL_NOTARY_ISSUER:?set it to the Issuer ID of the BARSQL_NOTARY_KEY API key}"
  )
fi

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

# The entitlements let the hardened runtime load SQLite extensions, which other developers sign or leave ad-hoc.
sign_app() {
  if [ -n "$identity" ]; then
    codesign --force --options runtime --timestamp --entitlements "$root_dir/packaging/macos/BarSQL.entitlements" \
      --sign "$identity" "$1"
    codesign --verify --strict "$1"
  else
    codesign --deep --sign - "$1"
  fi
}

# Waits for Apple's verdict and prints its log when the submission isn't accepted.
notarize() {
  local result id
  result="$(xcrun notarytool submit "$1" "${notary[@]}" --wait --timeout 30m --output-format json)" || true
  [ "$(plutil -extract status raw -o - - <<<"$result" 2>/dev/null)" = Accepted ] && return
  echo "notarizing $(basename "$1") failed: ${result:-no response}" >&2
  id="$(plutil -extract id raw -o - - <<<"$result" 2>/dev/null)" && xcrun notarytool log "$id" "${notary[@]}" >&2
  exit 1
}

# A new ticket can take a moment to reach Apple's servers.
staple() {
  for _ in 1 2 3; do
    xcrun stapler staple "$1" && return
    sleep 20
  done
  echo "stapling $1 failed" >&2
  exit 1
}

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
  sign_app "$dest"
}

rm -rf "$out"
mkdir -p "$dist"
make_app "$app" "${binaries[@]}"
arches=()
for binary in "${binaries[@]}"; do
  arch="$(lipo -archs "$binary" | sed 's/x86_64/amd64/')"
  make_app "$out/$arch/BarSQL.app" "$binary"
  arches+=("$arch")
done

if [ -n "$notarizing" ]; then
  # One submission covers every app. The stapled ticket lets each open without asking Apple first.
  submission="$root_dir/target/package/macos-notarize.zip"
  ditto -c -k --keepParent "$out" "$submission"
  notarize "$submission"
  rm "$submission"
  staple "$app"
  for arch in "${arches[@]}"; do
    staple "$out/$arch/BarSQL.app"
  done
fi

# The updater downloads the zip for its own architecture, so each zip carries that one alone.
rm -f "$dist"/BarSQL-darwin-*.zip "$dist/BarSQL-macos-universal.dmg"
for arch in "${arches[@]}"; do
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

if [ -n "$identity" ]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
  if [ -n "$notarizing" ]; then
    notarize "$dmg"
    staple "$dmg"
  fi
fi

# The certificate's name, whether BARSQL_MACOS_IDENTITY gave its name or its hash.
signer="$(codesign -dvv "$app" 2>&1 | awk '/^Authority=/ && !seen++ { sub(/^Authority=/, ""); print }')"
echo "BarSQL $version: $(lipo -archs "$app/Contents/MacOS/BarSQL"), signed ${signer:+by }${signer:-ad-hoc}${notarizing:+ and notarized}"
ls -l "$dist"
