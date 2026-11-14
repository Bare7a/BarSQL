#!/bin/sh
# Installs BarSQL on Linux: the .deb, .rpm or Arch package on x86-64 with apt, dnf, zypper or pacman,
# otherwise the portable build in your home folder. Every download is checked against SHA256SUMS.
#   curl -fsSL https://barsql.bare7a.eu/install.sh | sh
#   curl -fsSL https://barsql.bare7a.eu/install.sh | sh -s -- --portable
set -eu

repo=https://github.com/Bare7a/BarSQL
site=https://barsql.bare7a.eu

say() {
  printf '%s\n' "$*"
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<EOF
Installs BarSQL on Linux: the package on x86-64 with apt, dnf, zypper or pacman,
otherwise the portable build in your home folder.

Usage: curl -fsSL $site/install.sh | sh [-s -- --portable]

  --portable  Install the portable build in your home folder, without sudo
  -h, --help  Show this help
EOF
}

version_at_least() {
  have=$1 want=$2
  while [ -n "$have$want" ]; do
    h=${have%%.*} w=${want%%.*}
    [ "${h:-0}" -gt "${w:-0}" ] && return 0
    [ "${h:-0}" -lt "${w:-0}" ] && return 1
    case $have in *.*) have=${have#*.} ;; *) have= ;; esac
    case $want in *.*) want=${want#*.} ;; *) want= ;; esac
  done
  return 0
}

# fetch <url> <out> [bar]: bar shows curl's progress bar when stderr is a terminal.
fetch() {
  if command -v curl >/dev/null 2>&1; then
    quiet=-s
    [ "${3:-}" = bar ] && [ -t 2 ] && quiet=--progress-bar
    curl --proto '=https' --tlsv1.2 -fSL "$quiet" -o "$2" "$1" && return 0
  elif command -v wget >/dev/null 2>&1; then
    wget --https-only -q -O "$2" "$1" && return 0
  fi
  rm -f "$2"
  return 1
}

# The releases/latest redirect names the newest tag without the API's rate limit.
latest_tag() {
  if command -v curl >/dev/null 2>&1; then
    url=$(curl --proto '=https' --tlsv1.2 -fsSLI -o /dev/null -w '%{url_effective}' "$repo/releases/latest") || return 1
  else
    url=$(wget --https-only --max-redirect=0 -S --spider "$repo/releases/latest" 2>&1 |
      tr -d '\r' | sed -n 's/^ *[Ll]ocation: *\([^ ]*\).*/\1/p' | head -n 1)
  fi
  printf '%s\n' "${url##*/}" | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$'
}

verify_sha256() {
  expected=
  while read -r sum name; do
    if [ "$name" = "$2" ] || [ "$name" = "*$2" ]; then
      expected=$sum
      break
    fi
  done <"$3"
  if [ -z "$expected" ]; then
    printf "error: %s isn't listed in SHA256SUMS\n" "$2" >&2
    return 1
  fi
  actual=$(sha256sum "$1" | cut -d ' ' -f 1)
  if [ "$actual" != "$expected" ]; then
    printf "error: %s doesn't match its SHA-256 in SHA256SUMS; nothing was installed\n" "$2" >&2
    return 1
  fi
}

check_platform() {
  os=$(uname -s)
  if [ "$os" != Linux ]; then
    hint=
    [ "$os" = Darwin ] && hint=" or run: brew install bare7a/tap/barsql"
    fail "this script installs BarSQL on Linux. Download it for this system from $site/#download$hint"
  fi
  machine=$(uname -m)
  case $machine in
    x86_64 | amd64) arch=amd64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *) fail "BarSQL runs on x86-64 and ARM64 Linux, not $machine" ;;
  esac
  if ldd --version 2>&1 | grep -qi musl || ls /lib/ld-musl-* >/dev/null 2>&1; then
    fail "BarSQL needs glibc, and this system uses musl"
  fi
  libc=$(getconf GNU_LIBC_VERSION 2>/dev/null) || libc=
  glibc=${libc##* }
  [ -n "$glibc" ] || fail "BarSQL needs glibc 2.35 or later, and this system's version couldn't be found"
  version_at_least "$glibc" 2.35 ||
    fail "BarSQL needs glibc 2.35 or later (Ubuntu 22.04, Debian 12, Fedora 36, RHEL 10), and this system has $glibc"
}

need() {
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null 2>&1 || fail "BarSQL's installer needs $cmd"
  done
}

has() {
  command -v "$1" >/dev/null 2>&1
}

# Atomic and ostree systems mount /usr read-only, and SteamOS mounts / that way.
usr_read_only() {
  [ -e /run/ostree-booted ] && return 0
  usr='' root=''
  if [ -r /proc/mounts ]; then
    while read -r _ mnt _ opts _; do
      case $mnt in
        /usr) usr=$opts ;;
        /) root=$opts ;;
      esac
    done </proc/mounts
  fi
  case ,${usr:-$root}, in *,ro,*) return 0 ;; esac
  return 1
}

choose_install() {
  tool=portable asset=BarSQL-linux-$arch.tar.gz as_root=
  if [ "$arch" != amd64 ] || [ -n "$portable" ]; then
    return 0
  fi
  if usr_read_only; then
    say "/usr is read-only here, so BarSQL goes in your home folder."
    return 0
  fi
  if has apt-get && has dpkg; then
    package=apt-get file=BarSQL.deb
  elif has dnf; then
    package=dnf file=BarSQL.rpm
  elif has zypper; then
    package=zypper file=BarSQL.rpm
  elif has pacman; then
    package=pacman file=BarSQL.pkg.tar.zst
  else
    return 0
  fi
  if [ "$(id -u)" != 0 ]; then
    if has sudo; then
      as_root=sudo
    elif has doas; then
      as_root=doas
    else
      say "No sudo or doas, so BarSQL goes in your home folder."
      return 0
    fi
  fi
  tool=$package asset=$file
}

install_package() {
  path=$tmp/$asset
  case $tool in
    apt-get) set -- env DEBIAN_FRONTEND=noninteractive apt-get install -y "$path" ;;
    dnf) set -- dnf install -y "$path" ;;
    zypper) set -- zypper --non-interactive install --allow-unsigned-rpm "$path" ;;
    pacman) set -- pacman -U --noconfirm --needed "$path" ;;
  esac
  [ -n "$as_root" ] && say "$tool runs with $as_root, which may ask for your password."
  ${as_root:+"$as_root"} "$@" </dev/null ||
    fail "$tool couldn't install $asset. To install BarSQL in your home folder instead, run:
  curl -fsSL $site/install.sh | sh -s -- --portable"
  say "BarSQL $version is installed. Open it from your app menu, or run BarSQL."
}

# The menu entry quotes the path, and these characters would need escaping inside the quotes.
add_menu_entry() {
  case $dir in *\"* | *\`* | *\$* | *\\* | *%*) return 1 ;; esac
  raw=https://raw.githubusercontent.com/Bare7a/BarSQL/$tag/packaging/linux
  fetch "$raw/BarSQL.desktop" "$tmp/BarSQL.desktop" || return 1
  for size in 128 256 512; do
    fetch "$raw/barsql-$size.png" "$tmp/barsql-$size.png" || return 1
  done
  mkdir -p "$data/applications" || return 1
  while IFS= read -r line || [ -n "$line" ]; do
    case $line in
      Exec=*) printf 'Exec="%s/BarSQL" %%f\n' "$dir" ;;
      *) printf '%s\n' "$line" ;;
    esac
  done <"$tmp/BarSQL.desktop" >"$data/applications/BarSQL.desktop" || return 1
  for size in 128 256 512; do
    icons=$data/icons/hicolor/${size}x$size/apps
    mkdir -p "$icons" && cp "$tmp/barsql-$size.png" "$icons/BarSQL.png" || return 1
  done
  if has update-desktop-database; then
    update-desktop-database -q "$data/applications" 2>/dev/null || true
  fi
}

install_portable() {
  need tar
  data=${XDG_DATA_HOME:-$HOME/.local/share}
  bin=$HOME/.local/bin
  dir=$data/barsql
  tar -xzf "$tmp/$asset" -C "$tmp" BarSQL || fail "couldn't unpack $asset"
  mkdir -p "$dir" "$bin"
  cp "$tmp/BarSQL" "$dir/.BarSQL.new"
  chmod 755 "$dir/.BarSQL.new"
  # A rename keeps a running BarSQL on its old file.
  mv -f "$dir/.BarSQL.new" "$dir/BarSQL"
  ln -sfn "$dir/BarSQL" "$bin/BarSQL"

  open="Open it from your app menu, or run BarSQL."
  if ! add_menu_entry; then
    say "warning: couldn't add the menu entry"
    open="Run it with BarSQL."
  fi
  say "BarSQL $version is installed in $dir. $open"
  case :$PATH: in
    *":$bin:"*) ;;
    *) say "To run BarSQL from a terminal, add $bin to your PATH." ;;
  esac
  libs=
  for lib in $(ldd "$dir/BarSQL" 2>/dev/null | sed -n 's/^[[:space:]]*\([^[:space:]]*\) => not found.*/\1/p'); do
    libs="${libs:+$libs, }$lib"
  done
  if [ -n "$libs" ]; then
    say "BarSQL needs these libraries, which aren't installed: $libs. Install them with your package manager."
  fi
}

main() {
  portable=
  for arg in "$@"; do
    case $arg in
      --portable) portable=1 ;;
      -h | --help)
        usage
        exit 0
        ;;
      *)
        printf 'error: unknown option %s\n\n' "$arg" >&2
        usage >&2
        exit 1
        ;;
    esac
  done
  check_platform
  has curl || has wget || fail "BarSQL's installer needs curl or wget"
  need sha256sum mktemp
  tag=$(latest_tag) || fail "couldn't find BarSQL's latest release on $repo/releases"
  version=${tag#v}
  choose_install

  tmp=$(mktemp -d "${TMPDIR:-/tmp}/barsql-install.XXXXXX") || fail "couldn't create a temporary folder"
  trap 'rm -rf "$tmp"' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  # apt reads the package as its _apt user.
  chmod 755 "$tmp"

  say "Installing BarSQL $version from $asset"
  base=$repo/releases/download/$tag
  fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || fail "couldn't download $base/SHA256SUMS"
  fetch "$base/$asset" "$tmp/$asset" bar || fail "couldn't download $base/$asset"
  verify_sha256 "$tmp/$asset" "$asset" "$tmp/SHA256SUMS" || exit 1
  if [ "$tool" = portable ]; then
    install_portable
  else
    install_package
  fi
}

main "$@"
