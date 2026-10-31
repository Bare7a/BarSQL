# 📊 Results Grid

- **Virtualized** grid - smooth scroll over thousands of rows
- **Result-set tabs** - switch between outputs when a run returns multiple result sets
- **Sortable** columns, **fit columns** to their content, and **show / hide** columns
- Headers show each column's **type** and **key icons**; numbers line up on the decimal point (long fractions are rounded for display), `NULL` is dimmed, and **striped rows** are a View-menu toggle
- **Status bar stats** - cells selected in one column show their count, and the sum, average, min and max of their numbers; click one to copy it. A selection across columns breaks them down per column
- **Quick charts** - the Chart button plots a result as bars or lines. A panel beside it picks the label column and up to five value columns, each in its own colour, and how rows that share a label combine (sum, count or average). It starts from sensible picks, keeps them when you switch back to the grid, and scrolls sideways through many bars
- Right-click menus on cells, row numbers and headers: copy as any export format, filter by a value, sort, hide a column, mark rows for delete
- **Keyboard-first** navigation (arrows, Shift+select, Enter for the cell viewer)
- Column/row selection with Ctrl+click and Shift+click; `Ctrl+C` copies in the format you pick
- **Cell viewer** for large values - JSON, XML, HTML, or plain text; beautify/minify; editable in table view
- **JSON / JSONB** auto-parsed in cell and side viewer
- **Arrays, maps and tuples** - PostgreSQL arrays and ClickHouse `Array`, `Map` and `Tuple` values read as JSON in both viewers; switch the cell viewer to Text for the literal itself
- Side **JSON row viewer**, synced to the focused row and showing its number - filter keys and values (`/…/` for a regex) with the matches highlighted, **Copy JSON**, and **Hide NULL columns**

## ✏️ Editing Data

View and modify table data directly in the grid - no hand-written `UPDATE` / `DELETE` (writable connections):

- **Browse** any table's rows from the Schema Explorer, filter them with a `WHERE` condition that autocompletes, and **Count all** the rows the filter lets through
- **Inline edit** cells in place - changes are staged, then applied on demand (`Ctrl+S`)
- **Booleans and enums** - a boolean is a checkbox that a click or `Space` flips, and an enum, or a column whose `CHECK` lists its values, edits with a picker, in the add-row dialog too
- **Edit query results** too - a `SELECT` from one table that includes its primary key edits like a table view:
  - Aliased columns save to the columns they show, while expressions and generated columns stay as they are
  - The engine says where each column comes from (on Turso, the select list does), so a `search_path` or `USE` can't point an edit at another table
  - Edits run on the tab's own session: inside its transaction when one is open, otherwise all together or not at all
  - Joins, `GROUP BY`, set operations and views stay read-only
  - Running again, closing the tab or quitting asks before dropping changes that weren't applied
- **Undo / redo** staged changes, **paste** blocks of cells, **set NULL** (`Ctrl+Backspace`)
- **Insert** new rows and **bulk-delete** selected ones
- **Jump through foreign keys** - a referencing cell opens the row it points at
- Safe by design: edits require a primary key and **read-only** connections are blocked in the app and inside the driver. ClickHouse tables are browse-only
- `INSERT` / `UPDATE` / `DELETE … RETURNING` results flow straight back into the grid
