use barsql_app::ImportResult;
use barsql_core::Value;
use barsql_io::{ExportFormat, export_to_string};

use crate::harness::{E2e, Kind, names, run, unique_table};
use crate::support::text;

fn first_error(result: &ImportResult) -> &str {
    result.errors.first().map_or("(no message)", String::as_str)
}

async fn fresh_tables(e: &E2e, prefix: &str, columns: &str) -> (String, String) {
    let (src, dst) = (unique_table(&format!("{prefix}_src")), unique_table(&format!("{prefix}_dst")));
    for table in [&src, &dst] {
        e.create_temp_table(&format!("CREATE TABLE {table} ({columns}){}", e.engine_clause()), table).await;
    }
    (src, dst)
}

// Saved exports end with a newline, so add one like the grid does.
fn write_csv(e: &E2e, csv: &str) -> String {
    e.write(&format!("{}.csv", unique_table("export")), &format!("{csv}\n"))
}

// NULL vs '' quoting, the formula guard and MySQL datetime literals must survive the trip.
each_engine!(async fn round_trip_export_then_import(e) {
    let columns = match e.kind {
        Kind::ClickHouse => "id Int32, nullable_text Nullable(String), empty_text String, payload Nullable(String), \
                             amount Nullable(Decimal(10, 2)), flag Nullable(Bool), note Nullable(String)"
            .to_string(),
        Kind::SqlServer => "id INT, nullable_text NVARCHAR(MAX), empty_text NVARCHAR(MAX) NOT NULL, \
                            payload NVARCHAR(MAX), amount DECIMAL(10,2), flag BIT, note NVARCHAR(MAX)"
            .to_string(),
        Kind::Postgres | Kind::MySql | Kind::MariaDb | Kind::Turso => format!(
            "id INT, nullable_text TEXT, empty_text TEXT NOT NULL, payload {}, amount DECIMAL(10,2), flag {}, note TEXT",
            e.json_type(),
            e.bool_type()
        ),
    };
    let (src, dst) = fresh_tables(&e, "rt", &columns).await;
    // T-SQL has no TRUE and FALSE.
    let (yes, no) = if e.is_sqlserver() { ("1", "0") } else { ("TRUE", "FALSE") };
    e.exec(&format!(
        r#"INSERT INTO {src} (id, nullable_text, empty_text, payload, amount, flag, note) VALUES
            (1, NULL, '', '{{"a": 1, "b": [true, null]}}', 12.34, {yes}, 'plain'),
            (2, 'set', 'x', '{{"nested": {{"k": "v, with comma"}}}}', NULL, {no}, ''),
            (3, NULL, '', 'null', 0.00, NULL, '-not a number')"#
    ))
    .await;
    let cols = names(&["id", "nullable_text", "empty_text", "payload", "amount", "flag", "note"]);
    let select = |table: &str| format!("SELECT {} FROM {table} ORDER BY id", cols.join(", "));
    let exported = e.stream(&select(&src)).await.export(ExportFormat::Csv);
    let result = e.import_csv(&dst, &cols, &write_csv(&e, &exported)).await;
    assert_eq!(result.skipped, 0, "rows were rejected: {}", first_error(&result));
    let (before, after) = (e.query(&select(&src)).await, e.query(&select(&dst)).await);
    assert_eq!(after.rows, before.rows, "the round trip changed the rows\n{exported}");
});

// One column per case, so a failure names the value shape that broke.
const SHAPES: &[(&str, &str, &str, &str)] = &[
    ("c_null_text", "TEXT", "TEXT", "NULL"),
    ("c_empty_text", "TEXT", "TEXT", "''"),
    ("c_json_obj", "JSONB", "JSON", r#"'{"k": "v"}'"#),
    ("c_json_arr", "JSONB", "JSON", r#"'[1, 2, {"a": null}]'"#),
    ("c_json_null", "JSONB", "JSON", "'null'"),
    ("c_json_str", "JSONB", "JSON", r#"'"just a string"'"#),
    ("c_json_empty_str", "JSONB", "JSON", r#"'""'"#),
    ("c_json_sqlnull", "JSONB", "JSON", "NULL"),
    ("c_blob", "BYTEA", "BLOB", "'abc'"),
    ("c_bool_on", "BOOLEAN", "TINYINT(1)", "TRUE"),
    ("c_bool_off", "BOOLEAN", "TINYINT(1)", "FALSE"),
    ("c_bool_null", "BOOLEAN", "TINYINT(1)", "NULL"),
    ("c_num_null", "DECIMAL(10,2)", "DECIMAL(10,2)", "NULL"),
    ("c_ts", "TIMESTAMP", "DATETIME", "'2026-08-09 12:34:56'"),
    ("c_ts_null", "TIMESTAMP", "DATETIME", "NULL"),
    ("c_date", "DATE", "DATE", "'2026-08-09'"),
    ("c_dash_text", "TEXT", "TEXT", "'-not a number'"),
    ("c_at_text", "TEXT", "TEXT", "'@handle'"),
    ("c_plus_text", "TEXT", "TEXT", "'+44 20 7946 0000'"),
    ("c_eq_text", "TEXT", "TEXT", "'=1+2'"),
    ("c_ws_text", "TEXT", "TEXT", "'  leading spaces'"),
    ("c_quote_text", "TEXT", "TEXT", r#"'say "hi", twice'"#),
    ("c_newline_text", "TEXT", "TEXT", "'two\nlines'"),
];

each_engine!(async fn round_trip_value_shapes(e) {
    // rid keeps a NULL-only row from exporting as a blank line, which CSV skips.
    let mut cols = vec!["rid".to_string()];
    let mut defs = vec!["rid INT".to_string()];
    let mut vals = vec!["1".to_string()];
    for (col, pg_type, my_type, value) in SHAPES {
        cols.push(col.to_string());
        // SQLite takes Postgres' names, except for the types it has no affinity for.
        let lite_type = match *pg_type {
            "JSONB" => "TEXT",
            "BYTEA" => "BLOB",
            other => other,
        };
        // ClickHouse columns take NULL only when Nullable.
        let ch_type = match *pg_type {
            "TEXT" | "JSONB" | "BYTEA" => "Nullable(String)".to_string(),
            "BOOLEAN" => "Nullable(Bool)".to_string(),
            "DECIMAL(10,2)" => "Nullable(Decimal(10, 2))".to_string(),
            "TIMESTAMP" => "Nullable(DateTime)".to_string(),
            "DATE" => "Nullable(Date)".to_string(),
            other => format!("Nullable({other})"),
        };
        let ms_type = match *pg_type {
            "TEXT" | "JSONB" => "NVARCHAR(MAX)",
            "BYTEA" => "VARBINARY(MAX)",
            "BOOLEAN" => "BIT",
            "TIMESTAMP" => "DATETIME2",
            other => other,
        };
        let column_type = match e.kind {
            Kind::Postgres => pg_type,
            Kind::MySql | Kind::MariaDb => my_type,
            Kind::Turso => lite_type,
            Kind::ClickHouse => ch_type.as_str(),
            Kind::SqlServer => ms_type,
        };
        defs.push(format!("{col} {column_type}"));
        // T-SQL has no TRUE or FALSE, and text goes into varbinary only by CAST.
        let value = match (e.kind, *value) {
            (Kind::SqlServer, "TRUE") => "1".to_string(),
            (Kind::SqlServer, "FALSE") => "0".to_string(),
            (Kind::SqlServer, text) if *pg_type == "BYTEA" => format!("CAST({text} AS VARBINARY(MAX))"),
            (_, value) => value.to_string(),
        };
        vals.push(value);
    }
    let (src, dst) = fresh_tables(&e, "rt2", &defs.join(", ")).await;
    e.exec(&format!("INSERT INTO {src} ({}) VALUES ({})", cols.join(", "), vals.join(", "))).await;
    let select = format!("SELECT {} FROM {src}", cols.join(", "));
    let before = e.stream(&select).await;
    let (want_row, chunk) = (e.query(&select).await.rows.remove(0), &before.chunks[0]);

    let mut failures = Vec::new();
    for (i, (col, ..)) in SHAPES.iter().enumerate() {
        let pair = names(&["rid", col]);
        let types = [before.types[0].clone(), before.types[i + 1].clone()];
        let csv = export_to_string(ExportFormat::Csv, &pair, &types, None, None, [vec![chunk.cell(0, 0), chunk.cell(0, i + 1)]]);
        // ClickHouse deletes only with a WHERE.
        let empty = if e.is_clickhouse() { format!("TRUNCATE TABLE {dst}") } else { format!("DELETE FROM {dst}") };
        e.exec(&empty).await;
        let result = e.import_csv(&dst, &pair, &write_csv(&e, &csv)).await;
        if result.skipped > 0 {
            failures.push(format!("{col:<18} rejected: {}", first_error(&result)));
            continue;
        }
        let got = e.query(&format!("SELECT {col} FROM {dst}")).await.rows.remove(0).remove(0);
        if got != want_row[i + 1] {
            failures.push(format!("{col:<18} changed: {:?} -> {got:?}\n{csv}", want_row[i + 1]));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
});

// A file that quotes every field still loads, with '' only where the column can hold one.
each_engine!(async fn import_quoted_empty_fields_by_column_type(e) {
    let table = unique_table("quoted_all");
    let columns = match e.kind {
        Kind::ClickHouse => {
            "n Int32, amount Nullable(Decimal(10, 2)), when_ts Nullable(DateTime), note String".to_string()
        }
        Kind::MySql | Kind::MariaDb => "n INT, amount DECIMAL(10,2), when_ts DATETIME, note TEXT".to_string(),
        Kind::Postgres | Kind::Turso => "n INT, amount DECIMAL(10,2), when_ts TIMESTAMP, note TEXT".to_string(),
        Kind::SqlServer => "n INT, amount DECIMAL(10,2), when_ts DATETIME2, note NVARCHAR(MAX)".to_string(),
    };
    let ddl = format!("CREATE TABLE {table} ({columns}){}", e.engine_clause());
    e.create_temp_table(&ddl, &table).await;
    let path = e.write("quoted.csv", "\"n\",\"amount\",\"when_ts\",\"note\"\n\"1\",\"\",\"\",\"\"\n");
    let result = e.import_csv(&table, &names(&["n", "amount", "when_ts", "note"]), &path).await;
    assert_eq!(result.skipped, 0, "rejected: {}", first_error(&result));
    let row = e.query(&format!("SELECT amount, when_ts, note FROM {table}")).await.rows.remove(0);
    assert_eq!(row, [Value::Null, Value::Null, Value::from("")], "numeric and date blanks are NULL, text stays ''");
});

// Bool-like values become engine bools, 1/0 in numeric columns, and stay as written in text.
each_engine!(async fn import_bool_shaped_columns(e) {
    let table = unique_table("bool_targets");
    let note = if e.is_sqlserver() { "NVARCHAR(MAX)" } else { "TEXT" };
    let ddl = format!("CREATE TABLE {table} (rid INT, flag {}, hits INT, note {note}){}", e.bool_type(), e.engine_clause());
    e.create_temp_table(&ddl, &table).await;
    let path = e.write("bools.csv", "rid,flag,hits,note\n1,1,1,true\n2,0,0,false\n3,true,1,t\n4,false,0,f\n");
    let result = e.import_csv(&table, &names(&["rid", "flag", "hits", "note"]), &path).await;
    assert_eq!(result.skipped, 0, "rejected {} rows: {}", result.skipped, first_error(&result));
    let yes = if e.is_sqlserver() { "1" } else { "TRUE" };
    let flagged = e.query(&format!("SELECT COUNT(*) FROM {table} WHERE flag = {yes}")).await;
    assert_eq!(text(&flagged.rows[0][0]), "2");
    let notes = e.query(&format!("SELECT note FROM {table} ORDER BY rid")).await;
    let notes: Vec<String> = notes.rows.iter().map(|r| text(&r[0])).collect();
    assert_eq!(notes, ["true", "false", "t", "f"], "bool-shaped text must stay text");
});

// DDL copied from a user-reported failure, with enum arrays, a self FK, a PK and a unique key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_postgres_real_world_row() {
    run(Kind::Postgres, |e| async move {
        let table = unique_table("users_real");
        let (scopes, permissions) = (unique_table("user_scopes"), unique_table("permissions"));
        e.exec(&format!("CREATE TYPE {scopes} AS ENUM ('compliance', 'audit', 'billing')")).await;
        e.defer(format!("DROP TYPE IF EXISTS {scopes} CASCADE"));
        e.exec(&format!("CREATE TYPE {permissions} AS ENUM ('read', 'write')")).await;
        e.defer(format!("DROP TYPE IF EXISTS {permissions} CASCADE"));
        let ddl = format!(
            "CREATE TABLE {table} (
                id integer NOT NULL,
                email character varying NOT NULL,
                scopes {scopes}[] NOT NULL,
                display_name character varying,
                disabled boolean DEFAULT false NOT NULL,
                created_at timestamp without time zone,
                username text,
                updated_at timestamp without time zone,
                modified_by integer NOT NULL,
                permissions {permissions}[] DEFAULT '{{}}'::{permissions}[] NOT NULL,
                CONSTRAINT {table}_fk FOREIGN KEY (modified_by) REFERENCES {table}(id),
                CONSTRAINT {table}_pk PRIMARY KEY (id),
                CONSTRAINT {table}_uq UNIQUE (username)
            )"
        );
        e.create_temp_table(&ddl, &table).await;
        let header = "id,email,scopes,display_name,disabled,created_at,username,updated_at,modified_by,permissions";
        let csv = format!(
            "{header}\n\
             8,k.e@example.com,{{compliance}},Kyle E,true,,kyle.e,2024-04-08T12:07:05.812Z,235,{{}}\n\
             235,admin@example.com,\"{{audit,billing}}\",Admin,false,2023-01-02T03:04:05Z,admin,2024-01-01T00:00:00Z,235,{{read}}\n"
        );
        let path = e.write("users.csv", &csv);
        let cols: Vec<String> = header.split(',').map(str::to_string).collect();
        let result = e.import_csv(&table, &cols, &path).await;
        assert_eq!(result.skipped, 0, "rejected {} rows: {}", result.skipped, first_error(&result));

        let first = e
            .query(&format!(
                "SELECT disabled, created_at IS NULL, scopes[1], cardinality(permissions), \
                 updated_at = '2024-04-08T12:07:05.812Z'::timestamp FROM {table} WHERE id = 8"
            ))
            .await;
        let first: Vec<String> = first.rows[0].iter().map(text).collect();
        assert_eq!(first, ["true", "true", "compliance", "0", "true"]);
        let second = e.query(&format!("SELECT disabled, scopes[2], permissions[1] FROM {table} WHERE id = 235")).await;
        let second: Vec<String> = second.rows[0].iter().map(text).collect();
        assert_eq!(second, ["false", "billing", "read"]);
    })
    .await
}
