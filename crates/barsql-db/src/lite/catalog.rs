// The SQLite catalog, over any LiteSource. Pragmas are read through their table-valued forms and joined, so a
// tree expansion is one query even over HTTP.

use std::sync::LazyLock;

use barsql_core::schema::primary_keys;
use barsql_core::{
    ColumnInfo, ConstraintInfo, DriverType, FunctionInfo, FunctionKind, FunctionList, IndexInfo, ObjectKind, ObjectRef,
    QueryError, TableInfo, TriggerInfo,
};
use barsql_sql::ddl::{join_ddl, render_create_index, terminate_statement, unsupported_ddl};
use regex::Regex;

use super::{LiteRow, LiteSource, LiteValue};
use crate::dump::DumpColumn;

const LITE: DriverType = DriverType::Sqlite;

fn text(value: &str) -> LiteValue {
    LiteValue::Text(value.to_string())
}

pub(crate) fn tables(src: &mut dyn LiteSource) -> Result<Vec<TableInfo>, QueryError> {
    let rows = src.query(
        "SELECT type, name FROM sqlite_master
        WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%'
        ORDER BY name",
        &[],
    )?;
    Ok(rows.rows.iter().map(|r| TableInfo { schema: "main".into(), kind: r.text(0), name: r.text(1) }).collect())
}

// Unlike table_info, table_xinfo lists generated columns: hidden 2 (virtual) or 3 (stored). Hidden 1 is a virtual
// table's hidden column, which SELECT * leaves out too. A column in several foreign keys takes the first.
pub(crate) fn columns(src: &mut dyn LiteSource, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
    let rows = src.query(
        "SELECT x.name, x.type, x.\"notnull\", x.dflt_value, x.pk, x.hidden, f.\"table\", f.\"to\"
        FROM pragma_table_xinfo(?1) x
        LEFT JOIN pragma_foreign_key_list(?1) f ON f.\"from\" = x.name
        ORDER BY x.cid, f.id, f.seq",
        &[text(table)],
    )?;
    let mut out: Vec<ColumnInfo> = Vec::new();
    for r in &rows.rows {
        let name = r.text(0);
        let hidden = r.int(5);
        if hidden == 1 || out.last().is_some_and(|c| c.name == name) {
            continue;
        }
        let foreign_table = r.text(6);
        out.push(ColumnInfo {
            name,
            data_type: r.text(1),
            is_nullable: r.int(2) == 0,
            is_primary: r.int(4) > 0,
            is_foreign: !foreign_table.is_empty(),
            foreign_column: if foreign_table.is_empty() { String::new() } else { r.text(7) },
            foreign_table,
            default_val: r.text(3),
            is_computed: matches!(hidden, 2 | 3),
            ..Default::default()
        });
    }
    // A lone INTEGER PRIMARY KEY is an alias for the rowid, which SQLite fills in.
    if let [key] = out.iter_mut().filter(|c| c.is_primary).collect::<Vec<_>>().as_mut_slice() {
        key.is_identity = key.data_type.eq_ignore_ascii_case("integer");
    }
    Ok(out)
}

struct IndexEntry {
    name: String,
    unique: bool,
    // "c" for CREATE INDEX, "u" for a UNIQUE constraint, "pk" for PRIMARY KEY.
    origin: String,
    // Expression parts have no name and are left out.
    columns: Vec<String>,
}

fn index_entries(src: &mut dyn LiteSource, table: &str) -> Result<Vec<IndexEntry>, QueryError> {
    let rows = src.query(
        "SELECT il.name, il.\"unique\", il.origin, ii.name
        FROM pragma_index_list(?1) il
        LEFT JOIN pragma_index_info(il.name) ii
        ORDER BY il.seq, ii.seqno",
        &[text(table)],
    )?;
    let mut out: Vec<IndexEntry> = Vec::new();
    for r in &rows.rows {
        let name = r.text(0);
        if out.last().is_none_or(|e| e.name != name) {
            out.push(IndexEntry { name, unique: r.int(1) == 1, origin: r.text(2), columns: Vec::new() });
        }
        if let (Some(entry), Some(column)) = (out.last_mut(), r.opt_text(3)) {
            entry.columns.push(column);
        }
    }
    Ok(out)
}

fn index_info(entry: IndexEntry, table: &str) -> IndexInfo {
    IndexInfo {
        name: entry.name,
        schema: "main".into(),
        table: table.to_string(),
        columns: entry.columns,
        is_primary: entry.origin == "pk",
        is_unique: entry.unique,
        method: String::new(),
    }
}

pub(crate) fn indexes(src: &mut dyn LiteSource, table: &str) -> Result<Vec<IndexInfo>, QueryError> {
    Ok(index_entries(src, table)?.into_iter().map(|e| index_info(e, table)).collect())
}

// No pragma lists CHECK constraints, so they only show up in the table's DDL.
pub(crate) fn constraints(src: &mut dyn LiteSource, table: &str) -> Result<Vec<ConstraintInfo>, QueryError> {
    let mut out = Vec::new();
    let pks = primary_keys(&columns(src, table)?);
    if !pks.is_empty() {
        out.push(ConstraintInfo {
            schema: "main".into(),
            table: table.to_string(),
            kind: "PRIMARY KEY".into(),
            columns: pks,
            ..Default::default()
        });
    }
    for e in index_entries(src, table)?.into_iter().filter(|e| e.origin == "u") {
        out.push(ConstraintInfo {
            name: e.name,
            schema: "main".into(),
            table: table.to_string(),
            kind: "UNIQUE".into(),
            columns: e.columns,
            ..Default::default()
        });
    }
    let rows = src.query(
        "SELECT id, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?1) ORDER BY id, seq",
        &[text(table)],
    )?;
    let mut fks: Vec<(i64, ConstraintInfo)> = Vec::new();
    for r in &rows.rows {
        let id = r.int(0);
        if fks.last().is_none_or(|(last, _)| *last != id) {
            let fk = ConstraintInfo {
                schema: "main".into(),
                table: table.to_string(),
                kind: "FOREIGN KEY".into(),
                ref_table: r.text(1),
                ..Default::default()
            };
            fks.push((id, fk));
        }
        let entry = &mut fks.last_mut().expect("pushed").1;
        entry.columns.push(r.text(2));
        // NULL when the key references the target's primary key implicitly.
        if let Some(to) = r.opt_text(3) {
            entry.ref_columns.push(to);
        }
    }
    out.extend(fks.into_iter().map(|(_, c)| c));
    Ok(out)
}

pub(crate) fn triggers(src: &mut dyn LiteSource, table: &str) -> Result<Vec<TriggerInfo>, QueryError> {
    let rows = src.query(
        "SELECT name, COALESCE(sql, '') FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?1 ORDER BY name",
        &[text(table)],
    )?;
    Ok(rows
        .rows
        .iter()
        .map(|r| {
            let (timing, events) = parse_trigger_head(&r.text(1));
            TriggerInfo { name: r.text(0), schema: "main".into(), table: table.to_string(), timing, events }
        })
        .collect())
}

// One row per calling form, folded by name. Window-capable aggregates and pure window functions share the
// type 'w', so both count as aggregates. Table-valued functions like json_each are modules and aren't listed.
// SQL can't define functions here, so everything listed ships with the build. FTS5's bm25 and snippet are
// listed as not built in only because FTS5 registers them late. The JSON operators -> and ->> are listed too.
pub(crate) fn functions(src: &mut dyn LiteSource) -> Result<FunctionList, QueryError> {
    let rows = src.query(
        "SELECT name, MAX(type IN ('a', 'w'))
        FROM pragma_function_list
        WHERE name GLOB '[A-Za-z_]*'
        GROUP BY name
        ORDER BY name",
        &[],
    )?;
    let functions = rows
        .rows
        .iter()
        .map(|r| FunctionInfo {
            name: r.text(0),
            builtin: true,
            kind: if r.int(1) == 1 { FunctionKind::Aggregate } else { FunctionKind::Scalar },
            ..Default::default()
        })
        .collect();
    Ok(FunctionList { functions, ..Default::default() })
}

// Generated columns and a virtual table's hidden columns both take no value.
pub(crate) fn dump_columns(src: &mut dyn LiteSource, table: &str) -> Result<Vec<DumpColumn>, QueryError> {
    let rows = src.query("SELECT name, type, hidden FROM pragma_table_xinfo(?1)", &[text(table)])?;
    Ok(rows
        .rows
        .iter()
        .map(|r| DumpColumn { name: r.text(0), data_type: r.text(1), generated: r.int(2) != 0, ..Default::default() })
        .collect())
}

pub(crate) fn object_ddl(src: &mut dyn LiteSource, object: &ObjectRef) -> Result<String, QueryError> {
    match object.kind {
        ObjectKind::Table | ObjectKind::View => relation_ddl(src, &object.kind, &object.name),
        ObjectKind::Trigger => master_ddl(src, "trigger", &object.name),
        ObjectKind::Index => index_ddl(src, object),
        ObjectKind::Constraint => relation_ddl(src, &ObjectKind::Table, &object.table),
        _ => Err(QueryError::message(unsupported_ddl(&LITE, &object.kind))),
    }
}

// NULL sql gives an empty definition, not an error.
fn master_ddl(src: &mut dyn LiteSource, object_type: &str, name: &str) -> Result<String, QueryError> {
    let rows =
        src.query("SELECT sql FROM sqlite_master WHERE type = ?1 AND name = ?2", &[text(object_type), text(name)])?;
    match rows.rows.first() {
        None => Err(QueryError::message(format!("{object_type} {name} not found"))),
        Some(r) => Ok(terminate_statement(&r.text(0))),
    }
}

// SQLite stores standalone indexes as separate statements, so append them to the table's.
fn relation_ddl(src: &mut dyn LiteSource, kind: &ObjectKind, name: &str) -> Result<String, QueryError> {
    let base = master_ddl(src, kind.as_str(), name)?;
    if *kind != ObjectKind::Table {
        return Ok(base);
    }
    let rows = src.query(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL ORDER BY name",
        &[text(name)],
    )?;
    let mut blocks = vec![base];
    blocks.extend(rows.rows.iter().map(|r| terminate_statement(&r.text(0))));
    Ok(join_ddl(&blocks))
}

// Implicit indexes have NULL sql, so synthesize them.
fn index_ddl(src: &mut dyn LiteSource, object: &ObjectRef) -> Result<String, QueryError> {
    let stored = master_ddl(src, "index", &object.name);
    if let Ok(ddl) = &stored
        && !ddl.is_empty()
    {
        return Ok(ddl.clone());
    }
    let Some(entry) = index_entries(src, &object.table)?.into_iter().find(|e| e.name == object.name) else {
        return stored;
    };
    let synth = render_create_index(&LITE, &index_info(entry, &object.table));
    if !synth.is_empty() {
        return Ok(synth);
    }
    Err(QueryError::message(format!("index {} is created implicitly by its constraint", object.name)))
}

static TRIGGER_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)(?-u:\b)CREATE[\t\n\f\r ]+(?:TEMP(?:ORARY)?[\t\n\f\r ]+)?TRIGGER[\t\n\f\r ]+(?:IF[\t\n\f\r ]+NOT[\t\n\f\r ]+EXISTS[\t\n\f\r ]+)?(.*?)[\t\n\f\r ]+ON[\t\n\f\r ]").unwrap()
});

// Parses timing and event from the stored CREATE TRIGGER. SQLite defaults to BEFORE.
pub(crate) fn parse_trigger_head(ddl: &str) -> (String, String) {
    let Some(caps) = TRIGGER_HEAD.captures(ddl) else {
        return (String::new(), String::new());
    };
    let head = barsql_sql::to_upper(&caps[1].split_whitespace().collect::<Vec<_>>().join(" "));
    let timing = if head.contains("INSTEAD OF") {
        "INSTEAD OF"
    } else if head.contains("AFTER") {
        "AFTER"
    } else {
        "BEFORE"
    };
    let event = ["INSERT", "UPDATE", "DELETE"].into_iter().find(|ev| head.contains(ev)).unwrap_or_default();
    (timing.into(), event.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_heads() {
        let cases = [
            ("CREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1; END", ("AFTER", "INSERT")),
            ("create temp trigger if not exists t instead of update of a on v begin end", ("INSTEAD OF", "UPDATE")),
            ("CREATE TRIGGER t DELETE ON x BEGIN END", ("BEFORE", "DELETE")),
            ("not a trigger", ("", "")),
        ];
        for (ddl, (timing, event)) in cases {
            assert_eq!(parse_trigger_head(ddl), (timing.to_string(), event.to_string()), "{ddl}");
        }
    }
}
