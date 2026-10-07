use std::borrow::Cow;
use std::fmt::Write;

use barsql_core::{DriverType, SqlDialect};
use barsql_sql::{quote_ident, quote_ident_list, quote_literal};

// `select` makes the server render every value as an SQL literal, so its rows append straight onto `insert`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DumpQuery {
    // Run on the backup's session before the SELECT.
    pub setup: Vec<String>,
    pub select: String,
    pub insert: String,
    // First and last statements in the file. MySQL and SQLite turn off FK checks in between so rows
    // restore in any order.
    pub prologue: Vec<String>,
    pub epilogue: Vec<String>,
    // Right before the rows, after the table's DDL: SQL Server lets identity values in here.
    pub before_rows: Vec<String>,
    // Right after the rows: Postgres moves its sequences past them, SQL Server stops taking identity values.
    pub sequences: Vec<String>,
    // Each row comes back as one value, already a parenthesized tuple, rather than a value per column.
    pub whole_row: bool,
    // Written after each statement, for SQL Server's tools: GO.
    pub batch_separator: Option<&'static str>,
}

impl DumpQuery {
    // The tuple of a `whole_row` row. A ClickHouse String holds any bytes, and a tuple that isn't UTF-8 arrives as
    // hex (see `push_bytes`). Its stray bytes go back as `\xHH` escapes: in a VALUES tuple they can only sit inside a
    // quoted literal, which reads the escape as the byte.
    pub fn tuple<'a>(&self, cell: &'a str) -> Cow<'a, str> {
        let Some(bytes) = cell.strip_prefix("\\x").and_then(|hex| hex::decode(hex).ok()) else {
            return Cow::Borrowed(cell);
        };
        let mut out = String::with_capacity(bytes.len());
        for chunk in bytes.utf8_chunks() {
            out.push_str(chunk.valid());
            for byte in chunk.invalid() {
                let _ = write!(out, "\\x{byte:02X}");
            }
        }
        Cow::Owned(out)
    }
}

// Generated columns take no value, so backups leave them out.
#[derive(Debug, Clone, Default)]
pub(crate) struct DumpColumn {
    pub name: String,
    pub data_type: String,
    pub generated: bool,
    pub identity_always: bool,
    // Backed by a sequence, either serial or identity.
    pub serial: bool,
}

// Binary comes back as hex so the literal survives any client encoding. FLOAT goes through a double
// because QUOTE on a float keeps only six digits.
fn my_literal(column: &DumpColumn) -> String {
    let ident = quote_ident(&DriverType::MySql, &column.name);
    let data_type = column.data_type.to_lowercase();
    let binary = ["binary", "varbinary", "tinyblob", "blob", "mediumblob", "longblob"].contains(&data_type.as_str())
        || [
            "geometry",
            "point",
            "linestring",
            "polygon",
            "multipoint",
            "multilinestring",
            "multipolygon",
            "geomcollection",
        ]
        .iter()
        .any(|geo| data_type.starts_with(geo));
    match data_type.as_str() {
        _ if binary => format!("IF({ident} IS NULL, 'NULL', CONCAT('X''', HEX({ident}), ''''))"),
        "bit" => format!("IF({ident} IS NULL, 'NULL', CAST({ident} + 0 AS CHAR))"),
        "float" => format!("QUOTE({ident} + 0E0)"),
        _ => format!("QUOTE({ident})"),
    }
}

// Every value as T-SQL source. Text is N-quoted, numbers and binary are written bare, dates are quoted ISO 8601.
// float keeps 17 digits, and money its four decimals.
fn ms_literal(column: &DumpColumn) -> String {
    let ident = quote_ident(&DriverType::SqlServer, &column.name);
    let text = |value: &str| format!("N'N''' + REPLACE({value}, N'''', N'''''') + N''''");
    let quoted = |value: String| format!("N'''' + {value} + N''''");
    let expr = match column.data_type.to_lowercase().as_str() {
        "bit" | "tinyint" | "smallint" | "int" | "bigint" | "decimal" | "numeric" => {
            format!("CAST({ident} AS varchar(100))")
        }
        "money" | "smallmoney" => format!("CONVERT(varchar(100), {ident}, 2)"),
        "float" | "real" => format!("CONVERT(varchar(100), {ident}, 3)"),
        "date" | "time" | "datetime" | "datetime2" | "smalldatetime" | "datetimeoffset" => {
            quoted(format!("CONVERT(nvarchar(40), {ident}, 126)"))
        }
        "uniqueidentifier" => quoted(format!("CAST({ident} AS char(36))")),
        // Binary, and CLR types like hierarchyid and geometry as their binary form. Style 2 leaves out the 0x, which
        // style 1 also leaves out of an empty value.
        "binary" | "varbinary" | "image" | "hierarchyid" | "geometry" | "geography" => {
            format!("'0x' + CONVERT(varchar(max), CAST({ident} AS varbinary(max)), 2)")
        }
        _ => text(&format!("CAST({ident} AS nvarchar(max))")),
    };
    format!("CASE WHEN {ident} IS NULL THEN 'NULL' ELSE {expr} END")
}

// `source` is fully qualified. `target` is qualified only on Postgres and SQL Server, matching the DDL, so MySQL and
// SQLite files restore into whatever database runs them.
pub(crate) fn dump_query(
    driver: &DriverType,
    source: &str,
    target: &str,
    columns: &[DumpColumn],
) -> Result<DumpQuery, String> {
    let dialect = driver.dialect();
    let stored: Vec<&DumpColumn> = columns.iter().filter(|c| !c.generated).collect();
    let literal = |column: &DumpColumn| match dialect {
        Some(SqlDialect::Postgres) => Ok(format!("pg_catalog.quote_nullable({})", quote_ident(driver, &column.name))),
        Some(SqlDialect::MySql) => Ok(my_literal(column)),
        Some(SqlDialect::Sqlite) => Ok(format!("quote({})", quote_ident(driver, &column.name))),
        Some(SqlDialect::ClickHouse) => Ok(quote_ident(driver, &column.name)),
        Some(SqlDialect::TSql) => Ok(ms_literal(column)),
        None => Err(format!("backups aren't supported on {driver}")),
    };
    let literals = stored.iter().map(|column| literal(column)).collect::<Result<Vec<_>, _>>()?;
    let names: Vec<String> = stored.iter().map(|c| c.name.clone()).collect();
    // ClickHouse writes a whole row as a VALUES tuple, with every type in its own literal syntax.
    let (select, whole_row) = match dialect {
        Some(SqlDialect::ClickHouse) => {
            (format!("SELECT formatRowNoNewline('Values', {}) FROM {source}", literals.join(", ")), true)
        }
        Some(SqlDialect::Postgres | SqlDialect::MySql | SqlDialect::Sqlite | SqlDialect::TSql) | None => {
            (format!("SELECT {} FROM {source}", literals.join(", ")), false)
        }
    };
    let postgres = dialect == Some(SqlDialect::Postgres);
    let overriding = postgres && stored.iter().any(|c| c.identity_always);
    // Look the sequence up when the file runs. A renamed table keeps its old sequence name, but the
    // restored table's sequence gets a fresh one.
    let mut sequences: Vec<String> = stored
        .iter()
        .filter(|c| postgres && c.serial)
        .map(|c| {
            let ident = quote_ident(driver, &c.name);
            format!(
                "SELECT pg_catalog.setval(pg_catalog.pg_get_serial_sequence({}, {}), COALESCE(MAX({ident}), 1), \
                 MAX({ident}) IS NOT NULL) FROM {target}",
                quote_literal(target),
                quote_literal(&c.name)
            )
        })
        .collect();
    // MySQL writes TIMESTAMPs in the session's zone, so both ends use UTC.
    let utc = "SET time_zone = '+00:00'".to_string();
    let (setup, prologue, epilogue) = match dialect {
        Some(SqlDialect::MySql) => (
            vec![utc.clone()],
            vec![utc, "SET FOREIGN_KEY_CHECKS = 0".into()],
            vec!["SET FOREIGN_KEY_CHECKS = 1".into()],
        ),
        Some(SqlDialect::Sqlite) => {
            (vec![], vec!["PRAGMA foreign_keys = OFF".into()], vec!["PRAGMA foreign_keys = ON".into()])
        }
        Some(SqlDialect::Postgres | SqlDialect::TSql | SqlDialect::ClickHouse) | None => (vec![], vec![], vec![]),
    };
    // SQL Server takes identity values only between these, one table at a time.
    let mut before_rows = Vec::new();
    if dialect == Some(SqlDialect::TSql) && stored.iter().any(|c| c.serial) {
        before_rows.push(format!("SET IDENTITY_INSERT {target} ON"));
        sequences.push(format!("SET IDENTITY_INSERT {target} OFF"));
    }
    let batch_separator = (dialect == Some(SqlDialect::TSql)).then_some("GO");
    Ok(DumpQuery {
        setup,
        select,
        whole_row,
        before_rows,
        batch_separator,
        insert: format!(
            "INSERT INTO {target} ({}){} VALUES",
            quote_ident_list(driver, &names),
            if overriding { " OVERRIDING SYSTEM VALUE" } else { "" }
        ),
        prologue,
        epilogue,
        sequences,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, data_type: &str) -> DumpColumn {
        DumpColumn { name: name.into(), data_type: data_type.into(), ..Default::default() }
    }

    #[test]
    fn postgres_quotes_on_the_server_and_moves_sequences() {
        let columns = [
            DumpColumn { serial: true, identity_always: true, ..column("id", "integer") },
            column("name", "text"),
            DumpColumn { generated: true, ..column("upper_name", "text") },
        ];
        let table = r#""public"."users""#;
        let query = dump_query(&DriverType::Postgres, table, table, &columns).unwrap();
        assert_eq!(
            query.select,
            r#"SELECT pg_catalog.quote_nullable("id"), pg_catalog.quote_nullable("name") FROM "public"."users""#
        );
        assert_eq!(query.insert, r#"INSERT INTO "public"."users" ("id", "name") OVERRIDING SYSTEM VALUE VALUES"#);
        assert!(query.setup.is_empty() && query.prologue.is_empty() && query.epilogue.is_empty());
        assert_eq!(
            query.sequences,
            [
                r#"SELECT pg_catalog.setval(pg_catalog.pg_get_serial_sequence('"public"."users"', 'id'), COALESCE(MAX("id"), 1), MAX("id") IS NOT NULL) FROM "public"."users""#
            ]
        );
    }

    #[test]
    fn mysql_reads_its_schema_and_writes_binary_as_hex_bits_as_numbers_and_floats_whole() {
        let columns = [
            column("id", "int"),
            column("photo", "LONGBLOB"),
            column("flags", "bit"),
            column("at", "point"),
            column("ratio", "float"),
        ];
        let query = dump_query(&DriverType::MySql, "`shop`.`users`", "`users`", &columns).unwrap();
        assert_eq!(
            query.select,
            "SELECT QUOTE(`id`), IF(`photo` IS NULL, 'NULL', CONCAT('X''', HEX(`photo`), '''')), \
             IF(`flags` IS NULL, 'NULL', CAST(`flags` + 0 AS CHAR)), IF(`at` IS NULL, 'NULL', CONCAT('X''', HEX(`at`), '''')), \
             QUOTE(`ratio` + 0E0) FROM `shop`.`users`"
        );
        assert_eq!(query.insert, "INSERT INTO `users` (`id`, `photo`, `flags`, `at`, `ratio`) VALUES");
        assert_eq!(query.setup, ["SET time_zone = '+00:00'"]);
        assert_eq!(query.prologue, ["SET time_zone = '+00:00'", "SET FOREIGN_KEY_CHECKS = 0"]);
        assert_eq!(query.epilogue, ["SET FOREIGN_KEY_CHECKS = 1"]);
        assert!(query.sequences.is_empty());
    }

    #[test]
    fn clickhouse_rows_come_back_as_values_tuples_with_binary_escaped() {
        let columns =
            [column("id", "UInt64"), column("blob", "String"), DumpColumn { generated: true, ..column("n", "UInt8") }];
        let query = dump_query(&DriverType::ClickHouse, "`db`.`t`", "`db`.`t`", &columns).unwrap();
        assert_eq!(query.select, "SELECT formatRowNoNewline('Values', `id`, `blob`) FROM `db`.`t`");
        assert_eq!(query.insert, "INSERT INTO `db`.`t` (`id`, `blob`) VALUES");
        assert!(query.whole_row);
        assert_eq!(query.tuple("(1,'a')"), "(1,'a')");
        let mut hex = String::new();
        crate::display::push_bytes(b"(1,'\xde\xad\xbe\xef\\\\','\xc3\xa9')", &mut hex);
        assert!(hex.starts_with("\\x"), "{hex}");
        // de ad is a whole UTF-8 sequence, so it stays a character.
        assert_eq!(query.tuple(&hex), "(1,'\u{7ad}\\xBE\\xEF\\\\','é')");
    }

    #[test]
    fn sqlite_uses_its_quote_function_and_pauses_foreign_keys() {
        let columns = [column("id", "INTEGER"), column("body", "")];
        let query = dump_query(&DriverType::Sqlite, r#""notes""#, r#""notes""#, &columns).unwrap();
        assert_eq!(query.select, r#"SELECT quote("id"), quote("body") FROM "notes""#);
        assert_eq!(query.insert, r#"INSERT INTO "notes" ("id", "body") VALUES"#);
        assert_eq!(query.prologue, ["PRAGMA foreign_keys = OFF"]);
        assert_eq!(query.epilogue, ["PRAGMA foreign_keys = ON"]);
    }
}
