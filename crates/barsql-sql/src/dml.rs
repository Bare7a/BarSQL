use barsql_core::{ColumnInfo, DriverType, Row, SqlDialect, TableDataRequest, Value, schema::column_exists};

use crate::ddl::{DdlColumn, compose_create_table};
use crate::dialect::{Dialect, Paging, Preview};
use crate::quote::{placeholder, qualified_table, quote_ident, quote_ident_list, table_ref};
use crate::sql_text::to_upper;

pub type Statement = (String, Vec<Value>);

// MySQL reads text into a BIT column as bytes, so bit values go as binary digits through CONV. `{}` is the placeholder.
pub const MYSQL_BITS: &str = "CAST(CONV({}, 2, 10) AS UNSIGNED)";

// `columns` is the table's catalog, for values that need converting like MySQL's BIT. Missing columns go as they are.
pub fn build_update(
    driver: &DriverType,
    schema: &str,
    table: &str,
    changes: &Row,
    pk_values: &Row,
    pk_cols: &[String],
    columns: &[ColumnInfo],
) -> Result<Statement, String> {
    if changes.is_empty() {
        return Err("no changes to apply".into());
    }
    require_row_edits(driver)?;
    require_pk_values(pk_cols, pk_values)?;
    let mut args = Vec::with_capacity(changes.len() + pk_cols.len());
    let mut sets = Vec::with_capacity(changes.len());
    for (col, value) in changes {
        let mark = bind(driver, columns, col, value, &mut args)?;
        sets.push(format!("{} = {mark}", quote_ident(driver, col)));
    }
    let mut filters = Vec::with_capacity(pk_cols.len());
    for pk in pk_cols {
        let mark = bind(driver, columns, pk, &pk_values[pk], &mut args)?;
        filters.push(format!("{} = {mark}", quote_ident(driver, pk)));
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
    columns: &[ColumnInfo],
) -> Result<Statement, String> {
    require_row_edits(driver)?;
    require_pk_values(pk_cols, pk_row)?;
    let mut args = Vec::with_capacity(pk_cols.len());
    let mut filters = Vec::with_capacity(pk_cols.len());
    for pk in pk_cols {
        let mark = bind(driver, columns, pk, &pk_row[pk], &mut args)?;
        filters.push(format!("{} = {mark}", quote_ident(driver, pk)));
    }
    Ok((format!("DELETE FROM {} WHERE {}", table_ref(driver, schema, table), filters.join(" AND ")), args))
}

// Callers append any driver-specific suffix such as RETURNING *.
pub fn build_insert(
    driver: &DriverType,
    schema: &str,
    table: &str,
    values: &Row,
    columns: &[ColumnInfo],
) -> Result<Statement, String> {
    build_insert_with(driver, schema, table, values, columns, None)
}

// `output` goes before VALUES, like T-SQL's OUTPUT INSERTED.*.
pub fn build_insert_with(
    driver: &DriverType,
    schema: &str,
    table: &str,
    values: &Row,
    columns: &[ColumnInfo],
    output: Option<&str>,
) -> Result<Statement, String> {
    if values.is_empty() {
        return Err("no column values provided".into());
    }
    let cols: Vec<String> = values.keys().map(|c| quote_ident(driver, c)).collect();
    let mut args = Vec::with_capacity(values.len());
    let mut marks = Vec::with_capacity(values.len());
    for (col, value) in values {
        marks.push(bind(driver, columns, col, value, &mut args)?);
    }
    let output = output.map(|clause| format!(" {clause}")).unwrap_or_default();
    let sql = format!(
        "INSERT INTO {} ({}){output} VALUES ({})",
        table_ref(driver, schema, table),
        cols.join(", "),
        marks.join(", ")
    );
    Ok((sql, args))
}

fn bind(
    driver: &DriverType,
    columns: &[ColumnInfo],
    column: &str,
    value: &Value,
    args: &mut Vec<Value>,
) -> Result<String, String> {
    let info = columns
        .iter()
        .find(|c| c.name == column)
        .or_else(|| columns.iter().find(|c| c.name.eq_ignore_ascii_case(column)));
    let data_type = info.map(|c| c.data_type.as_str());
    if data_type.is_some_and(|t| is_bit_column(driver.dialect(), t)) {
        args.push(bit_digits(column, value)?);
        return Ok(MYSQL_BITS.replace("{}", &placeholder(driver, args.len())));
    }
    if let (Value::Text(text), Some(data_type)) = (value, data_type)
        && let Some(mark) = bind_bytes(driver, data_type, text, args)
    {
        return Ok(mark);
    }
    args.push(value.clone());
    Ok(placeholder(driver, args.len()))
}

// Binary columns take bytes as the grid shows them: UTF-8 text, or \x and hex. SQLite types are only hints, so there
// \x reads as bytes in any column without text affinity.
fn bind_bytes(driver: &DriverType, data_type: &str, text: &str, args: &mut Vec<Value>) -> Option<String> {
    let dialect = driver.dialect();
    let hex = match dialect {
        Some(SqlDialect::Sqlite) if !text_affinity(data_type) => hex_digits(text)?.to_ascii_uppercase(),
        Some(SqlDialect::Sqlite) => return None,
        _ if is_binary_type(dialect, data_type) => match hex_digits(text) {
            Some(hex) => hex.to_ascii_uppercase(),
            None => text.bytes().map(|b| format!("{b:02X}")).collect(),
        },
        _ => return None,
    };
    let mut param = |value: String| {
        args.push(Value::Text(value));
        placeholder(driver, args.len())
    };
    Some(match dialect {
        Some(SqlDialect::Postgres) => param(format!("\\x{hex}")),
        Some(SqlDialect::MySql) => format!("UNHEX({})", param(hex)),
        Some(SqlDialect::TSql) => format!("CONVERT(varbinary(max), {}, 1)", param(format!("0x{hex}"))),
        // Some libSQL servers predate unhex(). The digits are checked.
        Some(SqlDialect::Sqlite) => format!("X'{hex}'"),
        Some(SqlDialect::ClickHouse) | None => return None,
    })
}

fn hex_digits(text: &str) -> Option<&str> {
    text.strip_prefix("\\x")
        .filter(|hex| !hex.is_empty() && hex.len().is_multiple_of(2) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

// SQLite's rule: INT first, then CHAR, CLOB or TEXT.
fn text_affinity(data_type: &str) -> bool {
    let t = data_type.to_ascii_uppercase();
    !t.contains("INT") && ["CHAR", "CLOB", "TEXT"].iter().any(|k| t.contains(k))
}

fn is_binary_type(dialect: Option<SqlDialect>, data_type: &str) -> bool {
    let base = data_type.split('(').next().unwrap_or_default().trim().to_ascii_lowercase();
    match dialect {
        Some(SqlDialect::Postgres) => base == "bytea",
        Some(SqlDialect::MySql) => {
            matches!(base.as_str(), "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob")
        }
        Some(SqlDialect::TSql) => matches!(base.as_str(), "binary" | "varbinary" | "image"),
        Some(SqlDialect::Sqlite | SqlDialect::ClickHouse) | None => false,
    }
}

// Only MySQL's BIT needs its digits converted. SQL Server's is a boolean.
pub fn is_bit_column(dialect: Option<SqlDialect>, data_type: &str) -> bool {
    dialect == Some(SqlDialect::MySql)
        && data_type.split('(').next().is_some_and(|t| t.trim().eq_ignore_ascii_case("bit"))
}

pub fn is_bit_string(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b == b'0' || b == b'1')
}

// Numbers keep their value, as MySQL stores them in a BIT column, a negative one as its 64-bit two's complement.
pub fn bit_digits(column: &str, value: &Value) -> Result<Value, String> {
    let text = match value {
        Value::Null => return Ok(Value::Null),
        Value::Bool(b) => return Ok(Value::Text(u8::from(*b).to_string())),
        Value::Int(n) => return Ok(Value::Text(format!("{:b}", *n as u64))),
        Value::Float(f) => return Err(format!("column {column:?} takes binary digits like 1010, got {f}")),
        Value::Text(s) => s.trim(),
    };
    if !is_bit_string(text) {
        return Err(format!("column {column:?} takes binary digits like 1010, got {text:?}"));
    }
    // CONV saturates past 64 bits, the most a BIT column holds.
    let significant = text.trim_start_matches('0').len();
    if significant > 64 {
        return Err(format!("column {column:?} holds at most 64 bits, got {significant}"));
    }
    Ok(Value::Text(text.to_string()))
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

// SQL Server's COUNT stops at 2^31 - 1, so it counts in bigint there. `filter` is a validated WHERE condition, or
// empty for every row.
pub fn build_count(driver: &DriverType, schema: &str, table: &str, filter: &str) -> String {
    let count = match Dialect::for_driver(driver).id {
        Some(SqlDialect::TSql) => "COUNT_BIG(*)",
        Some(SqlDialect::Postgres | SqlDialect::MySql | SqlDialect::Sqlite | SqlDialect::ClickHouse) | None => {
            "COUNT(*)"
        }
    };
    let mut sql = format!("SELECT {count} FROM {}", table_ref(driver, schema, table));
    if !filter.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(filter);
    }
    sql
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
        assert!(build_update(&pg, "public", "t", &Row::new(), &row(&[("id", 1.into())]), &id, &[]).is_err());
        assert!(build_update(&pg, "public", "t", &row(&[("name", "x".into())]), &Row::new(), &id, &[]).is_err());
        let (sql, args) =
            build_update(&pg, "public", "t", &row(&[("name", "x".into())]), &row(&[("id", 1.into())]), &id, &[])
                .unwrap();
        assert!(!sql.is_empty() && args.len() == 2);
        assert!(build_delete(&pg, "public", "t", &id, &Row::new(), &[]).is_err());
        assert!(build_delete(&pg, "public", "t", &[], &row(&[("id", 1.into())]), &[]).is_err());
        assert!(build_delete(&pg, "public", "t", &id, &row(&[("id", 1.into())]), &[]).is_ok());
        assert!(build_insert(&pg, "public", "t", &Row::new(), &[]).is_err());
        assert!(build_insert(&pg, "public", "t", &row(&[("name", "x".into())]), &[]).is_ok());
    }

    #[test]
    fn generated_sql_is_deterministic() {
        let pg = DriverType::Postgres;
        let values = row(&[("b", 2.into()), ("a", 1.into()), ("c", 3.into())]);
        let (sql, _) = build_insert(&pg, "public", "t", &values, &[]).unwrap();
        assert_eq!(sql, r#"INSERT INTO "public"."t" ("a", "b", "c") VALUES ($1, $2, $3)"#);
        let (sql, args) =
            build_update(&pg, "public", "t", &values, &row(&[("id", 9.into())]), &cols(&["id"]), &[]).unwrap();
        assert_eq!(sql, r#"UPDATE "public"."t" SET "a" = $1, "b" = $2, "c" = $3 WHERE "id" = $4"#);
        assert_eq!(args, vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(9)]);
    }

    #[test]
    fn mysql_bit_columns_take_their_digits_through_conv() {
        let columns = [
            ColumnInfo { name: "id".into(), data_type: "int".into(), is_primary: true, ..Default::default() },
            ColumnInfo { name: "Flags".into(), data_type: "bit".into(), ..Default::default() },
            ColumnInfo { name: "mask".into(), data_type: "BIT(64)".into(), is_primary: true, ..Default::default() },
        ];
        let my = DriverType::MySql;
        let keys = cols(&["id", "mask"]);
        let key = row(&[("id", 1.into()), ("mask", "0101".into())]);
        let (sql, args) =
            build_update(&my, "s", "t", &row(&[("flags", " 1010 ".into())]), &key, &keys, &columns).unwrap();
        assert_eq!(
            sql,
            "UPDATE `s`.`t` SET `flags` = CAST(CONV(?, 2, 10) AS UNSIGNED) \
             WHERE `id` = ? AND `mask` = CAST(CONV(?, 2, 10) AS UNSIGNED)"
        );
        assert_eq!(args, vec![Value::from("1010"), Value::Int(1), Value::from("0101")]);
        let (sql, args) = build_delete(&my, "s", "t", &keys, &key, &columns).unwrap();
        assert_eq!(sql, "DELETE FROM `s`.`t` WHERE `id` = ? AND `mask` = CAST(CONV(?, 2, 10) AS UNSIGNED)");
        assert_eq!(args, vec![Value::Int(1), Value::from("0101")]);
        let values = row(&[("id", 2.into()), ("flags", Value::Null), ("mask", Value::Int(-1)), ("note", "1".into())]);
        let (sql, args) = build_insert(&my, "s", "t", &values, &columns).unwrap();
        assert_eq!(
            sql,
            "INSERT INTO `s`.`t` (`flags`, `id`, `mask`, `note`) VALUES (CAST(CONV(?, 2, 10) AS UNSIGNED), ?, \
             CAST(CONV(?, 2, 10) AS UNSIGNED), ?)"
        );
        assert_eq!(args, vec![Value::Null, Value::Int(2), Value::from("1".repeat(64).as_str()), Value::from("1")]);

        // CONV would read these as 1 and 0 without a word, and saturate past 64 bits.
        for bad in ["12", "abc", "", "0x0A", "b'1010'"] {
            let err = build_update(&my, "s", "t", &row(&[("flags", bad.into())]), &key, &keys, &columns).unwrap_err();
            assert_eq!(err, format!("column \"flags\" takes binary digits like 1010, got {:?}", bad), "{bad:?}");
        }
        let long = format!("000{}", "1".repeat(65));
        let err = build_insert(&my, "s", "t", &row(&[("flags", long.as_str().into())]), &columns).unwrap_err();
        assert_eq!(err, "column \"flags\" holds at most 64 bits, got 65");
        assert_eq!(
            bit_digits("b", &format!("000{}", "1".repeat(64)).into()),
            Ok(format!("000{}", "1".repeat(64)).into())
        );
        assert_eq!(bit_digits("b", &Value::Bool(true)), Ok("1".into()));
        assert_eq!(bit_digits("b", &Value::Int(10)), Ok("1010".into()));
        assert!(bit_digits("b", &Value::Float(2.5)).is_err());

        for driver in [DriverType::Postgres, DriverType::SqlServer] {
            let (sql, args) = build_insert(&driver, "s", "t", &row(&[("flags", "1".into())]), &columns).unwrap();
            assert!(!sql.contains("CONV"), "{sql}");
            assert_eq!(args, vec![Value::from("1")]);
        }
    }

    #[test]
    fn binary_columns_take_bytes_as_the_grid_shows_them() {
        let column =
            |data_type: &str| ColumnInfo { name: "id".into(), data_type: data_type.into(), ..Default::default() };
        let cases: [(DriverType, &str, &str, &str, Option<&str>); 11] = [
            (DriverType::MySql, "binary", r"\xdeadbeef", "DELETE FROM `s`.`t` WHERE `id` = UNHEX(?)", Some("DEADBEEF")),
            (DriverType::MySql, "varbinary", "abc", "DELETE FROM `s`.`t` WHERE `id` = UNHEX(?)", Some("616263")),
            (DriverType::MySql, "varchar", r"\xdead", "DELETE FROM `s`.`t` WHERE `id` = ?", Some(r"\xdead")),
            (DriverType::Postgres, "bytea", r"a\b", r#"DELETE FROM "s"."t" WHERE "id" = $1"#, Some(r"\x615C62")),
            (
                DriverType::SqlServer,
                "varbinary(max)",
                r"\xdead",
                "DELETE FROM [s].[t] WHERE [id] = CONVERT(varbinary(max), @P1, 1)",
                Some("0xDEAD"),
            ),
            (DriverType::Sqlite, "BLOB", r"\xDEADbeef", r#"DELETE FROM "t" WHERE "id" = X'DEADBEEF'"#, None),
            (DriverType::Turso, "", r"\xdeadbeef", r#"DELETE FROM "t" WHERE "id" = X'DEADBEEF'"#, None),
            (DriverType::Sqlite, "UUID", r"\x0102", r#"DELETE FROM "t" WHERE "id" = X'0102'"#, None),
            (DriverType::Sqlite, "BLOB", "abc", r#"DELETE FROM "t" WHERE "id" = ?"#, Some("abc")),
            (DriverType::Sqlite, "VARCHAR(10)", r"\xdead", r#"DELETE FROM "t" WHERE "id" = ?"#, Some(r"\xdead")),
            (DriverType::Sqlite, "TEXT", r"\xnot hex", r#"DELETE FROM "t" WHERE "id" = ?"#, Some(r"\xnot hex")),
        ];
        for (driver, data_type, shown, want, arg) in cases {
            let (sql, args) =
                build_delete(&driver, "s", "t", &cols(&["id"]), &row(&[("id", shown.into())]), &[column(data_type)])
                    .unwrap();
            assert_eq!(sql, want, "{driver} {data_type} {shown}");
            assert_eq!(args, arg.map(|a| vec![Value::from(a)]).unwrap_or_default(), "{driver} {data_type} {shown}");
        }
        let (sql, args) = build_update(
            &DriverType::MySql,
            "s",
            "t",
            &row(&[("id", r"\x0102".into())]),
            &row(&[("id", "x".into())]),
            &cols(&["id"]),
            &[column("blob")],
        )
        .unwrap();
        assert_eq!(sql, "UPDATE `s`.`t` SET `id` = UNHEX(?) WHERE `id` = UNHEX(?)");
        assert_eq!(args, vec![Value::from("0102"), Value::from("78")]);
        let (sql, args) =
            build_insert(&DriverType::Sqlite, "main", "t", &row(&[("id", r"\xdead".into())]), &[]).unwrap();
        assert_eq!((sql.as_str(), args), (r#"INSERT INTO "t" ("id") VALUES (?)"#, vec![Value::from(r"\xdead")]));
    }

    #[test]
    fn insert_clauses_go_between_the_columns_and_values() {
        let values = row(&[("x) VALUES (", 1.into())]);
        let (sql, _) =
            build_insert_with(&DriverType::SqlServer, "dbo", "t", &values, &[], Some("OUTPUT INSERTED.*")).unwrap();
        assert_eq!(sql, "INSERT INTO [dbo].[t] ([x) VALUES (]) OUTPUT INSERTED.* VALUES (@P1)");
        let (plain, _) = build_insert(&DriverType::SqlServer, "dbo", "t", &values, &[]).unwrap();
        assert_eq!(plain, "INSERT INTO [dbo].[t] ([x) VALUES (]) VALUES (@P1)");
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
            &[],
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
