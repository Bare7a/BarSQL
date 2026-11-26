# ⚡ BarSQL

![Rust](https://img.shields.io/badge/Rust-1.100+-B7410E?style=for-the-badge&logo=rust)
![GPUI](https://img.shields.io/badge/GPUI_Kit-0.10-4B275F?style=for-the-badge)
![License](https://img.shields.io/badge/license-GPL--3.0-blue?style=for-the-badge)
[![Release](https://img.shields.io/github/v/release/Bare7a/BarSQL?style=for-the-badge&color=success)](../../releases/latest)

> **A fast, local-first SQL workbench built with Rust and GPUI.**

No cloud. No accounts. No telemetry.  
Just a focused, native desktop tool for **PostgreSQL**, **MySQL / MariaDB**, **SQL Server**, **ClickHouse**, **Turso / libSQL** and **SQLite**, on Windows, macOS and Linux.

🌐 **[barsql.bare7a.eu](https://barsql.bare7a.eu)** · 🎬 **[Demo video](https://barsql.bare7a.eu/demo.mp4)** · 📖 **[User Guide](guide/README.md)**

https://github.com/user-attachments/assets/e7f8e5f6-a1ed-4855-9994-59a6f382c84f

---

## ⬇️ Download

Get the latest build for Windows, macOS or Linux from **[barsql.bare7a.eu](https://barsql.bare7a.eu/#download)** or the **[Releases](../../releases/latest)** page. No account needed, and every build updates itself.

**macOS**, with [Homebrew](https://brew.sh):

```bash
brew install bare7a/tap/barsql
```

<!-- winget: uncomment once Microsoft approves Bare7a.BarSQL.
**Windows**, with [winget](https://learn.microsoft.com/windows/package-manager/):

```powershell
winget install Bare7a.BarSQL
```

-->
**Windows**, with [Scoop](https://scoop.sh):

```powershell
scoop bucket add bare7a https://github.com/Bare7a/scoop-bucket
scoop install bare7a/barsql
```

**Linux**, with the [install script](guide/install.md#linux-install-script):

```bash
curl -fsSL https://barsql.bare7a.eu/install.sh | sh
```

The macOS app is signed and notarized. The Windows build isn't code-signed yet, so SmartScreen asks once before the first launch: click **More info → Run anyway**.

Which file to pick, the install script's options, uninstalling and verifying a download: **[Installing BarSQL](guide/install.md)**.

---

## ⚡ SQL tools are usually overkill. BarSQL isn’t.

Most are slow to start and heavy on memory, cloud-connected by default, tied to subscriptions or accounts, and overloaded with features you don’t use. BarSQL is built for developers who want speed, clarity and control, and it runs entirely on your machine.

🧳 Portable · ⚡ Fast startup · 🔒 Local-first · 🧠 Developer-focused · 🔄 Auto-updating

Under the hood:

- 🦀 Rust from the database drivers up to the window
- 🖥️ [GPUI](https://www.gpui.rs/), the GPU-accelerated UI framework behind the Zed editor
- 🧩 [GPUI Kit](https://gpui-kit.com/) components, including a code editor with tree-sitter highlighting
- 🔌 Native drivers for each engine: `tokio-postgres`, `mysql_async`, a bundled SQLite, `tiberius` for SQL Server, and BarSQL's own HTTP clients for ClickHouse and Turso

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
    <td align="center"><a href="docs/screenshots/1.webp"><img alt="Editor" src="docs/screenshots/1-800.webp" width="100%"></a><br><sub><b>Editor</b></sub></td>
    <td align="center"><a href="docs/screenshots/2.webp"><img alt="Transactions & multiple results" src="docs/screenshots/2-800.webp" width="100%"></a><br><sub><b>Transactions & multiple results</b></sub></td>
    <td align="center"><a href="docs/screenshots/3.webp"><img alt="Table data" src="docs/screenshots/3-800.webp" width="100%"></a><br><sub><b>Table data</b></sub></td>
  </tr>
  <tr>
    <td align="center"><a href="docs/screenshots/4.webp"><img alt="Cell editor" src="docs/screenshots/4-800.webp" width="100%"></a><br><sub><b>Cell editor</b></sub></td>
    <td align="center"><a href="docs/screenshots/5.webp"><img alt="Grid" src="docs/screenshots/5-800.webp" width="100%"></a><br><sub><b>Grid</b></sub></td>
    <td align="center"><a href="docs/screenshots/6.webp"><img alt="Export" src="docs/screenshots/6-800.webp" width="100%"></a><br><sub><b>Export</b></sub></td>
  </tr>
  <tr>
    <td align="center"><a href="docs/screenshots/7.webp"><img alt="Connections" src="docs/screenshots/7-800.webp" width="100%"></a><br><sub><b>Connections</b></sub></td>
    <td align="center"><a href="docs/screenshots/8.webp"><img alt="Quick Search" src="docs/screenshots/8-800.webp" width="100%"></a><br><sub><b>Quick Search</b></sub></td>
    <td align="center"><a href="docs/screenshots/9.webp"><img alt="DDL viewer" src="docs/screenshots/9-800.webp" width="100%"></a><br><sub><b>DDL viewer</b></sub></td>
  </tr>
  <tr>
    <td align="center"><a href="docs/screenshots/10.webp"><img alt="Plan viewer" src="docs/screenshots/10-800.webp" width="100%"></a><br><sub><b>Plan viewer</b></sub></td>
    <td align="center"><a href="docs/screenshots/11.webp"><img alt="CSV & SQL import" src="docs/screenshots/11-800.webp" width="100%"></a><br><sub><b>CSV & SQL import</b></sub></td>
    <td align="center"><a href="docs/screenshots/12.webp"><img alt="Structure editor" src="docs/screenshots/12-800.webp" width="100%"></a><br><sub><b>Structure editor</b></sub></td>
  </tr>
  <tr>
    <td align="center"><a href="docs/screenshots/13.webp"><img alt="ER diagram" src="docs/screenshots/13-800.webp" width="100%"></a><br><sub><b>ER diagram</b></sub></td>
    <td align="center"><a href="docs/screenshots/14.webp"><img alt="Quick charts" src="docs/screenshots/14-800.webp" width="100%"></a><br><sub><b>Quick charts</b></sub></td>
    <td align="center"><a href="docs/screenshots/15.webp"><img alt="Appearance" src="docs/screenshots/15-800.webp" width="100%"></a><br><sub><b>Appearance</b></sub></td>
  </tr>
</table>

---

## 🗄️ Supported Databases

| Database       | Read & write | Read-only mode | Secure transport                            | SSH tunnel |
| -------------- | :----------: | :------------: | :------------------------------------------ | :--------: |
| **PostgreSQL** |      ✅      |       ✅       | SSL - `disable` / `require` / `verify-full` |     ✅     |
| **MySQL**      |      ✅      |       ✅       | TLS - `disable` / `require` / `verify-full` |     ✅     |
| **MariaDB**    |      ✅      |       ✅       | TLS - `disable` / `require` / `verify-full` |     ✅     |
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
| [⬇️ Installing](guide/install.md) | Which file to pick, Homebrew, Scoop, the Linux install script, the first launch, uninstalling |
| [🔌 Connections](guide/connections.md) | Folders, read-only mode, SSL and TLS, SSH tunnels, SQLite extensions, engine notes |
| [🧠 SQL Editor](guide/editor.md) | Autocomplete, multiple cursors, scripts, transactions, the query plan viewer, saved queries and history |
| [🗃️ Schema Explorer](guide/schema.md) | The schema tree, ER diagrams, schema compare, Copy DDL, schema changes, the structure editor, table backups |
| [📊 Results Grid](guide/results.md) | Sorting, selection stats, quick charts, the cell and JSON viewers, editing data |
| [📥 Import & Export](guide/import-export.md) | CSV and SQL import, export formats |
| [🤖 AI Assistants (MCP)](guide/ai-assistants.md) | Letting AI apps read your databases, and change them only where you allow it |
| [🌍 The App](guide/app.md) | Themes, languages, palettes, the status bar, keyboard shortcuts, the data folder, auto updates |
| [🎨 Themes](guide/themes.md) | The 21 built-in themes, Select theme…, and making your own |

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

## 🤝 Contributing

Issues and pull requests are welcome. Good places to start:

- **[Report a bug or ask for a feature](../../issues/new/choose)** - especially anything that misbehaves on your engine or server version
- **[Add a language](CONTRIBUTING.md#translations)** - mostly JSON, plus a few lines of Rust to register it

If BarSQL is useful to you, a ⭐ helps other people find it.

---

## 📄 License

GPL-3.0-only; see [LICENSE](LICENSE).
