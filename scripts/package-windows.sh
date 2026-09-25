#!/usr/bin/env bash
# Builds the updater zip, the bare exe and the NSIS installer in target/package/dist.
# Runs in Git Bash and needs makensis on PATH.
set -euo pipefail

root_dir="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$root_dir/Cargo.toml" | head -n1)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "no X.Y.Z version in Cargo.toml" >&2; exit 1; }
out="$root_dir/target/package/windows"
dist="$root_dir/target/package/dist"

cargo build --manifest-path "$root_dir/Cargo.toml" --release --locked -p barsql --features production

rm -rf "$out"
mkdir -p "$out" "$dist"
cp "$root_dir/target/release/BarSQL.exe" "$out/BarSQL.exe"
rm -f "$dist/BarSQL-windows-amd64.zip" "$dist/BarSQL.exe" "$dist/BarSQL-amd64-installer.exe"
(cd "$out" && 7z a -tzip -bso0 "$dist/BarSQL-windows-amd64.zip" BarSQL.exe)
cp "$out/BarSQL.exe" "$dist/BarSQL.exe"

MSYS_NO_PATHCONV=1 makensis -V2 \
  "-DARG_BINARY=$(cygpath -w "$out/BarSQL.exe")" \
  "-DINFO_VERSION=$version" \
  "-DOUTFILE=$(cygpath -w "$dist/BarSQL-amd64-installer.exe")" \
  "$(cygpath -w "$root_dir/packaging/windows/installer.nsi")"

echo "BarSQL $version"
ls -l "$dist"
