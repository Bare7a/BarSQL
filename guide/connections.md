# 🔌 Connections

- Create, edit, test and manage database connections
- Organize into **folders** with drag-and-drop reorder
- Per-connection **tab colors**
- **Read-only mode** with defense-in-depth - blocked in the app and again inside each driver session, and PostgreSQL, MySQL, SQLite and ClickHouse sessions refuse writes in the database too, so even a function that writes can't get through
- **Ask before changes** for production connections - the editor confirms any statement that changes data or schema before it runs, and so does saving grid edits
- **Pick the database from a list** - the button beside the database field asks the server for its databases with the credentials you've entered, so you don't have to remember the name
- PostgreSQL SSL (`disable` / `require` / `verify-full`), MySQL TLS, SQL Server TLS including TDS 8.0 `strict`, ClickHouse over HTTPS, and Turso URLs with an auth token
- SQL Server **named instances** - type `host\INSTANCE`, or fill in the Instance field
- **SSH tunnel** to reach databases behind a bastion (see below)
- SQLite file picker, or drop a database file on the window
- **SQLite extensions** - load extensions such as [cr-sqlite](https://github.com/vlcn-io/cr-sqlite) (`.dylib` / `.so` / `.dll`) into a connection each time it opens, in list order. Test names the file that didn't load and why, such as a build for another OS or CPU, or a copy that's incomplete. The status bar shows how many a connection loads, and their functions complete and show on hover as coming from them. SQL can't load extensions itself

## 🔐 SSH Tunnel

Connect to a database that only its bastion can reach - no `ssh -L` in a side terminal.

- Auth by **private key** (with passphrase), **password**, or a running **ssh-agent**
- Host keys verified against `~/.ssh/known_hosts` by default, with an override for a custom file
- Unknown bastion? The error tells you the exact `ssh-keyscan` line to run - or tick **Skip host key check**
- Keepalives hold the tunnel open under a long, idle session
- The database host stays what the bastion resolves (usually `localhost`), and `verify-full` still checks the real certificate

## Engine notes

The [README](../README.md#%EF%B8%8F-supported-databases) lists the engines with their transport and SSH options.

- **SQL Server** - 2017 or newer, with SQL logins. Named instances (`host\INSTANCE`) are found through SQL Browser, though not over SSH. Read-only connections are guarded in the app and also ask the server for read intent. Stopping a running batch closes the tab's connection, so its temp tables, `SET` options and open transaction go with it; the next run opens a new one, and BarSQL says so
- **ClickHouse** - no transactions, and table views are read-only, since ClickHouse has no primary keys to edit rows by. Read-only connections also run with `readonly=2` on the server. Each tab keeps its own session, so `SET`, `USE` and temporary tables last between runs
- **Turso / libSQL** - spoken to over HTTP (Hrana). Streams expire when idle: BarSQL opens a new one by itself, and tells you when a `PRAGMA` or an attached database was lost with it. There's no server-side cancel (Stop drops the request) and no transaction held between runs, though a script's own `BEGIN … COMMIT` works. Give read-only connections a read-only token
