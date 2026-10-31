# ⚡ BarSQL

![Rust](https://img.shields.io/badge/Rust-1.99+-B7410E?style=for-the-badge&logo=rust)
![GPUI](https://img.shields.io/badge/GPUI_Kit-0.8-4B275F?style=for-the-badge)
![License](https://img.shields.io/badge/license-GPL--3.0-blue?style=for-the-badge)
![Status](https://img.shields.io/badge/status-active-success?style=for-the-badge)

> **A fast, local-first SQL workbench built with Rust and GPUI.**

No cloud. No accounts. No telemetry.  
Just a focused, native desktop tool for working with databases.

🌐 **[barsql.bare7a.eu](https://barsql.bare7a.eu)** · 📖 **[User Guide](guide/README.md)**

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

Prefer to build it yourself? See **Build from source** below.

---

## ⚡ SQL tools are usually overkill. BarSQL isn’t.

Work with **SQLite**, **PostgreSQL**, **MySQL / MariaDB**, **SQL Server**, **ClickHouse** and **Turso / libSQL** in a single fast desktop app that runs entirely on your machine.

🧳 Portable  
⚡ Fast startup  
🔒 Local-first  
🧠 Developer-focused  
🔄 Auto-updating

---

### 👉 Everything you need. Nothing you don’t.

- Query faster with smart, schema-aware autocomplete - functions included, with their signatures on hover
- Edit SQL the VS Code way - multiple cursors, line commands, go to definition and a command palette
- See what the server said - notices, warnings and `PRINT` output in a Messages tab
- Check what a server is doing in one click - activity, blocking, sessions, table statistics
- Reach databases behind a bastion over an SSH tunnel
- Stream results - rows arrive as the driver yields them
- Run multi-statement scripts and get a result tab per output
- Edit rows in the grid - in a table, or in the results of a `SELECT` from one
- Plot any result as a bar or line chart
- Explore schemas instantly - down to indexes, constraints and triggers, or as an ER diagram
- Design tables in a Structure tab and see the exact SQL before it runs
- Rename, truncate, drop and back up tables straight from the schema tree
- Compare two schemas and get the script that syncs them
- Copy any object's DDL in one click
- Import a CSV or run a `.sql` script
- Save and reuse queries
- Export anything in one click
- Let AI assistants such as Claude read your databases over MCP - and change them only where you allow it
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
    <td align="center"><img src=".github/screenshots/12.png?raw=true" width="100%"><br><sub><b>Structure editor</b></sub></td>
  </tr>
  <tr>
    <td align="center"><img src=".github/screenshots/13.png?raw=true" width="100%"><br><sub><b>ER diagram</b></sub></td>
    <td align="center"><img src=".github/screenshots/14.png?raw=true" width="100%"><br><sub><b>Quick charts</b></sub></td>
    <td align="center"><img src=".github/screenshots/15.png?raw=true" width="100%"><br><sub><b>Appearance</b></sub></td>
  </tr>
</table>

---

## ⚡ What BarSQL is

BarSQL is a **desktop SQL client built for developers who want speed, clarity and control**.

It combines:

- 🦀 Rust from the database drivers up to the window
- 🖥️ [GPUI](https://www.gpui.rs/), the GPU-accelerated UI framework behind the Zed editor
- 🧩 [GPUI Kit](https://gpui-kit.com/) components, including a code editor with tree-sitter highlighting
- 🔌 Native drivers for each engine: `tokio-postgres`, `mysql_async`, a bundled SQLite, `tiberius` for SQL Server, and BarSQL's own HTTP clients for ClickHouse and Turso

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

## 🗄️ Supported Databases

| Database       | Read & write | Read-only mode | Secure transport                            | SSH tunnel |
| -------------- | :----------: | :------------: | :------------------------------------------ | :--------: |
| **PostgreSQL** |      ✅      |       ✅       | SSL - `disable` / `require` / `verify-full` |     ✅     |
| **MySQL**      |      ✅      |       ✅       | TLS                                         |     ✅     |
| **MariaDB**    |      ✅      |       ✅       | TLS                                         |     ✅     |
| **SQLite**     |      ✅      |       ✅       | local file                                  |    n/a     |
| **SQL Server** |      ✅      |       ✅       | TLS - `disable` / `require` / `verify-full` / `strict` (TDS 8.0) | ✅ |
| **ClickHouse** |      ✅      |       ✅       | HTTP or HTTPS - `disable` / `require` / `verify-full` | ✅ |
| **Turso / libSQL** |  ✅      |       ✅       | HTTPS, or HTTP for a local `sqld`           | self-hosted `sqld` |

Per-engine details are in the **[engine notes](guide/connections.md#engine-notes)**.

---

## 📖 User Guide

Every feature in detail, one area per page:

| Page | What it covers |
| ---- | -------------- |
| [🔌 Connections](guide/connections.md) | Folders, read-only mode, SSL and TLS, SSH tunnels, SQLite extensions, engine notes |
| [🧠 SQL Editor](guide/editor.md) | Autocomplete, multiple cursors, scripts, transactions, the query plan viewer, saved queries and history |
| [🗃️ Schema Explorer](guide/schema.md) | The schema tree, ER diagrams, schema compare, Copy DDL, schema changes, the structure editor, table backups |
| [📊 Results Grid](guide/results.md) | Sorting, selection stats, quick charts, the cell and JSON viewers, editing data |
| [📥 Import & Export](guide/import-export.md) | CSV and SQL import, export formats |
| [🤖 AI Assistants (MCP)](guide/ai-assistants.md) | Letting AI apps read your databases, and change them only where you allow it |
| [🌍 The App](guide/app.md) | Themes, languages, palettes, the status bar, keyboard shortcuts, the data folder, auto updates |

---

## 🧳 Portable by Design

Everything lives in a single **`BarSQL-data/`** folder. When the app sits somewhere writable, that folder is created **right next to the executable** (beside the `.app` bundle on macOS) - move it to a USB stick, network drive, or another PC and it just works.

What's in it, and where it goes when the app can't write beside itself: **[The App](guide/app.md#-portable-by-design)**.

---

## ⚙️ Build from source

```bash
cargo run -p barsql --release
```

Requirements, tests and the project layout are in **[CONTRIBUTING.md](CONTRIBUTING.md)**, packaging and releases in **[RELEASING.md](RELEASING.md)**.

---

## 📄 License

GPL-3.0-only; see [LICENSE](LICENSE).
