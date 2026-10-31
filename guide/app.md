# 🌍 The App

- **Dark & light** themes, or **match the system** and follow it as it changes
- **Welcome screen** while no tab is open - a new connection or a SQLite file to start with, then a new query or the tab you closed last, plus the shortcuts to get around
- **Command palette** (`Ctrl/⌘+Shift+P`) - every menu and editor command, with its shortcut, ready to run where you opened it
- **English**, **Deutsch**, **Български** and **Español**
- **Quick Search palette** (`Ctrl+P`) - jump to connections, tables, saved queries, history, tabs
- Custom shortcuts editor + keyboard tips
- Native title bar and menu bar on macOS; one compact title bar with **File / Edit / View / Help** menus on Windows and Linux
- Tab menu: close others, to the right or all, **pin**, duplicate, rename, copy SQL
- **Status bar** - the tab's connection (click to switch it), the caret's line and column (click to go to a line), cursor count, the open transaction, and a running query with its **Stop** button. Read-only, ask-before-changes and extension chips open the connection's settings, and a guarded connection with a tab color tints the whole bar with it
- Everything opens instantly - no animations
- **Window state persistence** - size, position and maximized state restored between sessions

## ⌨️ Keyboard Shortcuts

The in-app shortcuts editor remaps all of these except the editor's own keys: go to definition, go to line, comments and cursors.

| Action                       | Shortcut                                    |
| ---------------------------- | ------------------------------------------- |
| Quick Search palette         | `Ctrl/⌘ + P`                                |
| Command palette              | `Ctrl/⌘ + Shift + P`                        |
| Go to definition             | `F12`                                       |
| Go to line                   | `Ctrl + G` (Control on macOS too)           |
| Toggle comment               | `Ctrl/⌘ + /`                                |
| Add a cursor at the next occurrence | `Ctrl/⌘ + D`                         |
| Run selection or statement   | `Ctrl/⌘ + Enter`                            |
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

## 🧳 Portable by Design

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

`settings.json` keeps your UI preferences - theme, language, layout and keyboard shortcuts. `mcp.log` appears once an AI assistant has used BarSQL.

When the app sits somewhere writable, that folder is created **right next to the executable** (beside the `.app` bundle on macOS) - move it to a USB stick, network drive, or another PC and it just works.

If the app lives in a read-only location (e.g. `/Applications` or a system path), it falls back to the OS per-user data directory instead:

- **macOS** → `~/Library/Application Support/BarSQL-data`
- **Linux** → `~/.config/BarSQL-data`
- **Windows** → `%AppData%\BarSQL-data`

Override the location with `BARSQL_DATA_DIR`. A debug build (`cargo run`) keeps its data in `./BarSQL-data`, in the folder you run it from.

## 🔄 Auto Updates

- Checks for a new version at launch and offers it with a toast
- The update dialog shows the release notes
- One-click download, checksum-verified install and restart
- Works for every download, and asks for admin rights when the app sits in a protected folder or came as a Linux package
