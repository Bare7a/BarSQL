#!/usr/bin/env bash
# Builds the Linux release files in target/package/dist. Needs cargo-deb and cargo-generate-rpm, and LINUXDEPLOY
# for the AppImage. release.yml builds the Arch package separately.
# Only the tarball has "linux" in its name, so portable copies never update from the other files.
set -euo pipefail

root_dir="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$root_dir/Cargo.toml" | head -n1)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "no X.Y.Z version in Cargo.toml" >&2; exit 1; }
case "$(uname -m)" in
  x86_64) arch=amd64 appimage_arch=x86_64 ;;
  aarch64) arch=arm64 appimage_arch=aarch64 ;;
  *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
out="$root_dir/target/package/linux"
dist="$root_dir/target/package/dist"

cargo build --manifest-path "$root_dir/Cargo.toml" --release --locked -p barsql --features production

rm -rf "$out"
mkdir -p "$out" "$dist"
install -m 0755 "$root_dir/target/release/BarSQL" "$out/BarSQL"
rm -f "$dist/BarSQL-linux-$arch.tar.gz" "$dist/BarSQL" "$dist"/BarSQL.{deb,rpm} "$dist/BarSQL-$appimage_arch.AppImage"
tar -C "$out" -czf "$dist/BarSQL-linux-$arch.tar.gz" BarSQL
cp "$out/BarSQL" "$dist/BarSQL"

cd "$root_dir"
cargo deb -p barsql --no-build --no-strip --output "$dist/BarSQL.deb"
cargo generate-rpm -p crates/barsql --output "$dist/BarSQL.rpm"

if [ -n "${LINUXDEPLOY:-}" ]; then
  appdir="$out/BarSQL.AppDir"
  cp packaging/linux/barsql-256.png "$out/BarSQL.png"
  cp packaging/linux/BarSQL.desktop "$out/BarSQL.desktop"
  (
    cd "$out"
    ARCH="$appimage_arch" LDAI_OUTPUT="$dist/BarSQL-$appimage_arch.AppImage" OUTPUT="$dist/BarSQL-$appimage_arch.AppImage" \
      "$LINUXDEPLOY" --appdir "$appdir" --executable BarSQL --desktop-file BarSQL.desktop --icon-file BarSQL.png \
      --output appimage
  )
else
  echo "LINUXDEPLOY is not set; skipping the AppImage"
fi

echo "BarSQL $version ($arch)"
ls -l "$dist"
