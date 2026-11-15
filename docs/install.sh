#!/bin/sh
# Installs BarSQL on Linux: the .deb, .rpm or Arch package on x86-64 with apt, dnf, zypper or pacman,
# otherwise the portable build in your home folder. Every download is checked against SHA256SUMS.
#   curl -fsSL https://barsql.bare7a.eu/install.sh | sh
#   curl -fsSL https://barsql.bare7a.eu/install.sh | sh -s -- --portable
set -eu

slug=Bare7a/BarSQL
repo=https://github.com/$slug
site=https://barsql.bare7a.eu
# release.yml fails a build whose Linux binary needs a newer glibc than this.
glibc_min=2.35

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

has() {
  command -v "$1" >/dev/null 2>&1
}

need() {
  for cmd in "$@"; do
    has "$cmd" || fail "BarSQL's installer needs $cmd"
  done
}

# Compares major.minor with glibc_min; a version it can't read counts as too old.
glibc_new_enough() {
  major=${1%%.*} minor=${1#*.}
  minor=${minor%%.*}
  {
    [ "$major" -gt "${glibc_min%%.*}" ] ||
      { [ "$major" -eq "${glibc_min%%.*}" ] && [ "$minor" -ge "${glibc_min#*.}" ]; }
  } 2>/dev/null
}

# fetch <url> <out> [bar]: bar shows curl's progress bar when stderr is a terminal.
fetch() {
  if [ "$downloader" = curl ]; then
    quiet=-s
    [ "${3:-}" = bar ] && [ -t 2 ] && quiet=--progress-bar
    curl --proto '=https' --tlsv1.2 -fSL "$quiet" -o "$2" "$1" || { rm -f "$2"; return 1; }
  else
    wget --https-only -q -O "$2" "$1" || { rm -f "$2"; return 1; }
  fi
}

# The releases/latest redirect names the newest tag without the API's rate limit.
latest_tag() {
  if [ "$downloader" = curl ]; then
    url=$(curl --proto '=https' --tlsv1.2 -fsSI -o /dev/null -w '%{redirect_url}' "$repo/releases/latest") || return 1
  else
    url=$(wget --https-only --max-redirect=0 -S --spider "$repo/releases/latest" 2>&1 |
      tr -d '\r' | sed -n 's/^ *[Ll]ocation: *\([^ ]*\).*/\1/p' | head -n 1)
  fi
  printf '%s\n' "${url##*/}" | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$'
}

# Reads names the way the app's updater does: a leading * or ./ is dropped, and comments are skipped.
listed_sha256() {
  while read -r sum name; do
    case $sum in '' | '#'*) continue ;; esac
    name=${name#\*}
    name=${name#./}
    if [ "$name" = "$1" ]; then
      printf '%s\n' "$sum"
      return 0
    fi
  done <"$tmp/SHA256SUMS"
  return 1
}

verify_sha256() {
  expected=$(listed_sha256 "$1") || fail "$1 isn't listed in SHA256SUMS"
  [ "$(sha256sum "$tmp/$1" | cut -d ' ' -f 1)" = "$expected" ] ||
    fail "$1 doesn't match its SHA-256 in SHA256SUMS; nothing was installed"
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
  # getconf decides, since a glibc system can have musl installed alongside it.
  libc=$(getconf GNU_LIBC_VERSION 2>/dev/null) || libc=
  glibc=${libc##* }
  if [ -z "$glibc" ]; then
    if ldd --version 2>&1 | grep -qi musl || ls /lib/ld-musl-* >/dev/null 2>&1; then
      fail "BarSQL needs glibc, and this system uses musl"
    fi
    fail "BarSQL needs glibc $glibc_min or later, and this system's version couldn't be found"
  fi
  glibc_new_enough "$glibc" ||
    fail "BarSQL needs glibc $glibc_min or later (Ubuntu 22.04, Debian 12, Fedora 36, RHEL 10), and this system has $glibc"
  [ "$(getconf LONG_BIT 2>/dev/null)" = 64 ] ||
    fail "BarSQL needs 64-bit Linux, and this system runs 32-bit programs"
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

# The version the package manager has installed, without its release suffix, or nothing.
installed_version() {
  case $1 in
    apt-get)
      v=$(dpkg-query -W -f "\${Status} \${Version}" barsql 2>/dev/null) || v=
      case $v in "install ok installed "*) v=${v##* } ;; *) v= ;; esac
      ;;
    dnf | zypper) v=$(rpm -q --qf '%{VERSION}' barsql 2>/dev/null) || v= ;;
    pacman)
      v=$(pacman -Q barsql 2>/dev/null) || v=
      v=${v#barsql }
      ;;
  esac
  printf '%s\n' "${v%-*}"
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
  if [ "$(installed_version "$package")" = "$version" ]; then
    say "BarSQL $version is already installed."
    exit 0
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
    say "$package runs with $as_root, which may ask for your password."
    # Asked before the download, so someone sudo refuses doesn't fetch the package for nothing.
    if [ "$as_root" = sudo ] && ! sudo -v; then
      say "sudo turned you away, so BarSQL goes in your home folder."
      as_root=
      return 0
    fi
  fi
  tool=$package asset=$file
}

install_package() {
  path=$tmp/$asset
  case $tool in
    # Unlike apt, apt-get and zypper give up at once on a held lock, as on a fresh Ubuntu's first updates.
    apt-get) set -- env DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 install -y "$path" ;;
    dnf) set -- dnf install -y "$path" ;;
    zypper) set -- env ZYPP_LOCK_TIMEOUT=300 zypper --non-interactive install --allow-unsigned-rpm "$path" ;;
    pacman) set -- pacman -U --noconfirm --needed "$path" ;;
  esac
  ${as_root:+"$as_root"} "$@" </dev/null ||
    fail "$tool couldn't install $asset. Fix the error above and run this again, or install BarSQL in your home folder instead:
  curl -fsSL $site/install.sh | sh -s -- --portable"
  say "BarSQL $version is installed. Open it from your app menu, or run BarSQL."
}

# The menu entry quotes the path, and these characters would need escaping inside the quotes.
add_menu_entry() {
  case $dir in *\"* | *\`* | *\$* | *\\* | *%*) return 1 ;; esac
  raw=https://raw.githubusercontent.com/$slug/$tag/packaging/linux
  sizes='128 256 512'
  fetch "$raw/BarSQL.desktop" "$tmp/BarSQL.desktop" || return 1
  for size in $sizes; do
    fetch "$raw/barsql-$size.png" "$tmp/barsql-$size.png" || return 1
  done
  mkdir -p "$data/applications" || return 1
  while IFS= read -r line || [ -n "$line" ]; do
    case $line in
      Exec=*) printf 'Exec="%s/BarSQL" %%f\n' "$dir" ;;
      *) printf '%s\n' "$line" ;;
    esac
  done <"$tmp/BarSQL.desktop" >"$data/applications/BarSQL.desktop" || return 1
  for size in $sizes; do
    icons=$data/icons/hicolor/${size}x$size/apps
    mkdir -p "$icons" && cp "$tmp/barsql-$size.png" "$icons/BarSQL.png" || return 1
  done
  if has update-desktop-database; then
    update-desktop-database -q "$data/applications" 2>/dev/null || true
  fi
}

# Under sudo the copy would land in root's home, and an older copy at ~/.local/bin keeps its data beside it.
check_portable() {
  if [ "$(id -u)" = 0 ] && [ -n "${SUDO_USER:-}${DOAS_USER:-}" ]; then
    fail "the portable build installs for one user, so run this without sudo or doas"
  fi
  data=${XDG_DATA_HOME:-$HOME/.local/share}
  bin=$HOME/.local/bin
  dir=$data/barsql
  if { [ -e "$bin/BarSQL" ] || [ -L "$bin/BarSQL" ]; } && [ "$(readlink "$bin/BarSQL")" != "$dir/BarSQL" ]; then
    old=$(readlink -f "$bin/BarSQL") || old=$bin/BarSQL
    hint=
    [ -d "${old%/*}/BarSQL-data" ] && hint=" To keep its connections, first move ${old%/*}/BarSQL-data to $dir/BarSQL-data."
    fail "$bin/BarSQL is another copy of BarSQL. Move it out of the way and run this again.$hint"
  fi
}

# SHA256SUMS lists the bare x86-64 binary the tarball holds, so other chips always download.
portable_current() {
  [ "$arch" = amd64 ] && [ -f "$dir/BarSQL" ] && [ "$(readlink "$bin/BarSQL")" = "$dir/BarSQL" ] &&
    expected=$(listed_sha256 BarSQL) && [ "$(sha256sum "$dir/BarSQL" | cut -d ' ' -f 1)" = "$expected" ]
}

install_portable() {
  need tar
  mkdir -p "$dir" "$bin"
  tar -xzf "$tmp/$asset" -O BarSQL >"$dir/.BarSQL.new" || { rm -f "$dir/.BarSQL.new"; fail "couldn't unpack $asset"; }
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
  if has curl; then
    downloader=curl
  elif has wget; then
    downloader=wget
  else
    fail "BarSQL's installer needs curl or wget"
  fi
  need sha256sum mktemp
  tag=$(latest_tag) || fail "couldn't find BarSQL's latest release on $repo/releases"
  version=${tag#v}
  choose_install
  if [ "$tool" = portable ]; then
    check_portable
  fi

  tmp=$(mktemp -d "${TMPDIR:-/tmp}/barsql-install.XXXXXX") || fail "couldn't create a temporary folder"
  trap 'rm -rf "$tmp"' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  # apt reads the package as its _apt user.
  chmod 755 "$tmp"

  base=$repo/releases/download/$tag
  fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || fail "couldn't download $base/SHA256SUMS"
  if [ "$tool" = portable ] && portable_current; then
    say "BarSQL $version is already installed in $dir."
    exit 0
  fi
  say "Installing BarSQL $version from $asset"
  fetch "$base/$asset" "$tmp/$asset" bar || fail "couldn't download $base/$asset"
  verify_sha256 "$asset"
  if [ "$tool" = portable ]; then
    install_portable
  else
    install_package
  fi
}

main "$@"
