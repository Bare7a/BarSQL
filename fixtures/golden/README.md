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
| `ui/` | Relative history timestamps in en/de/bg across every unit threshold and halfway rounding point |

These files change by hand, for a deliberate behaviour change. The database golden tests and the corpus tests write
`<name>.actual.json` beside a fixture that drifted, so the change can be reviewed and copied over.

Conventions:
- Values are recorded in UTC. `localProbes` repeat the date/time probes in UTC+03:00 to show which values follow the OS zone.
- `display` is the grid's display string; `null` means SQL NULL.
- `emitError` marks a result that cannot be delivered as JSON (NaN/±Inf); the tests count it as a deviation.
- `$userOid` replaces Postgres OIDs of user-defined types, which change on every run; Postgres names types it cannot decode by OID.
- EXPLAIN check mode re-parses the stored raw output instead of re-running it, because costs and timings vary.
- Corpus outputs are keyed by SQL dialect: `postgres`, `mysql`, `sqlite`, `tsql` and `clickhouse`. Every case needs
  all five. Turso speaks SQLite, so it shares `sqlite`. A table filter's `error` holds for every dialect unless
  `byDialect` overrides it.
