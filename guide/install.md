# ⬇️ Installing BarSQL

BarSQL runs on Windows, macOS and Linux, needs no account, and [updates itself](app.md#-auto-updates) however you install it. [barsql.bare7a.eu](https://barsql.bare7a.eu/#download) offers the download for your system, and the [latest release](https://github.com/Bare7a/BarSQL/releases/latest) has every file. To build it yourself, see [CONTRIBUTING.md](../CONTRIBUTING.md).

## 📦 Which File to Pick

### Windows

| File | What it is |
| ---- | ---------- |
| `BarSQL-windows-amd64.zip` | **Portable** - unzip it anywhere and run `BarSQL.exe` |
| `BarSQL-amd64-installer.exe` | **Installer** - asks for admin rights, puts BarSQL in Program Files with Start menu and desktop shortcuts, and opens `.db`, `.sqlite`, `.sqlite3`, `.s3db` and `.sl3` files |
| `BarSQL.exe` | **Standalone** - the portable `BarSQL.exe` without the zip |

### macOS

| File | What it is |
| ---- | ---------- |
| `BarSQL-macos-universal.dmg` | One app for Apple Silicon and Intel: open it and drag BarSQL to Applications |
| `BarSQL-darwin-arm64.zip` | The app for Apple Silicon alone, half the size |
| `BarSQL-darwin-amd64.zip` | The app for Intel alone |

Every macOS build needs macOS 12 or later.

### Linux

| File | What it is |
| ---- | ---------- |
| `BarSQL-linux-amd64.tar.gz`, `BarSQL-linux-arm64.tar.gz` | **Portable** for x86-64 or ARM64 - unpack it and run `BarSQL` |
| `BarSQL-x86_64.AppImage`, `BarSQL-aarch64.AppImage` | A single file for x86-64 or ARM64 - make it executable and run it |
| `BarSQL.deb`, `BarSQL.rpm`, `BarSQL.pkg.tar.zst` | **Packages** for x86-64: Debian and Ubuntu, Fedora, RHEL and openSUSE, and Arch. Each installs `/usr/bin/BarSQL` and an app menu entry that opens SQLite files |
| `BarSQL` | The x86-64 binary on its own |

Every Linux build needs glibc 2.35 or later: Ubuntu 22.04, Debian 12, Fedora 36, RHEL 10 or newer. The [install script](#linux-install-script) picks the right one for you.

Where each copy keeps your connections and settings is in [Portable by Design](app.md#-portable-by-design). Every release lists each file's SHA-256 in `SHA256SUMS`, and [SECURITY.md](../SECURITY.md#verifying-downloads) shows how to check a file's build provenance.

## 💻 From a Terminal

### Homebrew

On macOS, [Homebrew](https://brew.sh) installs the app for your Mac's chip, and the app keeps updating itself:

```bash
brew install bare7a/tap/barsql
```

Already have BarSQL in Applications? Add `--adopt` to take over that copy.

<!-- winget: uncomment once Microsoft approves Bare7a.BarSQL.
### winget

On Windows, [winget](https://learn.microsoft.com/windows/package-manager/) runs the installer for you:

```powershell
winget install Bare7a.BarSQL
```

-->
### Scoop

On Windows, [Scoop](https://scoop.sh) installs the portable build and keeps its `BarSQL-data` folder from one version to the next:

```powershell
scoop bucket add bare7a https://github.com/Bare7a/scoop-bucket
scoop install bare7a/barsql
```

The app keeps updating itself. Scoop's own list catches up at the next `scoop update barsql`.

### Linux Install Script

One command picks the build for your distro and chip, checks it against `SHA256SUMS` and installs it:

```bash
curl -fsSL https://barsql.bare7a.eu/install.sh | sh
```

On x86-64 with apt, dnf, zypper or pacman, that's the package, which asks for your password once. Anywhere else, ARM64 included, it's the portable build in `~/.local/share/barsql`, with a `BarSQL` command and an app menu entry. To get the portable build anywhere, without sudo:

```bash
curl -fsSL https://barsql.bare7a.eu/install.sh | sh -s -- --portable
```

Run it again to update, though the app updates itself either way.

## 🚀 First Launch

The macOS app is signed with a Developer ID and notarized by Apple, so it opens like any other app.

The Windows build isn't code-signed yet, so Windows asks once before BarSQL first opens: when SmartScreen says *Windows protected your PC*, click **More info → Run anyway**. Smart App Control, where it's turned on, blocks unsigned apps without an exception, so BarSQL can't run there until it's signed.

## 🧹 Uninstalling

Removing BarSQL leaves its `BarSQL-data` folder, with your connections and their passwords, until you delete that too. [Portable by Design](app.md#-portable-by-design) says where it is.

- **Installer** - uninstall BarSQL from Settings → Apps → Installed apps
- **Homebrew** - `brew uninstall bare7a/tap/barsql`, or `brew uninstall --zap bare7a/tap/barsql` to delete `BarSQL-data` as well
- **Scoop** - `scoop uninstall barsql`, or `scoop uninstall --purge barsql` to delete `BarSQL-data` as well
- **Linux packages** - `sudo apt remove barsql`, `sudo dnf remove barsql`, `sudo zypper remove barsql` or `sudo pacman -R barsql`
- **The install script's portable build** keeps `BarSQL-data` inside `~/.local/share/barsql`, so move it out first to keep your connections, then:
  ```bash
  rm -r ~/.local/share/barsql ~/.local/bin/BarSQL ~/.local/share/applications/BarSQL.desktop ~/.local/share/icons/hicolor/*/apps/BarSQL.png
  ```
- **Any other copy** - delete the app, the `.exe`, the AppImage or the binary
