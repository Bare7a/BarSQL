# 🗃️ Schema Explorer

- Tree view: schemas → tables / views → columns, then **indexes**, **constraints** and **triggers**
- Views are called out with their own icon and badge; **functions and procedures** close each schema
- Columns show an icon for their type, and gold primary-key and blue foreign-key icons, as the grid and ER diagram do. Hover one for its full type, foreign-key target, default, whether it's generated, and the values an enum allows
- Search tables and columns instantly, with the matches highlighted
- Arrow keys move through the tree, `→` / `←` expand and collapse, and `Enter` expands a row or inserts a column's name
- **Click** a column to insert its name at the cursor
- The 👁 button on a table (or **Browse data**) opens its rows in the grid, editable when it has a primary key
- **Count rows** answers straight away, without opening a tab
- **System views** - see what the server is doing (PostgreSQL activity, blocking and long-running queries; MySQL processes and open transactions; SQL Server requests, sessions and blocking; ClickHouse running queries) from the activity button above the tree or a connection's menu, and how a table is doing (statistics, index usage, ClickHouse parts, mutations and merges) from its menu. Each opens as an ordinary SQL tab and runs, so you can tweak it and run it again
- Refresh the whole schema, or one schema from its right-click menu
- **ER diagram** of a schema from its right-click menu, in a tab that comes back with the session - every table with all its columns, keys first, laid out so referenced tables stand left of the ones referencing them and the keys cross as little as possible. Drag to pan, zoom with the buttons, `⌘/Ctrl`+scroll or a pinch, and Fit to see it all. Hover or click a table to light up its keys and neighbours, find one by name, show only the key columns, and double-click (or use the open button) to open it
- **Compare schema…** with another schema or database of the same engine - missing and extra tables, added, dropped and changed columns, and a sync script you can copy or open in a tab

Columns stay directly under their table. Indexes, constraints and triggers sit below them as collapsed groups and are fetched only when you open one, so expanding a table stays cheap.

Each group row carries what you actually want at a glance: an index's columns and whether it's unique, a foreign key's target (`(org_id) → orgs(id)`), a check's expression, a trigger's timing and events.

## 📋 Copy DDL

Right-click any object - table, view, index, constraint, trigger, function - for **Copy DDL** and **Open DDL in new tab**. The second opens an ordinary SQL tab, so the statement arrives with syntax highlighting, search and editing, ready to run or tweak.

- **SQLite**, **libSQL**, **MySQL / MariaDB** and **ClickHouse** hand back the engine's own text (`sqlite_master`, `SHOW CREATE …`), so what you copy is what the server stored
- **SQL Server** tables are composed from the catalog as well: identity and computed columns, keys with their clustering, foreign keys with their actions, checks, and indexes with `INCLUDE` and filters. Views, procedures, functions and triggers come from `OBJECT_DEFINITION`
- **PostgreSQL** has no `SHOW CREATE TABLE`, so the statement is composed from the catalog: columns with their types, defaults, identity and generated expressions, collations, every table constraint, the indexes no constraint already implies, and `COMMENT ON` for anything documented
- A table's DDL includes its standalone indexes, so pasting it elsewhere rebuilds the table whole

## 🛠️ Changing the Schema

The same menus change things, too:

- **Tables and views** - Rename, Truncate (tables), Drop
- **Columns** - Rename, Drop
- **Indexes, constraints, triggers, functions and procedures** - Drop

Every change opens a dialog that shows the **exact SQL** it will run, written for your engine (`RENAME TABLE` on MySQL, `ALTER VIEW … RENAME` on PostgreSQL, `DELETE FROM` for a SQLite truncate, and so on). PostgreSQL adds **Cascade** and, for a truncate, **Restart identity**. If the server refuses - say, because a view depends on the table - the dialog stays open with its message, so you can tick Cascade and try again.

- A change runs on a connection of its own, lands in the query history, and can be cancelled while it waits on a lock
- The tree, autocomplete and open tabs follow it: a renamed table's tab switches to the new name, a dropped table's tab closes
- Read-only connections show the menu items but can't run them
- What an engine can't do isn't offered - SQLite can't rename a view or drop a constraint, for example

### 🧱 Structure Editor

**Edit structure…** on a table (or the **Structure** button in its data tab, or **Edit table structure** in the command palette) opens it in a **Structure** tab of its own, with its columns, indexes, foreign keys and checks in sections you switch between. **New table…** on a schema opens an empty one, with an `id` key column to start from.

- **Columns** - name, type (typed, or picked from the engine's usual types and the ones your tables already use), nullable, primary key, default, identity / auto-increment and, where the engine keeps them, comments
- **Indexes** - columns in the order you pick them, unique, and the index method on PostgreSQL. A new index is named after its columns until you name it; expression and partial indexes are kept as written and can be dropped
- **Foreign keys** - columns, the referenced table and its columns (its primary key, once you pick the table), and `ON UPDATE` / `ON DELETE` with only the actions the engine supports
- **Checks** - a name and a condition
- Added rows show green, changed fields yellow, dropped rows red and struck through, and the toolbar counts what's pending. **Revert** drops it all
- The **SQL preview** under the rows is the exact script **Apply** (or `⌘/Ctrl`+`S`) runs, and the Apply dialog shows it once more first. If the server refuses, the dialog stays open with its message and your edits stay as they are

Each engine gets a script it can run safely:

- **PostgreSQL** and **SQL Server** run the whole change in one transaction, so it applies completely or not at all. On SQL Server a column's default constraint is dropped and added back around a type change
- **MySQL / MariaDB** can't roll DDL back, so the change goes into **one `ALTER TABLE`** that applies whole or not at all. Only dropping or changing a foreign key takes a statement of its own first, and the dialog says so
- **SQLite / libSQL** use a plain `ALTER TABLE` where it can do the job, and otherwise **rebuild the table**: a new table in the new shape, the rows copied over, the old one swapped out, its indexes and triggers recreated - all in one transaction, checked with `PRAGMA foreign_key_check`. The dialog says when it rebuilds
- **ClickHouse** edits columns, with `Nullable(…)` for the nullable ones, and keeps the sorting key's columns as they are

After Apply the tab shows the table as it now is, and the tree, autocomplete and an open data tab of the table follow. Changes made elsewhere are noticed too: a tab with no edits reloads, one with edits says the table changed and offers **Reload**, and closing it or quitting with unapplied edits asks first. Read-only connections get the same tab as a viewer.

## 💾 Table Backups

**Back up to file…** writes a table to a `.sql` file - its structure, its rows, or both - that recreates it when you run the file or load it with [**Import → SQL script**](import-export.md#sql-scripts).

- **Values come back exactly** - the server writes every value as an SQL literal itself (PostgreSQL `quote_nullable`, MySQL `QUOTE` and hex for binary, SQLite `quote`, ClickHouse `formatRowNoNewline('Values', …)`, and per-type conversions on SQL Server: `N''` strings, exact money and float styles, ISO dates, `0x` binary), so dates, JSON, arrays, binary data and floating-point values survive the round trip
- **Rows restore in any order** - PostgreSQL and SQL Server add foreign keys after the data, MySQL and SQLite pause foreign-key checks, so a table that references itself restores cleanly
- **Sequences follow** - restored PostgreSQL identity and serial columns carry on after the highest id, and SQL Server identities are written with `SET IDENTITY_INSERT`; MySQL `TIMESTAMP`s are written in UTC, so they restore correctly in any time zone
- Generated columns are left for the database to compute
- Rows are grouped into multi-row `INSERT`s, with live progress and a **Stop** button
- The file only replaces an older one once the backup has finished, so a failed or stopped backup leaves the older file untouched
