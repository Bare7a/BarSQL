# Golden fixtures

The expected outputs each suite asserts against.

| Directory | Contents |
|---|---|
| `values/` | Value probes per engine, through the real streaming path: column types, the JSON cell values the UI receives, their display strings, summaries, undeliverable results |
| `errors/` | `QueryError` mapping for failing statements, per engine |
| `schema/` | Schema listings plus the object DDL for every listed object, per engine |
| `explain/` | Raw EXPLAIN output and the normalized plan it parses into, per engine |
| `corpus/` | Statement splits, read-only verdicts, table-filter validation, identifier quoting, plan detection, row-edit SQL, CSV reader records and import previews |
| `export/` | Export and copy formats (text, CSV, JSON, Markdown, SQL INSERT) |
| `ui/` | Relative history timestamps in en/de/bg/es across every unit threshold and halfway rounding point |

These files change for a deliberate behaviour change. The database golden tests and the corpus tests write
`<name>.actual.json` beside a fixture that drifted, so the change can be reviewed and copied over. With the stack up,
`BARSQL_GOLDEN_WRITE=1` makes the database golden tests rewrite the value and error fixtures from what the servers
return, keeping the listed deviations as written, and the schema fixtures of SQL Server, ClickHouse and Turso. Review
the diff before keeping it.

Conventions:
- Values are recorded in UTC. `localProbes` repeat the date/time probes in UTC+03:00 to show which values follow the OS zone.
  MySQL and MariaDB run them with the server's own zone at UTC+03:00 as well. Their values stay in UTC, since BarSQL's
  sessions there work in UTC.
- `display` is the grid's display string; `null` means SQL NULL.
- `emitError` marks a result that cannot be delivered as JSON (NaN/±Inf); the tests count it as a deviation.
- EXPLAIN check mode re-parses the stored raw output instead of re-running it, because costs and timings vary.
- Corpus outputs are keyed by SQL dialect: `postgres`, `mysql`, `sqlite`, `tsql` and `clickhouse`. Every case needs
  all five. Turso speaks SQLite, so it shares `sqlite`. A table filter's `error` holds for every dialect unless
  `byDialect` overrides it.
