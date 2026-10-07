use barsql_core::{ColumnInfo, DriverType, Row, SqlDialect, TableDataRequest, Value, schema::column_exists};

use crate::ddl::{DdlColumn, compose_create_table};
use crate::dialect::{Dialect, Paging, Preview};
use crate::quote::{placeholder, qualified_table, quote_ident, quote_ident_list, table_ref};
use crate::sql_text::to_upper;

pub type Statement = (String, Vec<Value>);

pub fn build_update(
    driver: &DriverType,
    schema: &str,
    table: &str,
    changes: &Row,
    pk_values: &Row,
    pk_cols: &[String],
) -> Result<Statement, String> {
    if changes.is_empty() {
        return Err("no changes to apply".into());
    }
    require_row_edits(driver)?;
    require_pk_values(pk_cols, pk_values)?;
    let mut args = Vec::with_capacity(changes.len() + pk_cols.len());
    let mut sets = Vec::with_capacity(changes.len());
    for (col, value) in changes {
        args.push(value.clone());
        sets.push(format!("{} = {}", quote_ident(driver, col), placeholder(driver, args.len())));
    }
    let mut filters = Vec::with_capacity(pk_cols.len());
    for pk in pk_cols {
        args.push(pk_values[pk].clone());
        filters.push(format!("{} = {}", quote_ident(driver, pk), placeholder(driver, args.len())));
    }
    let sql =
        format!("UPDATE {} SET {} WHERE {}", table_ref(driver, schema, table), sets.join(", "), filters.join(" AND "));
    Ok((sql, args))
}

pub fn build_delete(
    driver: &DriverType,
    schema: &str,
    table: &str,
    pk_cols: &[String],
    pk_row: &Row,
) -> Result<Statement, String> {
    require_row_edits(driver)?;
    require_pk_values(pk_cols, pk_row)?;
    let mut args = Vec::with_capacity(pk_cols.len());
    let mut filters = Vec::with_capacity(pk_cols.len());
    for pk in pk_cols {
        args.push(pk_row[pk].clone());
        filters.push(format!("{} = {}", quote_ident(driver, pk), placeholder(driver, args.len())));
    }
    Ok((format!("DELETE FROM {} WHERE {}", table_ref(driver, schema, table), filters.join(" AND ")), args))
}

// Callers append any driver-specific suffix such as RETURNING *.
pub fn build_insert(driver: &DriverType, schema: &str, table: &str, values: &Row) -> Result<Statement, String> {
    if values.is_empty() {
        return Err("no column values provided".into());
    }
    let cols: Vec<String> = values.keys().map(|c| quote_ident(driver, c)).collect();
    let marks: Vec<String> = (1..=values.len()).map(|i| placeholder(driver, i)).collect();
    let sql =
        format!("INSERT INTO {} ({}) VALUES ({})", table_ref(driver, schema, table), cols.join(", "), marks.join(", "));
    Ok((sql, values.values().cloned().collect()))
}

// ClickHouse keys aren't unique, so a key can't pick out the one row to change.
fn require_row_edits(driver: &DriverType) -> Result<(), String> {
    if Dialect::for_driver(driver).row_edits { Ok(()) } else { Err(format!("{driver} can't edit single rows")) }
}

// A missing key value turns the WHERE into `pk = NULL`, a no-op that still reports success.
fn require_pk_values(pk_cols: &[String], values: &Row) -> Result<(), String> {
    if pk_cols.is_empty() {
        return Err("table has no primary key".into());
    }
    match pk_cols.iter().find(|pk| !values.contains_key(*pk)) {
        Some(pk) => Err(format!("missing primary key value for {pk:?}")),
        None => Ok(()),
    }
}

pub fn build_table_select(
    driver: &DriverType,
    schema: &str,
    req: &TableDataRequest,
    cols: &[ColumnInfo],
    pks: &[String],
) -> String {
    let mut sql = format!("SELECT * FROM {}", table_ref(driver, schema, &req.table));
    if !req.filter.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&req.filter);
    }
    // An unknown client-supplied sort column is ignored rather than reported.
    let order_by = if !req.order_by.is_empty() && column_exists(cols, &req.order_by) {
        Some(req.order_by.as_str())
    } else {
        pks.first().map(String::as_str).or_else(|| cols.first().map(|c| c.name.as_str()))
    };
    let paging = Dialect::for_driver(driver).paging;
    if let Some(order_by) = order_by.filter(|o| !o.is_empty()) {
        let dir = if req.order_dir.eq_ignore_ascii_case("DESC") { "DESC" } else { "ASC" };
        sql.push_str(&format!(" ORDER BY {} {dir}", quote_ident(driver, order_by)));
    } else if paging == Paging::OffsetFetch {
        // OFFSET ... FETCH needs an ORDER BY, even one that orders nothing.
        sql.push_str(" ORDER BY (SELECT NULL)");
    }
    // SQLite reads LIMIT -1 as "no limit" and Postgres rejects it. A negative OFFSET errors everywhere.
    let limit = if req.limit <= 0 { 100 } else { req.limit };
    let offset = req.offset.max(0);
    match paging {
        Paging::LimitOffset => sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}")),
        Paging::OffsetFetch => sql.push_str(&format!(" OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")),
    }
    sql
}

// The schema tree's "Select in new tab".
pub fn preview_select(driver: &DriverType, schema: &str, table: &str, limit: i64) -> String {
    let target = table_ref(driver, schema, table);
    match Dialect::for_driver(driver).preview {
        Preview::Limit => format!("SELECT * FROM {target} LIMIT {limit};"),
        Preview::Top => format!("SELECT TOP ({limit}) * FROM {target};"),
    }
}

pub fn first_integer_primary_key(cols: &[ColumnInfo]) -> Option<&str> {
    cols.iter().find(|c| c.is_primary && c.data_type.to_lowercase().contains("int")).map(|c| c.name.as_str())
}

pub const IMPORT_BOOL: &str = "bool";
pub const IMPORT_INT: &str = "int";
pub const IMPORT_FLOAT: &str = "float";
pub const IMPORT_DATE: &str = "date";
pub const IMPORT_TIMESTAMP: &str = "timestamp";
pub const IMPORT_TEXT: &str = "text";

pub fn sql_type_for(driver: &DriverType, import_type: &str) -> &'static str {
    let types = &Dialect::for_driver(driver).import_types;
    match import_type {
        IMPORT_BOOL => types.boolean,
        IMPORT_INT => types.int,
        IMPORT_FLOAT => types.float,
        IMPORT_DATE => types.date,
        IMPORT_TIMESTAMP => types.timestamp,
        _ => types.text,
    }
}

// Substring matches, so VARCHAR(50), LONGTEXT, NVARCHAR etc. all hit.
const EMPTY_STRING_TYPES: &[&str] = &["CHAR", "TEXT", "CLOB", "STRING", "ENUM", "SET", "BINARY", "BLOB", "BYTEA"];
const NON_TEXT_TYPES: &[&str] = &[
    "INT", "SERIAL", "DECIMAL", "NUMERIC", "FLOAT", "DOUBLE", "REAL", "MONEY", "BIT", "BOOL", "DATE", "TIME", "YEAR",
    "JSON", "UUID", "INTERVAL", "XML", "OID", "ARRAY",
];

// Unknown or blank types are assumed to hold ''.
pub fn accepts_empty_string(data_type: &str) -> bool {
    let upper = to_upper(data_type.trim());
    if upper.is_empty() || EMPTY_STRING_TYPES.iter().any(|t| upper.contains(t)) {
        return true;
    }
    !NON_TEXT_TYPES.iter().any(|t| upper.contains(t))
}

// All columns are nullable so a partly blank source column can't fail the whole load.
pub fn build_import_create_table(
    driver: &DriverType,
    schema: &str,
    table: &str,
    columns: &[String],
    types: &[String],
) -> Result<String, String> {
    if columns.is_empty() {
        return Err("no columns to create".into());
    }
    if types.len() != columns.len() {
        return Err(format!("got {} columns but {} types", columns.len(), types.len()));
    }
    let cols: Vec<DdlColumn> = columns
        .iter()
        .zip(types)
        .map(|(name, t)| DdlColumn {
            name: name.clone(),
            data_type: sql_type_for(driver, t).into(),
            ..Default::default()
        })
        .collect();
    Ok(compose_create_table(driver, schema, table, &cols, &[]))
}

pub fn build_batch_insert(
    driver: &DriverType,
    schema: &str,
    table: &str,
    columns: &[String],
    rows: &[Vec<Value>],
) -> Result<Statement, String> {
    build_batch_insert_with(driver, schema, table, columns, rows, &[])
}

// `wraps[c]`, where set, is SQL with `{}` for column c's placeholder, like a CONVERT the server needs.
pub fn build_batch_insert_with(
    driver: &DriverType,
    schema: &str,
    table: &str,
    columns: &[String],
    rows: &[Vec<Value>],
    wraps: &[Option<&str>],
) -> Result<Statement, String> {
    if columns.is_empty() {
        return Err("no target columns".into());
    }
    if rows.is_empty() {
        return Err("no rows to insert".into());
    }
    let mut sql = format!(
        "INSERT INTO {} ({}) VALUES ",
        qualified_table(driver, schema, table),
        quote_ident_list(driver, columns)
    );
    let mut args = Vec::with_capacity(rows.len() * columns.len());
    for (r, row) in rows.iter().enumerate() {
        if row.len() != columns.len() {
            return Err(format!("row {r} has {} values, want {}", row.len(), columns.len()));
        }
        if r > 0 {
            sql.push_str(", ");
        }
        sql.push('(');
        for (c, value) in row.iter().enumerate() {
            if c > 0 {
                sql.push_str(", ");
            }
            args.push(value.clone());
            let mark = placeholder(driver, args.len());
            match wraps.get(c).copied().flatten() {
                Some(wrap) => sql.push_str(&wrap.replace("{}", &mark)),
                None => sql.push_str(&mark),
            }
        }
        sql.push(')');
    }
    Ok((sql, args))
}

// SQL Server's COUNT stops at 2^31 - 1, so it counts in bigint there.
pub fn build_count(driver: &DriverType, schema: &str, table: &str) -> String {
    let count = match Dialect::for_driver(driver).id {
        Some(SqlDialect::TSql) => "COUNT_BIG(*)",
        Some(SqlDialect::Postgres | SqlDialect::MySql | SqlDialect::Sqlite | SqlDialect::ClickHouse) | None => {
            "COUNT(*)"
        }
    };
    format!("SELECT {count} FROM {}", table_ref(driver, schema, table))
}

// DELETE, because SQLite has no TRUNCATE and elsewhere it needs extra privileges. ClickHouse's DELETE is a
// mutation that rewrites parts, so it truncates.
pub fn build_truncate(driver: &DriverType, schema: &str, table: &str) -> String {
    let target = qualified_table(driver, schema, table);
    match Dialect::for_driver(driver).id {
        Some(SqlDialect::ClickHouse) => format!("TRUNCATE TABLE {target}"),
        Some(SqlDialect::Postgres | SqlDialect::MySql | SqlDialect::Sqlite | SqlDialect::TSql) | None => {
            format!("DELETE FROM {target}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, Value)]) -> Row {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn update_and_delete_require_every_key() {
        let pg = DriverType::Postgres;
        let id = cols(&["id"]);
        assert!(build_update(&pg, "public", "t", &Row::new(), &row(&[("id", 1.into())]), &id).is_err());
        assert!(build_update(&pg, "public", "t", &row(&[("name", "x".into())]), &Row::new(), &id).is_err());
        let (sql, args) =
            build_update(&pg, "public", "t", &row(&[("name", "x".into())]), &row(&[("id", 1.into())]), &id).unwrap();
        assert!(!sql.is_empty() && args.len() == 2);
        assert!(build_delete(&pg, "public", "t", &id, &Row::new()).is_err());
        assert!(build_delete(&pg, "public", "t", &[], &row(&[("id", 1.into())])).is_err());
        assert!(build_delete(&pg, "public", "t", &id, &row(&[("id", 1.into())])).is_ok());
        assert!(build_insert(&pg, "public", "t", &Row::new()).is_err());
        assert!(build_insert(&pg, "public", "t", &row(&[("name", "x".into())])).is_ok());
    }

    #[test]
    fn generated_sql_is_deterministic() {
        let pg = DriverType::Postgres;
        let values = row(&[("b", 2.into()), ("a", 1.into()), ("c", 3.into())]);
        let (sql, _) = build_insert(&pg, "public", "t", &values).unwrap();
        assert_eq!(sql, r#"INSERT INTO "public"."t" ("a", "b", "c") VALUES ($1, $2, $3)"#);
        let (sql, args) = build_update(&pg, "public", "t", &values, &row(&[("id", 9.into())]), &cols(&["id"])).unwrap();
        assert_eq!(sql, r#"UPDATE "public"."t" SET "a" = $1, "b" = $2, "c" = $3 WHERE "id" = $4"#);
        assert_eq!(args, vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(9)]);
    }

    #[test]
    fn import_types_per_driver() {
        let cases = [
            (DriverType::Postgres, IMPORT_INT, "bigint"),
            (DriverType::Postgres, IMPORT_BOOL, "boolean"),
            (DriverType::Postgres, IMPORT_TIMESTAMP, "timestamp"),
            (DriverType::MySql, IMPORT_INT, "BIGINT"),
            (DriverType::MySql, IMPORT_BOOL, "TINYINT(1)"),
            (DriverType::MySql, IMPORT_TIMESTAMP, "DATETIME"),
            (DriverType::Sqlite, IMPORT_INT, "INTEGER"),
            (DriverType::Sqlite, IMPORT_FLOAT, "REAL"),
            (DriverType::Sqlite, IMPORT_DATE, "TEXT"),
            (DriverType::Postgres, "nonsense", "text"),
            (DriverType::Turso, IMPORT_FLOAT, "REAL"),
            (DriverType::SqlServer, IMPORT_TIMESTAMP, "DATETIME2"),
            (DriverType::ClickHouse, IMPORT_INT, "Nullable(Int64)"),
        ];
        for (driver, t, want) in cases {
            assert_eq!(sql_type_for(&driver, t), want, "{driver} {t}");
        }
    }

    #[test]
    fn paging_and_previews_follow_the_dialect() {
        let req = TableDataRequest { table: "t".into(), limit: 50, offset: 100, ..Default::default() };
        let mssql = DriverType::SqlServer;
        assert_eq!(
            build_table_select(&mssql, "dbo", &req, &[], &[]),
            "SELECT * FROM [dbo].[t] ORDER BY (SELECT NULL) OFFSET 100 ROWS FETCH NEXT 50 ROWS ONLY"
        );
        assert_eq!(
            build_table_select(&mssql, "dbo", &req, &[], &["id".into()]),
            "SELECT * FROM [dbo].[t] ORDER BY [id] ASC OFFSET 100 ROWS FETCH NEXT 50 ROWS ONLY"
        );
        assert_eq!(preview_select(&mssql, "dbo", "t", 100), "SELECT TOP (100) * FROM [dbo].[t];");
        assert_eq!(
            preview_select(&DriverType::Postgres, "public", "t", 100),
            "SELECT * FROM \"public\".\"t\" LIMIT 100;"
        );
        assert_eq!(preview_select(&DriverType::Turso, "main", "t", 100), "SELECT * FROM \"t\" LIMIT 100;");
        let edit = build_update(
            &DriverType::ClickHouse,
            "db",
            "t",
            &row(&[("a", 1.into())]),
            &row(&[("id", 1.into())]),
            &cols(&["id"]),
        );
        assert!(edit.is_err());
        assert_eq!(build_truncate(&DriverType::ClickHouse, "db", "t"), "TRUNCATE TABLE `db`.`t`");
    }

    #[test]
    fn import_create_table_is_all_nullable() {
        let got = build_import_create_table(
            &DriverType::Postgres,
            "public",
            "people",
            &cols(&["id", "name", "joined"]),
            &cols(&[IMPORT_INT, IMPORT_TEXT, IMPORT_DATE]),
        )
        .unwrap();
        assert_eq!(
            got,
            "CREATE TABLE \"public\".\"people\" (\n    \"id\" bigint,\n    \"name\" text,\n    \"joined\" date\n);"
        );
        assert!(
            build_import_create_table(&DriverType::Postgres, "public", "t", &cols(&["a", "b"]), &cols(&[IMPORT_INT]))
                .is_err()
        );
        assert!(build_import_create_table(&DriverType::Postgres, "public", "t", &[], &[]).is_err());
    }

    #[test]
    fn batch_insert() {
        let (sql, args) = build_batch_insert(
            &DriverType::Postgres,
            "public",
            "people",
            &cols(&["id", "name"]),
            &[vec![1.into(), "Alice".into()], vec![2.into(), Value::Null]],
        )
        .unwrap();
        assert_eq!(sql, r#"INSERT INTO "public"."people" ("id", "name") VALUES ($1, $2), ($3, $4)"#);
        assert_eq!(args, vec![Value::Int(1), "Alice".into(), Value::Int(2), Value::Null]);
        let (sql, args) =
            build_batch_insert(&DriverType::MySql, "shop", "orders", &cols(&["id"]), &[vec![1.into()], vec![2.into()]])
                .unwrap();
        assert_eq!(sql, "INSERT INTO `shop`.`orders` (`id`) VALUES (?), (?)");
        assert_eq!(args.len(), 2);
        let (sql, args) = build_batch_insert(
            &DriverType::Postgres,
            "public",
            "t",
            &cols(&["c"]),
            &[vec!["'); DROP TABLE t; --".into()]],
        )
        .unwrap();
        assert!(!sql.contains("DROP TABLE"));
        assert_eq!(args[0], Value::from("'); DROP TABLE t; --"));
        assert!(
            build_batch_insert(
                &DriverType::Postgres,
                "public",
                "t",
                &cols(&["a", "b"]),
                &[vec![1.into(), 2.into()], vec![3.into()]]
            )
            .is_err()
        );
        assert!(build_batch_insert(&DriverType::Postgres, "public", "t", &[], &[vec![1.into()]]).is_err());
        assert!(build_batch_insert(&DriverType::Postgres, "public", "t", &cols(&["a"]), &[]).is_err());
    }

    #[test]
    fn truncate_is_a_delete() {
        assert_eq!(build_truncate(&DriverType::Postgres, "public", "t"), r#"DELETE FROM "public"."t""#);
        assert_eq!(build_truncate(&DriverType::MySql, "shop", "t"), "DELETE FROM `shop`.`t`");
    }

    #[test]
    fn empty_string_acceptance() {
        let cases = [
            ("TEXT", true),
            ("text", true),
            ("VARCHAR(50)", true),
            ("character varying", true),
            ("LONGTEXT", true),
            ("NVARCHAR(10)", true),
            ("CHAR(1)", true),
            ("ENUM('a','b')", true),
            ("BYTEA", true),
            ("BLOB", true),
            ("", true),
            ("widget_status", true),
            ("INT", false),
            ("INTEGER", false),
            ("BIGINT", false),
            ("SERIAL", false),
            ("DECIMAL(10,2)", false),
            ("numeric", false),
            ("DOUBLE PRECISION", false),
            ("REAL", false),
            ("BOOLEAN", false),
            ("DATE", false),
            ("DATETIME", false),
            ("TIMESTAMP", false),
            ("TIME", false),
            ("JSONB", false),
            ("UUID", false),
        ];
        for (data_type, want) in cases {
            assert_eq!(accepts_empty_string(data_type), want, "{data_type}");
        }
    }

    #[test]
    fn table_select_defaults() {
        let columns = vec![
            ColumnInfo { name: "id".into(), is_primary: true, ..Default::default() },
            ColumnInfo { name: "name".into(), ..Default::default() },
        ];
        let req = |order_by: &str, dir: &str, filter: &str, limit: i64, offset: i64| TableDataRequest {
            table: "t".into(),
            order_by: order_by.into(),
            order_dir: dir.into(),
            filter: filter.into(),
            limit,
            offset,
            ..Default::default()
        };
        let pg = DriverType::Postgres;
        assert_eq!(
            build_table_select(&pg, "s", &req("", "", "", 0, -5), &columns, &cols(&["id"])),
            r#"SELECT * FROM "s"."t" ORDER BY "id" ASC LIMIT 100 OFFSET 0"#
        );
        assert_eq!(
            build_table_select(&pg, "s", &req("name", "desc", "id > 1", 50, 10), &columns, &cols(&["id"])),
            r#"SELECT * FROM "s"."t" WHERE id > 1 ORDER BY "name" DESC LIMIT 50 OFFSET 10"#
        );
        assert_eq!(
            build_table_select(&DriverType::Sqlite, "main", &req("nope", "", "", 10, 0), &columns, &[]),
            r#"SELECT * FROM "t" ORDER BY "id" ASC LIMIT 10 OFFSET 0"#
        );
    }
}
