# 🧠 SQL Editor

- **Tabbed** workspace with drag-and-drop tabs and **session restore**
- A native code editor with tree-sitter highlighting, code folding, find & replace and dark/light themes
- **Smart autocomplete** - fuzzy matching, context-aware (`SELECT` / `FROM` / `JOIN` / `WHERE` / `UPDATE` / `DELETE` / `INSERT`), `schema.table.column` dot completion, quoted identifiers, aliases and CTEs
- Built-in **`JOIN` snippets** where a join fits
- **Function completion and hover** for every engine - each dialect's built-in functions (from about 150 on SQLite to over 400 on ClickHouse) plus the server's own (user functions, extensions, ClickHouse combinators like `sumIf`), each with its signatures, return type and a short description
- Driver-correct identifier quoting (PostgreSQL, MySQL, SQLite, SQL Server `[brackets]`, ClickHouse)
- **VS Code-style multiple cursors** - `Ctrl/⌘+D` adds a cursor at the next occurrence of the word or selection, `Ctrl/⌘+Shift+L` at every one, `Ctrl/⌘+K Ctrl/⌘+D` skips one and `Ctrl/⌘+U` takes the last back
- **Occurrence highlighting** - the word at the caret, or the selected text, is marked wherever else it appears
- **VS Code-style line commands** - copy, cut and paste whole lines, move and duplicate lines, toggle comments
- **Run selection** (`Ctrl+Enter`) - with nothing selected it runs the statement under the caret, briefly highlighting it / **run all** (`Ctrl+Shift+Enter`) / **stop** long-running queries
- **Query parameters** - `:name` placeholders ask for their values before a run (values are SQL, so quote text; empty means `NULL`) and the last values come back next time. Casts like `::int`, strings and comments are left alone
- **Every-row guard** - an `UPDATE` or `DELETE` without a `WHERE` asks before it runs
- **Go to definition** (`F12` or `⌘/Ctrl`-click) - an alias or CTE jumps to where the query declares it, and a table (or a column's table) opens in the grid
- **Go to line** (`Ctrl+G`, on macOS too) - a line, or `line:column`
- **Errors point at the problem** - the word where a run failed is underlined in the editor, even when the engine names an unknown column without saying where. When a table or column the schema has is close to that name, **Did you mean …?** comes with a button that swaps it in
- **Streaming results** - rows render as the driver yields them
- **Multi-statement scripts** - run several `;`-separated statements at once; they execute in order on one connection, so temp tables, `SET` and scripted `BEGIN` / `COMMIT` hold. SQL Server scripts run batch by batch, split on `GO` (`GO 3` runs a batch three times)
- **Messages tab** - PostgreSQL notices, MySQL warnings, SQL Server `PRINT` and `RAISERROR` output, grouped under the statement that raised them, with levels and codes; it opens by itself when a run returns nothing else
- **Multiple result outputs** - a script or stored procedure that returns several result sets shows each in its own switchable result tab, query plans included; a failing statement reports its error and stops the run
- **Pinned transactions** per tab - run `BEGIN` / `COMMIT` / `ROLLBACK` as SQL or from the toolbar; queries run inside the open transaction until you commit or roll back (on every engine that holds one: not ClickHouse or Turso). Closing the tab, quitting or restarting for an update asks before rolling it back. On macOS so do the Dock's Quit, logging out and shutting down, which BarSQL cancels to ask. Renaming a connection or moving it to a folder keeps it open; disconnecting, or changing how it connects, rolls its tabs' transactions back and says so. Savepoints you make last until the transaction ends, across runs
- **Query plan viewer** (`Ctrl+Shift+E`) - see below
- `UPDATE` / `DELETE` / `INSERT` with **`RETURNING`** flow back to the Results Grid
- Gutter icons to run individual statements
- Right-click menu with run, explain, go to definition, **change all occurrences**, toggle comment and **format SQL**
- **Remappable keyboard shortcuts**

## 🧭 Query Plan Viewer

**Explain** (`Ctrl+Shift+E`) plans the statement under the cursor and shows the engine's plan as one tree, whichever database you're on. Nothing runs - the numbers are the planner's estimates. **Explain analyze** (`Ctrl+Shift+A`, or the menu on the toolbar's Explain button) _executes_ the statement instead and reports what really happened: measured rows, real timings, loop counts.

Typing `EXPLAIN` yourself works the same way - **Run** recognises it and opens the plan viewer instead of dumping the engine's raw rows into the grid. Statements whose output already carries structure (any `FORMAT JSON`, MySQL's `EXPLAIN ANALYZE`, `EXPLAIN QUERY PLAN`) run exactly as typed; the rest are asked again in JSON. Name a format on purpose (`EXPLAIN (FORMAT TEXT)`, `FORMAT=TRADITIONAL`) or use SQLite's bytecode `EXPLAIN` and you get the raw rows, as asked.

Plans are outputs like any other, so a script can mix them freely - `SELECT …; EXPLAIN SELECT …;` gives you `Result 1 · Plan 1` as switchable tabs, each keeping its own state.

- **Heat map** over the tree - each node's bar is its own share of the run's time, cost or rows, so the expensive step is the one you see first; switch the metric to re-rank
- **Own vs. total** for time and cost, side by side - a node isn't flagged just because its children are slow. Loop counts are folded in, so a node inside a nested loop compares fairly to its siblings
- **Bad row estimates flagged** - when the measured count is 10x off the planner's guess, the node is marked with the factor: usually where a missing index or stale statistics hides
- **Relations and indexes** called out per node, plus the filter or join condition behind it
- **Node details** - every field the engine reported (sort method, buffer hits, rows removed by filter, …), and the untouched engine output on a raw tab
- Per engine: PostgreSQL `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`, MySQL `EXPLAIN FORMAT=JSON` and `EXPLAIN ANALYZE`, MariaDB `ANALYZE FORMAT=JSON`, SQL Server's Showplan XML (`SET SHOWPLAN_XML` for an estimate, `SET STATISTICS XML` for actuals), SQLite and libSQL `EXPLAIN QUERY PLAN`, ClickHouse `EXPLAIN PLAN json = 1, indexes = 1` (plan shape and index use only - SQLite and ClickHouse report no cost or timings)
- **Analyzing a write never leaves data behind** - `EXPLAIN ANALYZE` executes the statement, so a write runs inside a transaction that is always rolled back (and says so). On a tab with an open transaction it runs there, exactly where a plain **Run** would have

## 📚 Query Library & History

- **Saved queries** - name, filter, sort, link tabs with dirty-state tracking
- Save, update, rename (`F2`) and delete from the sidebar or toolbar
- **Per-connection query history** with success/error and duration, filterable to the failed runs
- Clear history per connection or delete individual entries
