use std::collections::HashSet;
use std::sync::Arc;

use barsql_core::schema::primary_keys;
use barsql_core::{
    ColumnInfo, ConnectionStatus, ConstraintInfo, DriverType, IndexInfo, ObjectKind, ObjectRef, QueryError,
    RoutineInfo, SchemaInfo, TableInfo, TriggerInfo,
};
use barsql_sql::ddl::{
    DdlColumn, compose_create_table, join_ddl, render_constraint, render_create_index, terminate_statement,
    unsupported_ddl,
};
use barsql_sql::{qualified_table, quote_ident, quote_literal};
use tokio_postgres::Row;
use tokio_postgres::types::ToSql;

use super::{PgEngine, pg_error};
use crate::dump::DumpColumn;

const PG: DriverType = DriverType::Postgres;

impl PgEngine {
    async fn rows(self: &Arc<Self>, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, QueryError> {
        let lease = self.lease().await?;
        lease.client().query(sql, params).await.map_err(|err| pg_error(&err))
    }

    pub async fn connection_info(self: &Arc<Self>) -> Result<ConnectionStatus, QueryError> {
        let rows =
            self.rows("SELECT current_database()::text, current_schema()::text, current_user::text", &[]).await?;
        let row = rows.first().ok_or_else(|| QueryError::message("no rows in result set"))?;
        Ok(ConnectionStatus {
            connected: true,
            database: row.get::<_, Option<String>>(0).unwrap_or_default(),
            schema: row.get::<_, Option<String>>(1).unwrap_or_default(),
            user: row.get::<_, Option<String>>(2).unwrap_or_default(),
            host: self.options.host.clone(),
        })
    }

    pub async fn list_databases(self: &Arc<Self>) -> Result<Vec<String>, QueryError> {
        let rows = self
            .rows(
                "SELECT datname::text FROM pg_catalog.pg_database
                WHERE datallowconn AND NOT datistemplate
                ORDER BY datname",
                &[],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub(crate) async fn dump_columns(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
    ) -> Result<Vec<DumpColumn>, QueryError> {
        let rows = self
            .rows(
                "SELECT a.attname::text,
                    a.attgenerated <> '',
                    a.attidentity = 'a',
                    pg_catalog.pg_get_serial_sequence(pg_catalog.format('%I.%I', n.nspname, c.relname), a.attname) IS NOT NULL
                FROM pg_catalog.pg_attribute a
                JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
                JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped
                ORDER BY a.attnum",
                &[&schema, &table],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| DumpColumn {
                name: r.get(0),
                generated: r.get(1),
                identity_always: r.get(2),
                serial: r.get(3),
                ..Default::default()
            })
            .collect())
    }

    // Fully qualified to pg_catalog, so search_path is irrelevant here.
    pub async fn list_schemas(self: &Arc<Self>) -> Result<Vec<SchemaInfo>, QueryError> {
        let rows = self
            .rows(
                "SELECT nspname FROM pg_catalog.pg_namespace
                WHERE nspname NOT LIKE 'pg_%'
                  AND nspname NOT IN ('information_schema')
                ORDER BY CASE WHEN nspname = 'public' THEN 0 ELSE 1 END, nspname",
                &[],
            )
            .await?;
        Ok(rows.iter().map(|r| SchemaInfo { name: r.get(0) }).collect())
    }

    pub async fn list_tables(self: &Arc<Self>, schema: &str) -> Result<Vec<TableInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT n.nspname, c.relname,
                    CASE c.relkind
                        WHEN 'r' THEN 'table'
                        WHEN 'v' THEN 'view'
                        WHEN 'm' THEN 'materialized view'
                        WHEN 'f' THEN 'foreign table'
                        WHEN 'p' THEN 'partitioned table'
                        ELSE 'table'
                    END
                FROM pg_catalog.pg_class c
                JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                WHERE n.nspname = $1
                  AND c.relkind IN ('r', 'v', 'm', 'f', 'p')
                ORDER BY c.relname",
                &[&schema],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| TableInfo { schema: r.get(0), name: r.get(1), kind: r.get::<_, String>(2).to_lowercase() })
            .collect())
    }

    pub async fn list_columns(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT a.attname,
                    pg_catalog.format_type(a.atttypid, a.atttypmod),
                    NOT a.attnotnull,
                    COALESCE(pg_catalog.pg_get_expr(ad.adbin, ad.adrelid), ''),
                    EXISTS (
                        SELECT 1 FROM pg_catalog.pg_constraint con
                        WHERE con.conrelid = c.oid AND con.contype = 'p'
                          AND a.attnum = ANY (con.conkey)
                    ),
                    COALESCE(fk.reftable::text, ''),
                    COALESCE(fk.refcolumn::text, '')
                FROM pg_catalog.pg_attribute a
                JOIN pg_catalog.pg_class c ON a.attrelid = c.oid
                JOIN pg_catalog.pg_namespace n ON c.relnamespace = n.oid
                LEFT JOIN pg_catalog.pg_attrdef ad ON a.attrelid = ad.adrelid AND a.attnum = ad.adnum
                LEFT JOIN LATERAL (
                    SELECT ref.relname AS reftable, refatt.attname AS refcolumn
                    FROM pg_catalog.pg_constraint con
                    JOIN pg_catalog.pg_class ref ON ref.oid = con.confrelid
                    JOIN pg_catalog.pg_attribute refatt ON refatt.attrelid = con.confrelid
                        AND refatt.attnum = con.confkey[array_position(con.conkey, a.attnum)]
                    WHERE con.conrelid = c.oid AND con.contype = 'f'
                      AND a.attnum = ANY (con.conkey)
                    LIMIT 1
                ) fk ON true
                WHERE n.nspname = $1 AND c.relname = $2
                  AND a.attnum > 0 AND NOT a.attisdropped
                ORDER BY a.attnum",
                &[&schema, &table],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let foreign_table: String = r.get(5);
                ColumnInfo {
                    name: r.get(0),
                    data_type: r.get::<_, Option<String>>(1).unwrap_or_default(),
                    is_nullable: r.get(2),
                    default_val: r.get(3),
                    is_primary: r.get(4),
                    is_foreign: !foreign_table.is_empty(),
                    foreign_table,
                    foreign_column: r.get(6),
                }
            })
            .collect())
    }

    pub async fn list_indexes(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<IndexInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT i.relname, ix.indisunique, ix.indisprimary, am.amname, a.attname
                FROM pg_catalog.pg_index ix
                JOIN pg_catalog.pg_class i ON i.oid = ix.indexrelid
                JOIN pg_catalog.pg_class t ON t.oid = ix.indrelid
                JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace
                JOIN pg_catalog.pg_am am ON am.oid = i.relam
                LEFT JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord) ON true
                LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
                WHERE n.nspname = $1 AND t.relname = $2
                ORDER BY i.relname, k.ord",
                &[&schema, &table],
            )
            .await?;
        let mut out: Vec<IndexInfo> = Vec::new();
        for r in &rows {
            let name: String = r.get(0);
            if out.last().is_none_or(|idx| idx.name != name) && !out.iter().any(|idx| idx.name == name) {
                out.push(IndexInfo {
                    name: name.clone(),
                    schema: schema.clone(),
                    table: table.to_string(),
                    is_unique: r.get(1),
                    is_primary: r.get(2),
                    method: r.get(3),
                    columns: Vec::new(),
                });
            }
            let column: Option<String> = r.get(4);
            if let (Some(column), Some(idx)) = (column, out.iter_mut().find(|idx| idx.name == name)) {
                idx.columns.push(column);
            }
        }
        Ok(out)
    }

    pub async fn list_constraints(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ConstraintInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT con.conname, con.contype::text,
                    pg_catalog.pg_get_constraintdef(con.oid, true),
                    COALESCE(reft.relname::text, ''),
                    a.attname, refa.attname
                FROM pg_catalog.pg_constraint con
                JOIN pg_catalog.pg_class t ON t.oid = con.conrelid
                JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace
                LEFT JOIN pg_catalog.pg_class reft ON reft.oid = con.confrelid
                LEFT JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord) ON true
                LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum
                LEFT JOIN pg_catalog.pg_attribute refa ON refa.attrelid = con.confrelid
                    AND refa.attnum = con.confkey[k.ord]
                WHERE n.nspname = $1 AND t.relname = $2
                ORDER BY con.contype, con.conname, k.ord",
                &[&schema, &table],
            )
            .await?;
        let mut out: Vec<ConstraintInfo> = Vec::new();
        for r in &rows {
            let name: String = r.get(0);
            if !out.iter().any(|c| c.name == name) {
                out.push(ConstraintInfo {
                    name: name.clone(),
                    schema: schema.clone(),
                    table: table.to_string(),
                    kind: constraint_type_name(&r.get::<_, String>(1)),
                    definition: r.get::<_, Option<String>>(2).unwrap_or_default(),
                    ref_table: r.get(3),
                    ..Default::default()
                });
            }
            let c = out.iter_mut().find(|c| c.name == name).expect("constraint just added");
            if let Some(column) = r.get::<_, Option<String>>(4) {
                c.columns.push(column);
            }
            if let Some(column) = r.get::<_, Option<String>>(5) {
                c.ref_columns.push(column);
            }
        }
        Ok(out)
    }

    pub async fn list_triggers(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<TriggerInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        // tgisinternal hides the triggers Postgres creates to enforce foreign keys.
        let rows = self
            .rows(
                "SELECT tg.tgname, tg.tgtype::int
                FROM pg_catalog.pg_trigger tg
                JOIN pg_catalog.pg_class t ON t.oid = tg.tgrelid
                JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace
                WHERE n.nspname = $1 AND t.relname = $2 AND NOT tg.tgisinternal
                ORDER BY tg.tgname",
                &[&schema, &table],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let (timing, events) = decode_trigger_type(r.get(1));
                TriggerInfo { name: r.get(0), schema: schema.clone(), table: table.to_string(), timing, events }
            })
            .collect())
    }

    pub async fn list_routines(self: &Arc<Self>, schema: &str) -> Result<Vec<RoutineInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT p.proname,
                    p.prokind::text,
                    COALESCE(pg_catalog.pg_get_function_result(p.oid), ''),
                    COALESCE(pg_catalog.pg_get_function_identity_arguments(p.oid), '')
                FROM pg_catalog.pg_proc p
                JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
                WHERE n.nspname = $1 AND p.prokind IN ('f', 'p')
                ORDER BY p.proname, 4",
                &[&schema],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let procedure = r.get::<_, String>(1) == "p";
                RoutineInfo {
                    name: r.get(0),
                    schema: schema.clone(),
                    kind: if procedure { ObjectKind::Procedure } else { ObjectKind::Function },
                    return_type: if procedure { String::new() } else { r.get(2) },
                    args: r.get(3),
                }
            })
            .collect())
    }

    pub async fn object_ddl(self: &Arc<Self>, object: &ObjectRef) -> Result<String, QueryError> {
        let schema = self.schema_or(&object.schema).to_string();
        match object.kind {
            ObjectKind::Table => Ok(self.table_ddl(&schema, &object.name, false).await?.0),
            ObjectKind::View | ObjectKind::MaterializedView => {
                let def = self
                    .scalar_ddl(
                        "SELECT pg_catalog.pg_get_viewdef(c.oid, true)
                        FROM pg_catalog.pg_class c
                        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                        WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('v', 'm')",
                        "view",
                        &object.name,
                        &[&schema, &object.name],
                    )
                    .await?;
                // A materialized view has no OR REPLACE form.
                let head = if object.kind == ObjectKind::MaterializedView {
                    "CREATE MATERIALIZED VIEW "
                } else {
                    "CREATE OR REPLACE VIEW "
                };
                Ok(format!("{head}{} AS\n{def}", qualified_table(&PG, &schema, &object.name)))
            }
            ObjectKind::Index => {
                self.scalar_ddl(
                    "SELECT pg_catalog.pg_get_indexdef(i.oid)
                    FROM pg_catalog.pg_class i
                    JOIN pg_catalog.pg_namespace n ON n.oid = i.relnamespace
                    WHERE n.nspname = $1 AND i.relname = $2 AND i.relkind IN ('i', 'I')",
                    "index",
                    &object.name,
                    &[&schema, &object.name],
                )
                .await
            }
            ObjectKind::Trigger => {
                self.scalar_ddl(
                    "SELECT pg_catalog.pg_get_triggerdef(tg.oid, true)
                    FROM pg_catalog.pg_trigger tg
                    JOIN pg_catalog.pg_class t ON t.oid = tg.tgrelid
                    JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace
                    WHERE n.nspname = $1 AND t.relname = $2 AND tg.tgname = $3",
                    "trigger",
                    &object.name,
                    &[&schema, &object.table, &object.name],
                )
                .await
            }
            ObjectKind::Constraint => {
                let def = self
                    .scalar_ddl(
                        "SELECT pg_catalog.pg_get_constraintdef(con.oid, true)
                        FROM pg_catalog.pg_constraint con
                        JOIN pg_catalog.pg_class t ON t.oid = con.conrelid
                        JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace
                        WHERE n.nspname = $1 AND t.relname = $2 AND con.conname = $3",
                        "constraint",
                        &object.name,
                        &[&schema, &object.table, &object.name],
                    )
                    .await?;
                Ok(format!(
                    "ALTER TABLE ONLY {}\n    ADD CONSTRAINT {} {};",
                    qualified_table(&PG, &schema, &object.table),
                    quote_ident(&PG, &object.name),
                    def.strip_suffix(';').unwrap_or(&def)
                ))
            }
            ObjectKind::Function | ObjectKind::Procedure => {
                self.scalar_ddl(
                    "SELECT pg_catalog.pg_get_functiondef(p.oid)
                    FROM pg_catalog.pg_proc p
                    JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
                    WHERE n.nspname = $1 AND p.proname = $2
                      AND ($3 = '' OR pg_catalog.pg_get_function_identity_arguments(p.oid) = $3)
                    ORDER BY p.oid
                    LIMIT 1",
                    object.kind.as_str(),
                    &object.name,
                    &[&schema, &object.name, &object.args],
                )
                .await
            }
            _ => Err(QueryError::message(unsupported_ddl(&PG, &object.kind))),
        }
    }

    // Postgres has no SHOW CREATE TABLE, so build it from the catalog. `split_foreign_keys` returns the FKs
    // separately so a backup can add them after rows that may reference each other in any order.
    pub(crate) async fn table_ddl(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
        split_foreign_keys: bool,
    ) -> Result<(String, Vec<String>), QueryError> {
        let cols = self.ddl_columns(schema, table).await?;
        if cols.is_empty() {
            return Err(QueryError::message(format!("table {table} not found")));
        }
        let constraints = self.list_constraints(schema, table).await?;
        let constrained: HashSet<&str> = constraints.iter().map(|c| c.name.as_str()).collect();
        let (foreign, inline): (Vec<_>, Vec<_>) =
            constraints.iter().partition(|c| split_foreign_keys && c.kind.eq_ignore_ascii_case("FOREIGN KEY"));
        let clauses: Vec<String> = inline.iter().map(|c| render_constraint(&PG, c)).filter(|c| !c.is_empty()).collect();
        let foreign_keys = foreign
            .iter()
            .map(|c| render_constraint(&PG, c))
            .filter(|c| !c.is_empty())
            .map(|c| format!("ALTER TABLE {} ADD {c};", qualified_table(&PG, schema, table)))
            .collect();
        let mut blocks = vec![compose_create_table(&PG, schema, table, &cols, &clauses)];
        for idx in self.list_indexes(schema, table).await? {
            // Shares the constraint name, so the clause above already covers it.
            if constrained.contains(idx.name.as_str()) {
                continue;
            }
            let synth = render_create_index(&PG, &idx);
            if !synth.is_empty() {
                blocks.push(synth);
            }
        }
        blocks.push(self.table_comments(schema, table).await?);
        Ok((join_ddl(&blocks), foreign_keys))
    }

    async fn ddl_columns(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<DdlColumn>, QueryError> {
        // COLLATE only when it differs from the type's default.
        let rows = self
            .rows(
                "SELECT a.attname,
                    pg_catalog.format_type(a.atttypid, a.atttypmod),
                    a.attnotnull,
                    COALESCE(pg_catalog.pg_get_expr(ad.adbin, ad.adrelid), ''),
                    a.attidentity::text,
                    a.attgenerated::text,
                    COALESCE(co.collname::text, ''),
                    pg_catalog.pg_get_serial_sequence(
                        pg_catalog.quote_ident(n.nspname) || '.' || pg_catalog.quote_ident(c.relname),
                        a.attname) IS NOT NULL
                FROM pg_catalog.pg_attribute a
                JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
                JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                JOIN pg_catalog.pg_type ty ON ty.oid = a.atttypid
                LEFT JOIN pg_catalog.pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum
                LEFT JOIN pg_catalog.pg_collation co ON co.oid = a.attcollation
                    AND a.attcollation <> ty.typcollation
                WHERE n.nspname = $1 AND c.relname = $2
                  AND a.attnum > 0 AND NOT a.attisdropped
                ORDER BY a.attnum",
                &[&schema, &table],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let data_type: String = r.get::<_, Option<String>>(1).unwrap_or_default();
                let default_expr: String = r.get(3);
                let identity: String = r.get::<_, Option<String>>(4).unwrap_or_default();
                let generated: String = r.get::<_, Option<String>>(5).unwrap_or_default();
                let is_serial: bool = r.get(7);
                let mut col = DdlColumn {
                    name: r.get(0),
                    data_type: data_type.clone(),
                    not_null: r.get(2),
                    collation: r.get(6),
                    ..Default::default()
                };
                match (generated.as_str(), identity.as_str()) {
                    // A generated column's expression sits in pg_attrdef too, but it isn't a DEFAULT.
                    ("s", _) => col.generated = default_expr,
                    (_, "a") => col.identity = "ALWAYS".into(),
                    (_, "d") => col.identity = "BY DEFAULT".into(),
                    // The sequence is dropped with the table, so a nextval() default would not replay.
                    _ if is_serial && serial_type_for(&data_type).is_some() => {
                        col.data_type = serial_type_for(&data_type).unwrap_or_default().into();
                    }
                    _ => col.default = default_expr,
                }
                col
            })
            .collect())
    }

    async fn table_comments(self: &Arc<Self>, schema: &str, table: &str) -> Result<String, QueryError> {
        let qualified = qualified_table(&PG, schema, table);
        let rows = self
            .rows(
                "SELECT COALESCE(a.attname::text, ''), d.description
                FROM pg_catalog.pg_class c
                JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                JOIN pg_catalog.pg_description d ON d.objoid = c.oid
                LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum = d.objsubid
                    AND NOT a.attisdropped
                WHERE n.nspname = $1 AND c.relname = $2
                  AND (d.objsubid = 0 OR a.attname IS NOT NULL)
                ORDER BY d.objsubid",
                &[&schema, &table],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let column: String = r.get(0);
                let target = if column.is_empty() {
                    format!("TABLE {qualified}")
                } else {
                    format!("COLUMN {qualified}.{}", quote_ident(&PG, &column))
                };
                format!("COMMENT ON {target} IS {};", quote_literal(&r.get::<_, String>(1)))
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    async fn scalar_ddl(
        self: &Arc<Self>,
        sql: &str,
        kind: &str,
        name: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<String, QueryError> {
        let rows = self.rows(sql, params).await?;
        match rows.first() {
            None => Err(QueryError::message(format!("{kind} {name} not found"))),
            Some(row) => Ok(terminate_statement(&row.get::<_, Option<String>>(0).unwrap_or_default())),
        }
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

fn constraint_type_name(contype: &str) -> String {
    match contype {
        "p" => "PRIMARY KEY".into(),
        "f" => "FOREIGN KEY".into(),
        "u" => "UNIQUE".into(),
        "c" => "CHECK".into(),
        "x" => "EXCLUDE".into(),
        other => other.to_uppercase(),
    }
}

// pg_trigger.tgtype bit flags, see TRIGGER_TYPE_* in the Postgres source.
fn decode_trigger_type(tgtype: i32) -> (String, String) {
    let timing = if tgtype & (1 << 6) != 0 {
        "INSTEAD OF"
    } else if tgtype & (1 << 1) != 0 {
        "BEFORE"
    } else {
        "AFTER"
    };
    let events: Vec<&str> = [(1 << 2, "INSERT"), (1 << 4, "UPDATE"), (1 << 3, "DELETE"), (1 << 5, "TRUNCATE")]
        .into_iter()
        .filter(|(bit, _)| tgtype & bit != 0)
        .map(|(_, name)| name)
        .collect();
    (timing.into(), events.join(", "))
}

fn serial_type_for(data_type: &str) -> Option<&'static str> {
    match data_type {
        "smallint" => Some("smallserial"),
        "integer" => Some("serial"),
        "bigint" => Some("bigserial"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_trigger_types() {
        assert_eq!(decode_trigger_type(0b0000_0110), ("BEFORE".into(), "INSERT".into()));
        assert_eq!(decode_trigger_type((1 << 6) | (1 << 4) | (1 << 3)), ("INSTEAD OF".into(), "UPDATE, DELETE".into()));
        assert_eq!(decode_trigger_type(1 << 5), ("AFTER".into(), "TRUNCATE".into()));
        assert_eq!(decode_trigger_type(16), ("AFTER".into(), "UPDATE".into()));
        assert_eq!(decode_trigger_type((1 << 6) | (1 << 1) | (1 << 3)), ("INSTEAD OF".into(), "DELETE".into()));
        assert_eq!(decode_trigger_type(30), ("BEFORE".into(), "INSERT, UPDATE, DELETE".into()));
        assert_eq!(decode_trigger_type(2), ("BEFORE".into(), String::new()));
        assert_eq!(decode_trigger_type(7), ("BEFORE".into(), "INSERT".into()));
        assert_eq!(constraint_type_name("x"), "EXCLUDE");
        assert_eq!(constraint_type_name("t"), "T");
    }

    #[test]
    fn integer_columns_map_to_serial_types() {
        assert_eq!(serial_type_for("smallint"), Some("smallserial"));
        assert_eq!(serial_type_for("integer"), Some("serial"));
        assert_eq!(serial_type_for("bigint"), Some("bigserial"));
        for other in ["text", "numeric", ""] {
            assert_eq!(serial_type_for(other), None, "{other}");
        }
    }
}
