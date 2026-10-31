# 🚢 Releasing

1. The workspace starts at 1.0.0. For later releases, run `cargo xtask bump-version --patch` (or `--minor`, `--major`, or an explicit version such as `1.1.0`), then commit and push. `cargo xtask version` prints the current version.
2. Tag and push (`git tag v1.0.0 && git push origin v1.0.0`), or start the workflow by hand on any branch (Actions → Release BarSQL → Run workflow) with the version, and it tags the commit it runs on.
3. `.github/workflows/release.yml` checks that the tag matches `Cargo.toml` and isn't already on another commit, then builds on Ubuntu 22.04, Windows and macOS while the full test suite runs on all three. Once both pass, it publishes every asset plus `SHA256SUMS`.

Run the workflow by hand with the version left empty to build the packages without publishing them; they stay on the run's page for 7 days.

`cargo xtask package` builds this OS's packages locally, into `target/package/dist` (on Linux, after `cargo install cargo-deb cargo-generate-rpm`). `BARSQL_RELEASE_DIR=<that folder> cargo test -p barsql-app a_packaged_release_stages_its_app` runs the updater over them.

## Packages

| Script                                                                    | Portable updater asset (one top-level entry)                                           | Other downloads                                                |
| ------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| `scripts/package-macos.sh`                                                | `BarSQL-darwin-arm64.zip`, `BarSQL-darwin-amd64.zip` (the same universal `BarSQL.app`) | `BarSQL-macos-universal.dmg`                                   |
| `scripts/package-windows.sh`                                              | `BarSQL-windows-amd64.zip` (`BarSQL.exe`)                                              | `BarSQL.exe`, `BarSQL-amd64-installer.exe`                     |
| `scripts/package-linux.sh`                                                | `BarSQL-linux-amd64.tar.gz` (`BarSQL`)                                                 | `BarSQL`, `BarSQL.deb`, `BarSQL.rpm`, `BarSQL-x86_64.AppImage` |
| `packaging/linux/PKGBUILD` (release workflow, in an Arch Linux container) |                                                                                        | `BarSQL.pkg.tar.zst`                                           |

- Only the portable updater assets name both a platform and an architecture, so portable copies never pick another download. The AppImage and the packages update from their own asset, and the installer and the dmg from the `.zip`.
- Release builds turn on the `production` feature, which enables the update check at launch. The release profile uses thin LTO and strips symbols.
- **macOS:** a `lipo` universal `.app` for macOS 12 and later with an ad-hoc signature; the dmg adds an `Applications` link. `BARSQL_NATIVE_ONLY=1` builds this Mac's architecture alone.
- **Windows:** `crates/barsql/build.rs` embeds the icon and the version block. `packaging/windows/installer.nsi` installs the app with the `SQLite Database` file class for `.db`, `.sqlite`, `.sqlite3`, `.s3db` and `.sl3`.
- **Linux:** [`cargo-deb`](https://crates.io/crates/cargo-deb) and [`cargo-generate-rpm`](https://crates.io/crates/cargo-generate-rpm) build the deb and rpm from `crates/barsql/Cargo.toml`, and `makepkg` builds the Arch package from `packaging/linux/PKGBUILD`. Each installs `/usr/bin/BarSQL` and a desktop entry that opens SQLite files; the deb and rpm work out their library dependencies from the binary. The window's app id is `BarSQL`, so Wayland docks match it to `BarSQL.desktop`.
- The icon's source is `packaging/icon.svg`. The macOS `.icns` sets it on Apple's icon grid (an 824px body on a 1024px canvas, with a shadow). Windows and Linux show the stack without its tile, since their taskbars and docks are mostly dark and the dark tile would disappear there, leaving the stack looking small. The title bar shows the stack too, in a dark and a light version (`crates/barsql-ui/images/barsql-mark-*.svg`). `cargo xtask icons` regenerates the set from it: the Linux PNGs, `icon.ico`, `icons.icns` and `docs/icon.png`, each size rendered from the vector, and the SVG copies for the app (the marks and its icon) and `docs/`.

## How Updates Work

Release builds check [Bare7a/BarSQL](https://github.com/Bare7a/BarSQL)'s releases at launch and offer a newer one with a toast. About → Check for Updates does the same at any time. The code lives in `crates/barsql-app/src/update*` and `crates/barsql-ui/src/update_dialog.rs`.

1. **Check.** The latest release, if it is newer than this build. The asset depends on how this copy was installed: the `.zip` or `.tar.gz` named for this platform and architecture, or the `.AppImage`, `.deb`, `.rpm` or `.pkg.tar.zst` when the app came as one. Its digest comes from `SHA256SUMS`.
2. **Install Update.** The download goes to a fresh `barsql-update-*` folder in the temp directory, with progress. Its SHA-256 is checked, then the archive's single top-level entry is unpacked, guarded against zip-slip, links and oversized entries. An AppImage or a package stays as it is. An asset that `SHA256SUMS` does not list is refused. Closing the dialog leaves the download running, and Check for Updates reopens it where it got to. A folder left by an app that quit or crashed without applying its download is removed at a later launch, once that app is gone and nothing has touched the folder for an hour.
3. **Restart & Apply.** The app relaunches itself as a helper with the `BARSQL_UPDATER_*` variables set, then quits. The helper first marks the download's folder as in use, so a BarSQL started meanwhile leaves it alone. It waits for the app to exit, swaps `BarSQL.app`, the binary or the AppImage while keeping a backup until the new version has launched, restores the backup on failure, and logs to `update.log` in the data folder, starting it afresh each time.
   - When the folder isn't writable, like Program Files or `/usr/local/bin`, the swap runs as `BarSQL --barsql-update-swap` behind the system's admin prompt (UAC, the macOS password dialog or pkexec). On Windows it also updates the installer's version under Installed apps.
   - Windows can't delete the running `BarSQL.exe`, so the swap moves it aside as `BarSQL.exe.old.<n>`. The new version deletes that once the helper has exited, or, in a folder that needs admin rights, Windows deletes it at the next restart.
   - The Linux packages are installed with `dpkg -i`, `rpm -U` or `pacman -U` through pkexec instead.
   - If the prompt is cancelled or anything fails, the old version starts again.
