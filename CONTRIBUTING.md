# ⚙️ Contributing

How to build, test and find your way around BarSQL. Packaging, releases and the updater are in [RELEASING.md](RELEASING.md).

## Requirements

- [Rust](https://rustup.rs/) - rustup installs the toolchain pinned in `rust-toolchain.toml` on first use
- **Linux** also needs GPUI's libraries: `clang libfontconfig-dev libfreetype-dev libwayland-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libvulkan-dev`
- Docker or Podman, only for the test databases (PostgreSQL, MySQL, MariaDB, ClickHouse, libSQL and, optionally, SQL Server)

## Run

```bash
cargo run -p barsql
```

## Build

```bash
cargo build -p barsql --release   # or: cargo xtask package  (this OS's packages, in target/package/dist)
```

## Tests

```bash
cargo test --workspace   # unit, SQLite and whole-window UI tests
cargo xtask lint         # rustfmt and clippy, as CI runs them
cargo xtask e2e up       # PostgreSQL, MySQL, MariaDB, ClickHouse and libSQL from docker-compose.yml
cargo xtask e2e          # the suites that need them
cargo xtask e2e down
cargo xtask screenshots  # the README screenshots, against the PostgreSQL above
cargo xtask docs-images  # the landing page's images, from those screenshots
```

`cargo xtask` is this repository's task runner (the `xtask/` crate); run it alone to list its commands. `COMPOSE="podman compose"` makes it use Podman.

The databases listen on 55432 (PostgreSQL), 33306 (MySQL), 33307 (MariaDB), 38123 (ClickHouse) and 38080 (libSQL's `sqld`). SQL Server is opt-in: `BARSQL_E2E_MSSQL=1` with `cargo xtask e2e up` adds it on 31433 (Azure SQL Edge on Apple silicon, where SQL Server itself doesn't run), and the same variable turns its tests on.

- The UI tests in `crates/barsql-ui` run on GPUI's `TestAppContext` against a SQLite database, with real keystrokes, clicks, file drops and window resizes. `src/scenarios` drives the whole window.
- `--features e2e` adds the per-engine scenarios and the fixtures in `fixtures/golden`, which pin down value display, DDL, EXPLAIN plans and errors for each engine. `BARSQL_GOLDEN_WRITE=1` rewrites them when an engine's output changes on purpose; review the diff before keeping it.

CI (`.github/workflows/test.yml`) runs the Linux checks and the E2E suites, SQL Server 2022 included, on every pull request and push to master. macOS and Windows run before a release and when you start the workflow by hand (Actions → Tests → Run workflow). Starting it by hand with **regenerate goldens** ticked rewrites the SQL Server fixtures on the real server and uploads them as an artifact to review.

## Screenshots

`cargo xtask screenshots` takes the README's screenshots again, at 2048 × 1152 points (4096 × 2304 pixels on a Retina display). It restores `fixtures/screenshots/forum.sql` into the PostgreSQL from `docker-compose.yml`, starts from the connections, tabs and settings in `fixtures/screenshots/data`, and plays the scenes in `crates/barsql-ui/src/screenshots.rs` in a snapshot build. A BarSQL window stays open while they run. It finishes with `cargo xtask docs-images`, which writes each screenshot into `docs/screenshots` as a full-size lossless WebP plus 640, 800 and 1280px copies, which the landing page picks from to fit the screen.

For a single screenshot, build with `cargo build -p barsql --features snapshot`, then point `BARSQL_SNAPSHOT=out.png` at a data folder in `BARSQL_DATA_DIR`. The app draws its window headless, saves it and quits. `BARSQL_SNAPSHOT_SIZE` sets the window's size in points (`2048x1152`), `BARSQL_SNAPSHOT_RUN=1` runs the active tab first, and `BARSQL_SNAPSHOT_PANEL` chooses what the screenshot shows:

- the sidebar panels: `saved`, `recent`, `connections`
- the grid: `cell`, `export`, `json`
- dialogs: `connection-dialog`, `import`, `about`, `shortcuts`, `tips`
- the update dialog: `update`, `update-current`, `update-downloading`, `update-ready`, `update-failed`
- other screens: `plan`, `quick`, `quick=<query>`, `suggest=<text>`, `toasts`, `palette` (the command palette), `chart` (the first result as a chart, with `BARSQL_SNAPSHOT_RUN=1`), `er` (the `main` schema's ER diagram), `structure` (a table's Structure tab: the first in `main`, or `structure=<table>`)

## Docs

- `README.md` - the front page on GitHub: the pitch, downloads, screenshots and the engines table
- `guide/` - the user guide, one page per area
- `docs/` - the landing page at [barsql.bare7a.eu](https://barsql.bare7a.eu), served by GitHub Pages

A change users can see updates its guide page; the README and the landing page follow headline features only. Preview the landing page over HTTP, since Chrome doesn't load its icons and logos on a `file://` page:

```bash
python3 -m http.server 8765 --directory docs   # then open http://localhost:8765
```

## Project Structure

```text
├── .github/               # test and release workflows, PR and issue templates, the README screenshots
├── Cargo.toml             # the workspace; GPUI Kit pinned to an exact version
├── rust-toolchain.toml    # the Rust version rustup installs
├── docker-compose.yml     # PostgreSQL / MySQL / MariaDB / ClickHouse / libSQL / SQL Server for the E2E suites
├── crates/
│   ├── barsql/            # the binary: startup, single instance, file arguments, --mcp
│   ├── barsql-app/        # orchestration without GPUI: connections, runs, transactions, edits, structure changes, import, backups, updater, MCP server
│   ├── barsql-core/       # domain types, settings, the data folder
│   ├── barsql-db/         # the engines: PostgreSQL, MySQL / MariaDB, SQLite, SQL Server, ClickHouse, Turso / libSQL, SSH tunnels
│   ├── barsql-io/         # CSV and .sql import, export formats, SQL formatting
│   ├── barsql-sql/        # statement splitting, quoting, the read-only check, DDL and ALTER scripts, schema diffs, the SQL language service
│   ├── barsql-storage/    # the JSON files in BarSQL-data
│   └── barsql-ui/         # the GPUI views, locales and whole-window scenarios
├── xtask/                 # cargo xtask: lint, e2e, screenshots, docs-images, icons, package, bump-version
├── docs/                  # the landing page at barsql.bare7a.eu (GitHub Pages)
├── guide/                 # the user guide, one page per area
├── fixtures/
│   ├── golden/            # per-engine expectations for the E2E suites
│   └── screenshots/       # the README screenshots' forum database, CSV and data folder
├── packaging/             # the icon and the macOS / Windows / Linux packaging
└── scripts/               # package-macos.sh / package-windows.sh / package-linux.sh
```

## Tech Stack

BarSQL's interface is drawn with **[GPUI](https://www.gpui.rs/)**, the UI framework the Zed editor is built on, and the components of **[GPUI Kit](https://gpui-kit.com/)**.

- One Rust codebase from the database drivers to the window
- The whole window is rendered on the GPU
- A background runtime (Tokio) talks to the databases, so the UI thread only draws

| Layer      | Technology                                                        |
| ---------- | ----------------------------------------------------------------- |
| Language   | [Rust](https://www.rust-lang.org/)                                |
| UI         | [GPUI](https://www.gpui.rs/) + [GPUI Kit](https://gpui-kit.com/)  |
| Async      | [Tokio](https://tokio.rs/)                                        |
| PostgreSQL | [tokio-postgres](https://github.com/sfackler/rust-postgres)       |
| MySQL      | [mysql_async](https://github.com/blackbeam/mysql_async)           |
| SQLite     | [rusqlite](https://github.com/rusqlite/rusqlite) (bundled SQLite) |
| SQL Server | [tiberius](https://github.com/prisma/tiberius)                    |
| ClickHouse & Turso | BarSQL's own HTTP client, on [httparse](https://github.com/seanmonstar/httparse) |
| SSH        | [russh](https://github.com/Eugeny/russh)                          |
| TLS        | [rustls](https://github.com/rustls/rustls)                        |
| Formatting | [sqlformat](https://github.com/shssoichiro/sqlformat-rs)          |
| Icons      | [Lucide](https://lucide.dev/)                                     |
