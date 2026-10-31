# 📥 Import & Export

Load a **CSV** (or any delimited file) or run a **`.sql` script**, from the ⬆ button in the Schema Explorer or a schema's or table's right-click menu.

## CSV

- **Delimiter auto-detected** (`,` `;` tab `|`) - the one that splits every line the same way wins, so commas inside quoted values don't fool it; override it if you'd rather
- **Column mapping** - each source column shows its first value and the target column it loads into, with a warning when its values may not fit that column's type; blank the target to skip that column entirely
- **Import into an existing table**, optionally emptying it first, **or create a new one** with types inferred from the data (integer, decimal, boolean, date, timestamp, text) - each one editable before you commit to it, and named in your engine's own dialect
- Headers optional, `NULL` text configurable (`\N`, `NULL`, whatever your exporter writes), leading lines skippable for files with a preamble, whitespace trimmable
- **Empty fields keep their meaning**: a bare one is `NULL`, a quoted `""` is an empty string - the same convention Postgres `COPY … FORMAT csv` uses, and what lets a BarSQL CSV export load straight back in. In a numeric or date column, where `""` is never valid, both mean `NULL`
- UTF-8 BOMs stripped, ragged rows tolerated, and the preview warns when a file isn't UTF-8 (like Excel's Windows-1252 CSVs), whose unreadable characters would load as `�`
- **Bad rows are skipped and reported**, not fatal - or tick **stop at the first error** if you'd rather nothing partial lands
- Rows load in batches with live progress, and **Stop** ends a long run

## SQL scripts

A `.sql` file runs statement by statement on one connection, so `SET`, temp tables and scripted transactions behave. You get a progress bar and a count - not thousands of result tabs. It's also how a [table backup](schema.md#-table-backups) comes back. The file must be UTF-8 (a BOM is fine); any other encoding is refused rather than loaded with `�` in place of what it can't read.

> Batches are committed as they go, so a stopped or failed import keeps the rows it had already
> loaded and tells you how many that was. Read-only connections refuse imports outright.

## 📤 Export

- **Text** / **CSV** / **JSON** / **Markdown** / **SQL INSERT** - SQL is written for the connection's own engine: its identifier quoting, `N''` strings and `1`/`0` booleans on SQL Server, and literals that read the same whatever the server's string settings
- Export all or **selected** rows and columns
- Copy to clipboard or **save to file**
- Remembers your last export format
- **CSV and SQL exports import back cleanly** - `NULL` stays apart from an empty string, and cells that spreadsheets would treat as formulas are guarded on the way out and unguarded on the way in
