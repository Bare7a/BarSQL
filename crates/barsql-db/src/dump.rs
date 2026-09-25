use barsql_core::DriverType;
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
    // Runs after the rows to move Postgres sequences past them.
    pub sequences: Vec<String>,
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

// `source` is fully qualified. `target` is qualified only on Postgres, matching the DDL, so MySQL and SQLite
// files restore into whatever database runs them.
pub(crate) fn dump_query(driver: &DriverType, source: &str, target: &str, columns: &[DumpColumn]) -> DumpQuery {
    let stored: Vec<&DumpColumn> = columns.iter().filter(|c| !c.generated).collect();
    let literals: Vec<String> = stored
        .iter()
        .map(|column| match driver {
            DriverType::Postgres => format!("pg_catalog.quote_nullable({})", quote_ident(driver, &column.name)),
            DriverType::MySql => my_literal(column),
            _ => format!("quote({})", quote_ident(driver, &column.name)),
        })
        .collect();
    let names: Vec<String> = stored.iter().map(|c| c.name.clone()).collect();
    let overriding = stored.iter().any(|c| c.identity_always);
    // Look the sequence up when the file runs. A renamed table keeps its old sequence name, but the
    // restored table's sequence gets a fresh one.
    let sequences = stored
        .iter()
        .filter(|c| c.serial)
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
    let (setup, prologue, epilogue) = match driver {
        DriverType::MySql => (
            vec![utc.clone()],
            vec![utc, "SET FOREIGN_KEY_CHECKS = 0".into()],
            vec!["SET FOREIGN_KEY_CHECKS = 1".into()],
        ),
        DriverType::Sqlite => {
            (vec![], vec!["PRAGMA foreign_keys = OFF".into()], vec!["PRAGMA foreign_keys = ON".into()])
        }
        _ => (vec![], vec![], vec![]),
    };
    DumpQuery {
        setup,
        select: format!("SELECT {} FROM {source}", literals.join(", ")),
        insert: format!(
            "INSERT INTO {target} ({}){} VALUES",
            quote_ident_list(driver, &names),
            if overriding { " OVERRIDING SYSTEM VALUE" } else { "" }
        ),
        prologue,
        epilogue,
        sequences,
    }
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
        let query = dump_query(&DriverType::Postgres, table, table, &columns);
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
        let query = dump_query(&DriverType::MySql, "`shop`.`users`", "`users`", &columns);
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
    fn sqlite_uses_its_quote_function_and_pauses_foreign_keys() {
        let columns = [column("id", "INTEGER"), column("body", "")];
        let query = dump_query(&DriverType::Sqlite, r#""notes""#, r#""notes""#, &columns);
        assert_eq!(query.select, r#"SELECT quote("id"), quote("body") FROM "notes""#);
        assert_eq!(query.insert, r#"INSERT INTO "notes" ("id", "body") VALUES"#);
        assert_eq!(query.prologue, ["PRAGMA foreign_keys = OFF"]);
        assert_eq!(query.epilogue, ["PRAGMA foreign_keys = ON"]);
    }
}
