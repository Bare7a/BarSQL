# 🤖 AI Assistants (MCP)

AI apps such as Claude, Cursor or VS Code can look into your databases through the [Model Context Protocol](https://modelcontextprotocol.io). The AI app starts `BarSQL --mcp` in the background - no window - and talks to it over a pipe.

- **Opt-in per connection** - tick **Allow AI assistants (MCP)** in the connection dialog. Other connections stay invisible to the AI
- **Reads stay read-only** - every query passes BarSQL's read-only check, and on PostgreSQL, MySQL, SQLite and ClickHouse the database session refuses writes too, even one inside a function. SQL Server and Turso only get the check, so give those connections a read-only login or token
- **Changes only where you allow them** - also tick **Allow changes**, on a connection that isn't read-only, and the AI gets `run_change` for INSERT, UPDATE, DELETE and schema changes
  - Your AI app asks before each change unless you always allow it. With **Ask before running changes** on, BarSQL asks every time through the AI app, and refuses changes to an AI app that can't ask
  - Each change runs in one transaction where the database has them, and reports the rows it touched. A preview runs it and rolls it back. MySQL commits schema changes at once, so a change with CREATE, ALTER or DROP can't be previewed there
  - An UPDATE or DELETE without a WHERE needs the AI to say it means every row, and SQL that begins, commits or rolls back a transaction itself is refused
- **Tools** - `list_connections`, `list_tables`, `describe_table`, `show_ddl`, `run_query`, `explain_query`, and `run_change`, which connections without **Allow changes** refuse
- **Small answers** - at most 200 rows, long values shortened, and a query stops after 30 seconds
- **No credentials** - the AI sees each connection's name, engine and database, never its password or token
- **A log of every call** in `BarSQL-data/mcp.log`, with what each change did
- What the AI reads is sent to its AI service, so share only what you'd show it

To set it up, tick the box, click **Copy MCP setup** and paste it into your AI app's MCP settings - Claude Desktop's `claude_desktop_config.json`, or a project's `.mcp.json` for Claude Code. Or add it to Claude Code from a terminal:

```bash
claude mcp add BarSQL -- /Applications/BarSQL.app/Contents/MacOS/BarSQL --mcp
```
