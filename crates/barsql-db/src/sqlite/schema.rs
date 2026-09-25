use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use barsql_core::schema::primary_keys;
use barsql_core::{
    ColumnInfo, ConnectionStatus, ConstraintInfo, DriverType, IndexInfo, ObjectKind, ObjectRef, QueryError,
    RoutineInfo, SchemaInfo, TableInfo, TriggerInfo,
};
use barsql_sql::ddl::{join_ddl, render_create_index, terminate_statement, unsupported_ddl};
use barsql_sql::quote_ident;
use regex::Regex;
use rusqlite::{Connection, OptionalExtension};

use super::{SqliteEngine, lite_error};
use crate::dump::DumpColumn;

const LITE: DriverType = DriverType::Sqlite;

fn ident(name: &str) -> String {
    quote_ident(&LITE, name)
}

struct IndexEntry {
    name: String,
    unique: bool,
    // "c" for CREATE INDEX, "u" for a UNIQUE constraint, "pk" for PRIMARY KEY.
    origin: String,
}

fn index_list(conn: &Connection, table: &str) -> Result<Vec<IndexEntry>, QueryError> {
    let mut stmt = conn.prepare(&format!("PRAGMA index_list({})", ident(table))).map_err(|e| lite_error(&e))?;
    let rows = stmt
        .query_map([], |r| Ok(IndexEntry { name: r.get(1)?, unique: r.get::<_, i64>(2)? == 1, origin: r.get(3)? }))
        .map_err(|e| lite_error(&e))?;
    rows.collect::<Result<_, _>>().map_err(|e| lite_error(&e))
}

fn index_columns(conn: &Connection, index: &str) -> Result<Vec<String>, QueryError> {
    let mut stmt = conn.prepare(&format!("PRAGMA index_info({})", ident(index))).map_err(|e| lite_error(&e))?;
    let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(2)).map_err(|e| lite_error(&e))?;
    Ok(rows.filter_map(|r| r.ok().flatten()).collect())
}

struct ForeignKeyRow {
    id: i64,
    ref_table: String,
    from: String,
    // NULL when the key references the target's primary key implicitly.
    to: Option<String>,
}

fn foreign_key_rows(conn: &Connection, table: &str) -> Result<Vec<ForeignKeyRow>, QueryError> {
    let mut stmt = conn.prepare(&format!("PRAGMA foreign_key_list({})", ident(table))).map_err(|e| lite_error(&e))?;
    let rows = stmt
        .query_map([], |r| Ok(ForeignKeyRow { id: r.get(0)?, ref_table: r.get(2)?, from: r.get(3)?, to: r.get(4)? }))
        .map_err(|e| lite_error(&e))?;
    rows.collect::<Result<_, _>>().map_err(|e| lite_error(&e))
}

fn columns(conn: &Connection, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
    let mut fks: HashMap<String, (String, String)> = HashMap::new();
    for fk in foreign_key_rows(conn, table)? {
        fks.entry(fk.from).or_insert((fk.ref_table, fk.to.unwrap_or_default()));
    }
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", ident(table))).map_err(|e| lite_error(&e))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .map_err(|e| lite_error(&e))?;
    let mut out = Vec::new();
    for row in rows {
        let (name, data_type, notnull, default, pk) = row.map_err(|e| lite_error(&e))?;
        let (foreign_table, foreign_column) = fks.get(&name).cloned().unwrap_or_default();
        out.push(ColumnInfo {
            name,
            data_type,
            is_nullable: notnull == 0,
            is_primary: pk > 0,
            is_foreign: !foreign_table.is_empty(),
            foreign_table,
            foreign_column,
            default_val: default.unwrap_or_default(),
        });
    }
    Ok(out)
}

// NULL sql gives an empty definition, not an error.
fn master_ddl(conn: &Connection, object_type: &str, name: &str) -> Result<String, QueryError> {
    let ddl: Option<Option<String>> = conn
        .query_row("SELECT sql FROM sqlite_master WHERE type = ? AND name = ?", [object_type, name], |r| r.get(0))
        .optional()
        .map_err(|e| lite_error(&e))?;
    match ddl {
        None => Err(QueryError::message(format!("{object_type} {name} not found"))),
        Some(sql) => Ok(terminate_statement(&sql.unwrap_or_default())),
    }
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

impl SqliteEngine {
    pub async fn connection_info(self: &Arc<Self>) -> Result<ConnectionStatus, QueryError> {
        Ok(ConnectionStatus { connected: true, database: "main".into(), schema: "main".into(), ..Default::default() })
    }

    pub async fn list_schemas(self: &Arc<Self>) -> Result<Vec<SchemaInfo>, QueryError> {
        Ok(vec![SchemaInfo { name: "main".into() }])
    }

    // Generated columns and a virtual table's hidden columns both take no value.
    pub(crate) async fn dump_columns(self: &Arc<Self>, table: &str) -> Result<Vec<DumpColumn>, QueryError> {
        let table = table.to_string();
        self.with_conn(move |conn| {
            let mut stmt =
                conn.prepare(&format!("PRAGMA table_xinfo({})", ident(&table))).map_err(|e| lite_error(&e))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(DumpColumn {
                        name: r.get(1)?,
                        data_type: r.get(2)?,
                        generated: r.get::<_, i64>(6)? != 0,
                        ..Default::default()
                    })
                })
                .map_err(|e| lite_error(&e))?;
            rows.collect::<Result<_, _>>().map_err(|e| lite_error(&e))
        })
        .await
    }

    pub async fn list_tables(self: &Arc<Self>, _schema: &str) -> Result<Vec<TableInfo>, QueryError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT type, name FROM sqlite_master
                    WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%'
                    ORDER BY name",
                )
                .map_err(|e| lite_error(&e))?;
            let rows = stmt
                .query_map([], |r| Ok(TableInfo { schema: "main".into(), kind: r.get(0)?, name: r.get(1)? }))
                .map_err(|e| lite_error(&e))?;
            rows.collect::<Result<_, _>>().map_err(|e| lite_error(&e))
        })
        .await
    }

    pub async fn list_columns(self: &Arc<Self>, _schema: &str, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
        let table = table.to_string();
        self.with_conn(move |conn| columns(conn, &table)).await
    }

    pub async fn list_indexes(self: &Arc<Self>, _schema: &str, table: &str) -> Result<Vec<IndexInfo>, QueryError> {
        let table = table.to_string();
        self.with_conn(move |conn| {
            index_list(conn, &table)?
                .into_iter()
                .map(|e| {
                    Ok(IndexInfo {
                        columns: index_columns(conn, &e.name)?,
                        name: e.name,
                        schema: "main".into(),
                        table: table.clone(),
                        is_primary: e.origin == "pk",
                        is_unique: e.unique,
                        method: String::new(),
                    })
                })
                .collect()
        })
        .await
    }

    // No pragma lists CHECK constraints, so they only show up in the table's DDL.
    pub async fn list_constraints(
        self: &Arc<Self>,
        _schema: &str,
        table: &str,
    ) -> Result<Vec<ConstraintInfo>, QueryError> {
        let table = table.to_string();
        self.with_conn(move |conn| {
            let mut out = Vec::new();
            let pks = primary_keys(&columns(conn, &table)?);
            if !pks.is_empty() {
                out.push(ConstraintInfo {
                    schema: "main".into(),
                    table: table.clone(),
                    kind: "PRIMARY KEY".into(),
                    columns: pks,
                    ..Default::default()
                });
            }
            for e in index_list(conn, &table)?.into_iter().filter(|e| e.origin == "u") {
                out.push(ConstraintInfo {
                    columns: index_columns(conn, &e.name)?,
                    name: e.name,
                    schema: "main".into(),
                    table: table.clone(),
                    kind: "UNIQUE".into(),
                    ..Default::default()
                });
            }
            let mut fks: Vec<(i64, ConstraintInfo)> = Vec::new();
            for fk in foreign_key_rows(conn, &table)? {
                let entry = match fks.iter().position(|(id, _)| *id == fk.id) {
                    Some(ix) => &mut fks[ix].1,
                    None => {
                        fks.push((
                            fk.id,
                            ConstraintInfo {
                                schema: "main".into(),
                                table: table.clone(),
                                kind: "FOREIGN KEY".into(),
                                ref_table: fk.ref_table.clone(),
                                ..Default::default()
                            },
                        ));
                        &mut fks.last_mut().expect("pushed").1
                    }
                };
                entry.columns.push(fk.from);
                if let Some(to) = fk.to {
                    entry.ref_columns.push(to);
                }
            }
            out.extend(fks.into_iter().map(|(_, c)| c));
            Ok(out)
        })
        .await
    }

    pub async fn list_triggers(self: &Arc<Self>, _schema: &str, table: &str) -> Result<Vec<TriggerInfo>, QueryError> {
        let table = table.to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare("SELECT name, COALESCE(sql, '') FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ? ORDER BY name")
                .map_err(|e| lite_error(&e))?;
            let rows = stmt
                .query_map([&table], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| lite_error(&e))?;
            let mut out = Vec::new();
            for row in rows {
                let (name, ddl) = row.map_err(|e| lite_error(&e))?;
                let (timing, events) = parse_trigger_head(&ddl);
                out.push(TriggerInfo { name, schema: "main".into(), table: table.clone(), timing, events });
            }
            Ok(out)
        })
        .await
    }

    // SQLite has no routines.
    pub async fn list_routines(self: &Arc<Self>, _schema: &str) -> Result<Vec<RoutineInfo>, QueryError> {
        Ok(Vec::new())
    }

    pub async fn object_ddl(self: &Arc<Self>, object: &ObjectRef) -> Result<String, QueryError> {
        let object = object.clone();
        self.with_conn(move |conn| match object.kind {
            ObjectKind::Table | ObjectKind::View => relation_ddl(conn, &object.kind, &object.name),
            ObjectKind::Trigger => master_ddl(conn, "trigger", &object.name),
            ObjectKind::Index => index_ddl(conn, &object),
            ObjectKind::Constraint => relation_ddl(conn, &ObjectKind::Table, &object.table),
            _ => Err(QueryError::message(unsupported_ddl(&LITE, &object.kind))),
        })
        .await
    }

    pub async fn primary_keys(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
    ) -> Result<(Vec<ColumnInfo>, Vec<String>), QueryError> {
        let cols = self.list_columns(schema, table).await?;
        let pks = primary_keys(&cols);
        Ok((cols, pks))
    }
}

// SQLite stores standalone indexes as separate statements, so append them to the table's.
fn relation_ddl(conn: &Connection, kind: &ObjectKind, name: &str) -> Result<String, QueryError> {
    let base = master_ddl(conn, kind.as_str(), name)?;
    if *kind != ObjectKind::Table {
        return Ok(base);
    }
    let mut stmt = conn
        .prepare(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND tbl_name = ? AND sql IS NOT NULL ORDER BY name",
        )
        .map_err(|e| lite_error(&e))?;
    let rows = stmt.query_map([name], |r| r.get::<_, String>(0)).map_err(|e| lite_error(&e))?;
    let mut blocks = vec![base];
    for ddl in rows {
        blocks.push(terminate_statement(&ddl.map_err(|e| lite_error(&e))?));
    }
    Ok(join_ddl(&blocks))
}

// Implicit indexes have NULL sql, so synthesize them.
fn index_ddl(conn: &Connection, object: &ObjectRef) -> Result<String, QueryError> {
    let stored = master_ddl(conn, "index", &object.name);
    if let Ok(ddl) = &stored
        && !ddl.is_empty()
    {
        return Ok(ddl.clone());
    }
    for e in index_list(conn, &object.table)? {
        if e.name != object.name {
            continue;
        }
        let idx = IndexInfo {
            columns: index_columns(conn, &e.name)?,
            name: e.name,
            schema: "main".into(),
            table: object.table.clone(),
            is_primary: e.origin == "pk",
            is_unique: e.unique,
            method: String::new(),
        };
        let synth = render_create_index(&LITE, &idx);
        if !synth.is_empty() {
            return Ok(synth);
        }
        return Err(QueryError::message(format!("index {} is created implicitly by its constraint", object.name)));
    }
    stored
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
