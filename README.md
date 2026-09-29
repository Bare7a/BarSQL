# ⚡ BarSQL

![Rust](https://img.shields.io/badge/Rust-1.98+-B7410E?style=for-the-badge&logo=rust)
![GPUI](https://img.shields.io/badge/GPUI_Kit-0.6-4B275F?style=for-the-badge)
![License](https://img.shields.io/badge/license-GPL--3.0-blue?style=for-the-badge)
![Status](https://img.shields.io/badge/status-active-success?style=for-the-badge)

> **A fast, local-first SQL workbench built with Rust and GPUI.**

No cloud. No accounts. No telemetry.  
Just a focused, native desktop tool for working with databases.

🌐 **[barsql.bare7a.eu](https://barsql.bare7a.eu)**

---

## ⬇️ Download

Grab the latest build for **Windows**, **macOS**, or **Linux** from the [**Releases**](../../releases/latest) page.

### Recommended assets:

- **Windows**: `BarSQL-windows-amd64.zip` (portable) **or** `BarSQL-amd64-installer.exe` (installer)
- **Linux**: `BarSQL-linux-amd64.tar.gz` (portable)
- **macOS**: `BarSQL-darwin-arm64.zip` **or** `BarSQL-darwin-amd64.zip` (the same universal `.app`)

### Additional options:

- **Windows**: `BarSQL.exe` (standalone)
- **Linux**: `BarSQL-x86_64.AppImage`, `BarSQL.deb`, `BarSQL.rpm`, `BarSQL.pkg.tar.zst`, `BarSQL` (standalone)
- **macOS**: `BarSQL-macos-universal.dmg`

No account required - just download and run.

Every build above updates itself. The installer, the Linux packages and copies in protected folders ask for admin rights first.

Prefer to build it yourself? See **Installation & Development** below.

---

## 🚀 Try it in seconds

```bash
cargo run -p barsql --release
```

Or during development:

```bash
cargo run -p barsql
```

---

## ⚡ SQL tools are usually overkill. BarSQL isn’t.

Work with **SQLite**, **PostgreSQL** and **MySQL / MariaDB** in a single fast desktop app that runs entirely on your machine.

🧳 Portable  
⚡ Fast startup  
🔒 Local-first  
🧠 Developer-focused  
🔄 Auto-updating

---

### 👉 Everything you need. Nothing you don’t.

- Query faster with smart, schema-aware autocomplete
- Reach databases behind a bastion over an SSH tunnel
- Stream results - rows arrive as the driver yields them
- Run multi-statement scripts and get a result tab per output
- Explore schemas instantly - down to indexes, constraints and triggers
- Rename, truncate, drop and back up tables straight from the schema tree
- Copy any object's DDL in one click
- Import a CSV or run a `.sql` script
- Save and reuse queries
- Export anything in one click
- Auto-update with one click

---

## 🖥️ Screenshots

<table>
  <tr>
    <td align="center"><img src=".github/screenshots/1.png?raw=true" width="100%"><br><sub><b>Editor</b></sub></td>
    <td align="center"><img src=".github/screenshots/2.png?raw=true" width="100%"><br><sub><b>Transactions & multiple results</b></sub></td>
    <td align="center"><img src=".github/screenshots/3.png?raw=true" width="100%"><br><sub><b>Table data</b></sub></td>
  </tr>
  <tr>
    <td align="center"><img src=".github/screenshots/4.png?raw=true" width="100%"><br><sub><b>Cell editor</b></sub></td>
    <td align="center"><img src=".github/screenshots/5.png?raw=true" width="100%"><br><sub><b>Grid</b></sub></td>
    <td align="center"><img src=".github/screenshots/6.png?raw=true" width="100%"><br><sub><b>Export</b></sub></td>
  </tr>
  <tr>
    <td align="center"><img src=".github/screenshots/7.png?raw=true" width="100%"><br><sub><b>Connections</b></sub></td>
    <td align="center"><img src=".github/screenshots/8.png?raw=true" width="100%"><br><sub><b>Quick Search</b></sub></td>
    <td align="center"><img src=".github/screenshots/9.png?raw=true" width="100%"><br><sub><b>DDL viewer</b></sub></td>
  </tr>
  <tr>
    <td align="center"><img src=".github/screenshots/10.png?raw=true" width="100%"><br><sub><b>Plan viewer</b></sub></td>
    <td align="center"><img src=".github/screenshots/11.png?raw=true" width="100%"><br><sub><b>CSV & SQL import</b></sub></td>
    <td align="center"><img src=".github/screenshots/12.png?raw=true" width="100%"><br><sub><b>Appearance</b></sub></td>
  </tr>
</table>

---

## ⚡ What BarSQL is

BarSQL is a **desktop SQL client built for developers who want speed, clarity and control**.

It combines:

- 🦀 Rust from the database drivers up to the window
- 🖥️ [GPUI](https://www.gpui.rs/), the GPU-accelerated UI framework behind the Zed editor
- 🧩 [GPUI Kit](https://gpui-kit.com/) components, including a code editor with tree-sitter highlighting
- 🔌 Native drivers for each engine: `tokio-postgres`, `mysql_async` and a bundled SQLite

---

## 🚨 Why it exists

Most SQL tools today are:

- slow to start and heavy on memory
- cloud-connected by default
- tied to subscriptions or accounts
- overloaded with features you don’t use

BarSQL focuses on one thing:

> **A fast, local environment for working with databases.**

---

# ⚡ Features

## 🗄️ Supported Databases

| Database       | Read & write | Read-only mode | Secure transport                            | SSH tunnel |
| -------------- | :----------: | :------------: | :------------------------------------------ | :--------: |
| **PostgreSQL** |      ✅      |       ✅       | SSL - `disable` / `require` / `verify-full` |     ✅     |
| **MySQL**      |      ✅      |       ✅       | TLS                                         |     ✅     |
| **MariaDB**    |      ✅      |       ✅       | TLS                                         |     ✅     |
| **SQLite**     |      ✅      |       ✅       | local file                                  |    n/a     |

---

## 🔌 Connections

- Create, edit, test and manage database connections
- Organize into **folders** with drag-and-drop reorder
- Per-connection **tab colors**
- **Read-only mode** with defense-in-depth - blocked in the app and again inside each driver session
- **Pick the database from a list** - the button beside the database field asks the server for its databases with the credentials you've entered, so you don't have to remember the name
- PostgreSQL SSL (`disable` / `require` / `verify-full`) and MySQL TLS
- **SSH tunnel** to reach databases behind a bastion (see below)
- SQLite file picker, or drop a database file on the window

### 🔐 SSH Tunnel

Connect to a database that only its bastion can reach - no `ssh -L` in a side terminal.

- Auth by **private key** (with passphrase), **password**, or a running **ssh-agent**
- Host keys verified against `~/.ssh/known_hosts` by default, with an override for a custom file
- Unknown bastion? The error tells you the exact `ssh-keyscan` line to run - or tick **Skip host key check**
- Keepalives hold the tunnel open under a long, idle session
- The database host stays what the bastion resolves (usually `localhost`), and `verify-full` still checks the real certificate

---

## 🧠 SQL Editor

- **Tabbed** workspace with drag-and-drop tabs and **session restore**
- A native code editor with tree-sitter highlighting, code folding, find & replace and dark/light themes
- **Smart autocomplete** - fuzzy matching, context-aware (`SELECT` / `FROM` / `JOIN` / `WHERE` / `UPDATE` / `DELETE` / `INSERT`), `schema.table.column` dot completion, quoted identifiers, aliases and CTEs
- Built-in **`JOIN` snippets** where a join fits
- Driver-correct identifier quoting (PostgreSQL, MySQL, SQLite)
- **VS Code-style line commands** - copy, cut and paste whole lines, move and duplicate lines, select the next occurrence, toggle comments
- **Run selection** (`Ctrl+Enter`) / **run all** (`Ctrl+Shift+Enter`) / **stop** long-running queries
- **Streaming results** - rows render as the driver yields them
- **Multi-statement scripts** - run several `;`-separated statements at once; they execute in order on one connection, so temp tables, `SET` and scripted `BEGIN` / `COMMIT` hold
- **Multiple result outputs** - a script or stored procedure that returns several result sets shows each in its own switchable result tab, query plans included; a failing statement reports its error and stops the run
- **Pinned transactions** per tab - run `BEGIN` / `COMMIT` / `ROLLBACK` as SQL or from the toolbar; queries run inside the open transaction until you commit or roll back
- **Query plan viewer** (`Ctrl+Shift+E`) - see below
- `UPDATE` / `DELETE` / `INSERT` with **`RETURNING`** flow back to the Results Grid
- Gutter icons to run individual statements
- Right-click menu with **format SQL**
- **Remappable keyboard shortcuts**

### 🧭 Query Plan Viewer

**Explain** (`Ctrl+Shift+E`) plans the statement under the cursor and shows the engine's plan as one tree, whichever database you're on. Nothing runs - the numbers are the planner's estimates. **Explain analyze** (`Ctrl+Shift+A`, or the toolbar button) _executes_ the statement instead and reports what really happened: measured rows, real timings, loop counts.

Typing `EXPLAIN` yourself works the same way - **Run** recognises it and opens the plan viewer instead of dumping the engine's raw rows into the grid. Statements whose output already carries structure (any `FORMAT JSON`, MySQL's `EXPLAIN ANALYZE`, `EXPLAIN QUERY PLAN`) run exactly as typed; the rest are asked again in JSON. Name a format on purpose (`EXPLAIN (FORMAT TEXT)`, `FORMAT=TRADITIONAL`) or use SQLite's bytecode `EXPLAIN` and you get the raw rows, as asked.

Plans are outputs like any other, so a script can mix them freely - `SELECT …; EXPLAIN SELECT …;` gives you `Result 1 · Plan 1` as switchable tabs, each keeping its own state.

- **Heat map** over the tree - each node's bar is its own share of the run's time, cost or rows, so the expensive step is the one you see first; switch the metric to re-rank
- **Own vs. total** for time and cost, side by side - a node isn't flagged just because its children are slow. Loop counts are folded in, so a node inside a nested loop compares fairly to its siblings
- **Bad row estimates flagged** - when the measured count is 10x off the planner's guess, the node is marked with the factor: usually where a missing index or stale statistics hides
- **Relations and indexes** called out per node, plus the filter or join condition behind it
- **Node details** - every field the engine reported (sort method, buffer hits, rows removed by filter, …), and the untouched engine output on a raw tab
- Per engine: PostgreSQL `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`, MySQL `EXPLAIN FORMAT=JSON` and `EXPLAIN ANALYZE`, MariaDB `ANALYZE FORMAT=JSON`, SQLite `EXPLAIN QUERY PLAN` (plan shape only - SQLite reports no cost or timings)
- **Analyzing a write never leaves data behind** - `EXPLAIN ANALYZE` executes the statement, so a write runs inside a transaction that is always rolled back (and says so). On a tab with an open transaction it runs there, exactly where a plain **Run** would have

---

## 🗃️ Schema Explorer

- Tree view: schemas → tables / views → columns, then **indexes**, **constraints** and **triggers**
- Views are called out with their own icon and badge; **functions and procedures** close each schema
- Search tables and columns instantly
- **Click** a column to insert its name at the cursor
- The 👁 button on a table (or **Browse data**) opens its rows in the grid, editable when it has a primary key
- **Count rows** answers straight away, without opening a tab
- Refresh the whole schema, or one schema from its right-click menu

Columns stay directly under their table. Indexes, constraints and triggers sit below them as collapsed groups and are fetched only when you open one, so expanding a table stays cheap.

Each group row carries what you actually want at a glance: an index's columns and whether it's unique, a foreign key's target (`(org_id) → orgs(id)`), a check's expression, a trigger's timing and events.

### 📋 Copy DDL

Right-click any object - table, view, index, constraint, trigger, function - for **Copy DDL** and **Open DDL in new tab**. The second opens an ordinary SQL tab, so the statement arrives with syntax highlighting, search and editing, ready to run or tweak.

- **SQLite** and **MySQL / MariaDB** hand back the engine's own text (`sqlite_master`, `SHOW CREATE …`), so what you copy is what the server stored
- **PostgreSQL** has no `SHOW CREATE TABLE`, so the statement is composed from the catalog: columns with their types, defaults, identity and generated expressions, collations, every table constraint, the indexes no constraint already implies, and `COMMENT ON` for anything documented
- A table's DDL includes its standalone indexes, so pasting it elsewhere rebuilds the table whole

### 🛠️ Changing the Schema

The same menus change things, too:

- **Tables and views** - Rename, Truncate (tables), Drop
- **Columns** - Rename, Drop
- **Indexes, constraints, triggers, functions and procedures** - Drop

Every change opens a dialog that shows the **exact SQL** it will run, written for your engine (`RENAME TABLE` on MySQL, `ALTER VIEW … RENAME` on PostgreSQL, `DELETE FROM` for a SQLite truncate, and so on). PostgreSQL adds **Cascade** and, for a truncate, **Restart identity**. If the server refuses - say, because a view depends on the table - the dialog stays open with its message, so you can tick Cascade and try again.

- A change runs on a connection of its own, lands in the query history, and can be cancelled while it waits on a lock
- The tree, autocomplete and open tabs follow it: a renamed table's tab switches to the new name, a dropped table's tab closes
- Read-only connections show the menu items but can't run them
- What an engine can't do isn't offered - SQLite can't rename a view or drop a constraint, for example

### 💾 Table Backups

**Back up to file…** writes a table to a `.sql` file - its structure, its rows, or both - that recreates it when you run the file or load it with **Import → SQL script**.

- **Values come back exactly** - the server writes every value as an SQL literal itself (PostgreSQL `quote_nullable`, MySQL `QUOTE` and hex for binary, SQLite `quote`), so dates, JSON, arrays, binary data and floating-point values survive the round trip
- **Rows restore in any order** - PostgreSQL adds foreign keys after the data, MySQL and SQLite pause foreign-key checks, so a table that references itself restores cleanly
- **Sequences follow** - restored PostgreSQL identity and serial columns carry on after the highest id; MySQL `TIMESTAMP`s are written in UTC, so they restore correctly in any time zone
- Generated columns are left for the database to compute
- Rows are grouped into multi-row `INSERT`s, with live progress and a **Stop** button
- The file only replaces an older one once the backup has finished, so a failed or stopped backup leaves the older file untouched

---

## 📊 Results Grid

- **Virtualized** grid - smooth scroll over thousands of rows
- **Result-set tabs** - switch between outputs when a run returns multiple result sets
- **Sortable** columns, **fit columns** to their content, and **show / hide** columns
- **Keyboard-first** navigation (arrows, Shift+select, Enter for the cell viewer)
- Column/row selection with Ctrl+click and Shift+click; `Ctrl+C` copies in the format you pick
- **Cell viewer** for large values - JSON, XML, HTML, or plain text; beautify/minify; editable in table view
- **JSON / JSONB** auto-parsed in cell and side viewer
- Side **JSON row viewer** with filter / regex search, synced to focused row

---

## ✏️ Editing Data

View and modify table data directly in the grid - no hand-written `UPDATE` / `DELETE` (writable connections):

- **Browse** any table's rows from the Schema Explorer
- **Inline edit** cells in place - changes are staged, then applied on demand (`Ctrl+S`)
- **Undo / redo** staged changes, **paste** blocks of cells, **set NULL** (`Ctrl+Backspace`)
- **Insert** new rows and **bulk-delete** selected ones
- **Jump through foreign keys** - a referencing cell opens the row it points at
- Safe by design: edits require a primary key and **read-only** connections are blocked in the app and inside the driver
- `INSERT` / `UPDATE` / `DELETE … RETURNING` results flow straight back into the grid

---

## 📚 Query Library & History

- **Saved queries** - name, filter, sort, link tabs with dirty-state tracking
- Save, update, rename and delete from the sidebar or toolbar
- **Per-connection query history** with success/error and duration
- Clear history per connection or delete individual entries

---

## 📥 Import

Load a **CSV** (or any delimited file) or run a **`.sql` script**, from the ⬆ button in the Schema Explorer or a schema's or table's right-click menu.

### CSV

- **Delimiter auto-detected** (`,` `;` tab `|`) - the one that splits every line the same way wins, so commas inside quoted values don't fool it; override it if you'd rather
- **Column mapping** - each source column shows its first value and the target column it loads into; blank the target to skip that column entirely
- **Import into an existing table**, optionally emptying it first, **or create a new one** with types inferred from the data (integer, decimal, boolean, date, timestamp, text) - each one editable before you commit to it, and named in your engine's own dialect
- Headers optional, `NULL` text configurable (`\N`, `NULL`, whatever your exporter writes), leading lines skippable for files with a preamble, whitespace trimmable
- **Empty fields keep their meaning**: a bare one is `NULL`, a quoted `""` is an empty string - the same convention Postgres `COPY … FORMAT csv` uses, and what lets a BarSQL CSV export load straight back in. In a numeric or date column, where `""` is never valid, both mean `NULL`
- UTF-8 BOMs stripped, ragged rows tolerated
- **Bad rows are skipped and reported**, not fatal - or tick **stop at the first error** if you'd rather nothing partial lands
- Rows load in batches with live progress, and **Stop** ends a long run

### SQL scripts

A `.sql` file runs statement by statement on one connection, so `SET`, temp tables and scripted transactions behave. You get a progress bar and a count - not thousands of result tabs. It's also how a table backup comes back.

> Batches are committed as they go, so a stopped or failed import keeps the rows it had already
> loaded and tells you how many that was. Read-only connections refuse imports outright.

---

## 📤 Export

- **Text** / **CSV** / **JSON** / **Markdown** / **SQL INSERT**
- Export all or **selected** rows and columns
- Copy to clipboard or **save to file**
- Remembers your last export format
- **CSV and SQL exports import back cleanly** - `NULL` stays apart from an empty string, and cells that spreadsheets would treat as formulas are guarded on the way out and unguarded on the way in

---

## 🔄 Auto Updates

- Checks for a new version at launch and offers it with a toast
- The update dialog shows the release notes
- One-click download, checksum-verified install and restart
- Works for every download, and asks for admin rights when the app sits in a protected folder or came as a Linux package

---

## 🌍 UX

- **Dark & light** themes
- **English**, **Deutsch** and **Български**
- **Quick Search palette** (`Ctrl+P`) - jump to connections, tables, saved queries, history, tabs
- Custom shortcuts editor + keyboard tips
- Native title bar and menu bar on macOS; one compact title bar with **File / Edit / View / Help** menus on Windows and Linux
- Everything opens instantly - no animations
- **Window state persistence** - size, position and maximized state restored between sessions

---

## ⌨️ Keyboard Shortcuts

Every shortcut is remappable in the in-app shortcuts editor.

| Action                       | Shortcut                                    |
| ---------------------------- | ------------------------------------------- |
| Quick Search palette         | `Ctrl/⌘ + P`                                |
| Run selection                | `Ctrl/⌘ + Enter`                            |
| Run all statements           | `Ctrl/⌘ + Shift + Enter`                    |
| Explain query plan           | `Ctrl/⌘ + Shift + E`                        |
| Explain query plan (analyze) | `Ctrl/⌘ + Shift + A`                        |
| Save query                   | `Ctrl/⌘ + S`                                |
| Rename saved query           | `F2`                                        |
| New / close tab              | `Ctrl/⌘ + T` / `Ctrl/⌘ + W`                 |
| Reopen closed tab            | `Ctrl/⌘ + Shift + T`                        |
| Next / previous tab          | `Ctrl/⌘ + Tab` / `Ctrl/⌘ + Shift + Tab`     |
| Toggle sidebar / JSON panel  | `Ctrl/⌘ + B` / `Ctrl/⌘ + J`                 |
| Zoom in / out / reset        | `Ctrl/⌘ + =` / `Ctrl/⌘ + -` / `Ctrl/⌘ + 0`  |
| Editor font size + / −       | `Ctrl/⌘ + Shift + .` / `Ctrl/⌘ + Shift + ,` |
| Fullscreen                   | `F11`                                       |

---

# 🧳 Portable by Design

Everything lives in a single **`BarSQL-data/`** folder:

```text
BarSQL(.exe)
BarSQL-data/
  connections.json
  editor_session.json
  query_history.json
  saved_queries.json
  settings.json
```

`settings.json` keeps your UI preferences - theme, language, layout and keyboard shortcuts.

When the app sits somewhere writable, that folder is created **right next to the executable** (beside the `.app` bundle on macOS) - move it to a USB stick, network drive, or another PC and it just works.

If the app lives in a read-only location (e.g. `/Applications` or a system path), it falls back to the OS per-user data directory instead:

- **macOS** → `~/Library/Application Support/BarSQL-data`
- **Linux** → `~/.config/BarSQL-data`
- **Windows** → `%AppData%\BarSQL-data`

Override the location with `BARSQL_DATA_DIR`. A debug build (`cargo run`) keeps its data in `./BarSQL-data`, in the folder you run it from.

---

# 🖥️ Built with GPUI

BarSQL's interface is drawn with **[GPUI](https://www.gpui.rs/)**, the UI framework the Zed editor is built on, and the components of **[GPUI Kit](https://gpui-kit.com/)**.

- One Rust codebase from the database drivers to the window
- The whole window is rendered on the GPU
- A background runtime (Tokio) talks to the databases, so the UI thread only draws

---

# ⚙️ Installation & Development

## Requirements

- [Rust](https://rustup.rs/) - rustup installs the toolchain pinned in `rust-toolchain.toml` on first use
- **Linux** also needs GPUI's libraries: `clang libfontconfig-dev libfreetype-dev libwayland-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libvulkan-dev`
- Docker or Podman, only for the PostgreSQL / MySQL / MariaDB test databases

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
cargo xtask e2e up       # PostgreSQL, MySQL and MariaDB from docker-compose.yml
cargo xtask e2e          # the suites that need them
cargo xtask e2e down
cargo xtask screenshots  # the README screenshots, against the PostgreSQL above
cargo xtask docs-images  # the landing page's images, from those screenshots
```

`cargo xtask` is this repository's task runner (the `xtask/` crate); run it alone to list its commands. `COMPOSE="podman compose"` makes it use Podman.

- The UI tests in `crates/barsql-ui` run on GPUI's `TestAppContext` against a SQLite database, with real keystrokes, clicks, file drops and window resizes. `src/scenarios` drives the whole window.
- `--features e2e` adds the per-engine scenarios and the fixtures in `fixtures/golden`, which pin down value display, DDL, EXPLAIN plans and errors for each engine.

CI (`.github/workflows/test.yml`) runs the Linux checks and the E2E suites on every pull request and push to master. macOS and Windows run before a release and when you start the workflow by hand (Actions → Tests → Run workflow).

## Screenshots

`cargo xtask screenshots` takes the pictures above again, at 2048 × 1152 points (4096 × 2304 pixels on a Retina display). It restores `fixtures/screenshots/forum.sql` into the PostgreSQL from `docker-compose.yml`, starts from the connections, tabs and settings in `fixtures/screenshots/data`, and plays the scenes in `crates/barsql-ui/src/screenshots.rs` in a snapshot build. A BarSQL window stays open while they run. It finishes with `cargo xtask docs-images`, which writes each screenshot into `docs/screenshots` as a full-size lossless WebP plus a 640px thumbnail for the landing page.

For a single screenshot, build with `cargo build -p barsql --features snapshot`, then point `BARSQL_SNAPSHOT=out.png` at a data folder in `BARSQL_DATA_DIR`. The app draws its window headless, saves it and quits. `BARSQL_SNAPSHOT_SIZE` sets the window's size in points (`2048x1152`), `BARSQL_SNAPSHOT_RUN=1` runs the active tab first, and `BARSQL_SNAPSHOT_PANEL` chooses what the screenshot shows:

- the sidebar panels: `saved`, `recent`, `connections`
- the grid: `cell`, `export`, `json`
- dialogs: `connection-dialog`, `import`, `about`, `shortcuts`, `tips`
- the update dialog: `update`, `update-current`, `update-downloading`, `update-ready`, `update-failed`
- other screens: `plan`, `quick`, `quick=<query>`, `suggest=<text>`, `toasts`

---

# 🔄 How Updates Work

Release builds check [Bare7a/BarSQL](https://github.com/Bare7a/BarSQL)'s releases at launch and offer a newer one with a toast. About → Check for Updates does the same at any time. The code lives in `crates/barsql-app/src/update*` and `crates/barsql-ui/src/update_dialog.rs`.

1. **Check.** The latest release, if it is newer than this build. The asset depends on how this copy was installed: the `.zip` or `.tar.gz` named for this platform and architecture, or the `.AppImage`, `.deb`, `.rpm` or `.pkg.tar.zst` when the app came as one. Its digest comes from `SHA256SUMS`.
2. **Install Update.** The download goes to a fresh `barsql-update-*` folder in the temp directory, with progress. Its SHA-256 is checked, then the archive's single top-level entry is unpacked, guarded against zip-slip, links and oversized entries. An AppImage or a package stays as it is. An asset that `SHA256SUMS` does not list is refused. Closing the dialog leaves the download running, and Check for Updates reopens it where it got to. A folder left by an app that quit or crashed without applying its download is removed at a later launch, once that app is gone and nothing has touched the folder for an hour.
3. **Restart & Apply.** The app relaunches itself as a helper with the `BARSQL_UPDATER_*` variables set, then quits. The helper first marks the download's folder as in use, so a BarSQL started meanwhile leaves it alone. It waits for the app to exit, swaps `BarSQL.app`, the binary or the AppImage while keeping a backup until the new version has launched, restores the backup on failure, and logs to `update.log` in the data folder, starting it afresh each time.
   - When the folder isn't writable, like Program Files or `/usr/local/bin`, the swap runs as `BarSQL --barsql-update-swap` behind the system's admin prompt (UAC, the macOS password dialog or pkexec). On Windows it also updates the installer's version under Installed apps.
   - Windows can't delete the running `BarSQL.exe`, so the swap moves it aside as `BarSQL.exe.old.<n>`. The new version deletes that once the helper has exited, or, in a folder that needs admin rights, Windows deletes it at the next restart.
   - The Linux packages are installed with `dpkg -i`, `rpm -U` or `pacman -U` through pkexec instead.
   - If the prompt is cancelled or anything fails, the old version starts again.

---

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
- The icon's source is `packaging/icon.svg`.

---

# 🧱 Project Structure

```text
├── .github/               # test and release workflows, PR and issue templates, the README screenshots
├── Cargo.toml             # the workspace; GPUI Kit pinned to an exact version
├── rust-toolchain.toml    # the Rust version rustup installs
├── docker-compose.yml     # PostgreSQL / MySQL / MariaDB for the E2E suites
├── crates/
│   ├── barsql/            # the binary: startup, single instance, file arguments
│   ├── barsql-app/        # orchestration without GPUI: connections, runs, transactions, import, backups, updater
│   ├── barsql-core/       # domain types, settings, the data folder
│   ├── barsql-db/         # the engines: PostgreSQL, MySQL / MariaDB, SQLite, SSH tunnels
│   ├── barsql-io/         # CSV and .sql import, export formats, SQL formatting
│   ├── barsql-sql/        # statement splitting, quoting, DDL, the SQL language service
│   ├── barsql-storage/    # the JSON files in BarSQL-data
│   └── barsql-ui/         # the GPUI views, locales and whole-window scenarios
├── xtask/                 # cargo xtask: lint, e2e, screenshots, docs-images, package, bump-version
├── docs/                  # the landing page at barsql.bare7a.eu (GitHub Pages)
├── fixtures/
│   ├── golden/            # per-engine expectations for the E2E suites
│   └── screenshots/       # the README screenshots' forum database, CSV and data folder
├── packaging/             # the icon and the macOS / Windows / Linux packaging
└── scripts/               # package-macos.sh / package-windows.sh / package-linux.sh
```

---

# 🧰 Tech Stack

| Layer      | Technology                                                        |
| ---------- | ----------------------------------------------------------------- |
| Language   | [Rust](https://www.rust-lang.org/)                                |
| UI         | [GPUI](https://www.gpui.rs/) + [GPUI Kit](https://gpui-kit.com/)  |
| Async      | [Tokio](https://tokio.rs/)                                        |
| PostgreSQL | [tokio-postgres](https://github.com/sfackler/rust-postgres)       |
| MySQL      | [mysql_async](https://github.com/blackbeam/mysql_async)           |
| SQLite     | [rusqlite](https://github.com/rusqlite/rusqlite) (bundled SQLite) |
| SSH        | [russh](https://github.com/Eugeny/russh)                          |
| TLS        | [rustls](https://github.com/rustls/rustls)                        |
| Formatting | [sqlformat](https://github.com/shssoichiro/sqlformat-rs)          |
| Icons      | [Lucide](https://lucide.dev/)                                     |

---

# 📄 License

GPL-3.0-only; see [LICENSE](LICENSE).
