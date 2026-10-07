use std::collections::HashMap;
use std::sync::Arc;

use barsql_core::{
    ColumnInfo, ConnectionStatus, ConstraintInfo, DriverType, FunctionInfo, FunctionList, FunctionSignature, IndexInfo,
    ObjectKind, ObjectRef, QueryError, RoutineInfo, SchemaInfo, TableInfo, TriggerInfo,
};
use barsql_sql::ddl::{render_constraint, render_create_index, terminate_statement, unsupported_ddl};
use barsql_sql::{qualified_table, quote_ident_list};
use mysql_async::prelude::Queryable;
use mysql_async::{Params, Row, Value as MyValue};

use super::{MyEngine, my_error};
use crate::dump::DumpColumn;

const MY: DriverType = DriverType::MySql;
const SYSTEM_SCHEMAS: &[&str] = &["information_schema", "performance_schema", "mysql", "sys"];

fn text(row: &Row, i: usize) -> Option<String> {
    match row.as_ref(i)? {
        MyValue::NULL => None,
        MyValue::Bytes(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        MyValue::Int(v) => Some(v.to_string()),
        MyValue::UInt(v) => Some(v.to_string()),
        other => Some(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

fn string(row: &Row, i: usize) -> String {
    text(row, i).unwrap_or_default()
}

impl MyEngine {
    async fn rows(self: &Arc<Self>, sql: &str, params: Vec<MyValue>) -> Result<Vec<Row>, QueryError> {
        let mut lease = self.lease().await?;
        let params = if params.is_empty() { Params::Empty } else { Params::Positional(params) };
        lease.conn.exec(sql, params).await.map_err(my_error)
    }

    async fn text_rows(self: &Arc<Self>, sql: &str) -> Result<(Vec<String>, Vec<Row>), QueryError> {
        let mut lease = self.lease().await?;
        let mut result = lease.conn.query_iter(sql).await.map_err(my_error)?;
        let columns =
            result.columns().map(|cols| cols.iter().map(|c| c.name_str().into_owned()).collect()).unwrap_or_default();
        let rows = result.collect::<Row>().await.map_err(my_error)?;
        Ok((columns, rows))
    }

    pub async fn connection_info(self: &Arc<Self>) -> Result<ConnectionStatus, QueryError> {
        let (_, rows) = self.text_rows("SELECT DATABASE(), CURRENT_USER()").await?;
        let row = rows.first().ok_or_else(|| QueryError::message("no rows in result set"))?;
        let database = text(row, 0).ok_or_else(|| QueryError::message("no database selected"))?;
        let schema = if self.options.schema.is_empty() { database.clone() } else { self.options.schema.clone() };
        Ok(ConnectionStatus {
            connected: true,
            database,
            schema,
            user: string(row, 1),
            host: self.options.host.clone(),
        })
    }

    pub async fn list_schemas(self: &Arc<Self>) -> Result<Vec<SchemaInfo>, QueryError> {
        let (_, rows) = self.text_rows("SHOW DATABASES").await?;
        Ok(rows
            .iter()
            .map(|r| string(r, 0))
            .filter(|name| !SYSTEM_SCHEMAS.contains(&name.to_lowercase().as_str()))
            .map(|name| SchemaInfo { name })
            .collect())
    }

    pub(crate) async fn dump_columns(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
    ) -> Result<Vec<DumpColumn>, QueryError> {
        let rows = self
            .rows(
                "SELECT COLUMN_NAME, DATA_TYPE, COALESCE(GENERATION_EXPRESSION, '')
                FROM information_schema.COLUMNS
                WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
                ORDER BY ORDINAL_POSITION",
                vec![schema.into(), table.into()],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| DumpColumn {
                name: string(r, 0),
                data_type: string(r, 1),
                generated: !string(r, 2).is_empty(),
                ..Default::default()
            })
            .collect())
    }

    pub async fn list_tables(self: &Arc<Self>, schema: &str) -> Result<Vec<TableInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT TABLE_NAME,
                    CASE TABLE_TYPE
                        WHEN 'BASE TABLE' THEN 'table'
                        WHEN 'VIEW' THEN 'view'
                        ELSE LOWER(TABLE_TYPE)
                    END
                FROM information_schema.TABLES
                WHERE TABLE_SCHEMA = ?
                  AND TABLE_TYPE IN ('BASE TABLE', 'VIEW')
                ORDER BY TABLE_NAME",
                vec![schema.clone().into()],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| TableInfo { schema: schema.clone(), name: string(r, 0), kind: string(r, 1).to_lowercase() })
            .collect())
    }

    pub async fn list_columns(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT c.COLUMN_NAME, c.DATA_TYPE, c.IS_NULLABLE, COALESCE(c.COLUMN_DEFAULT, ''), c.COLUMN_KEY,
                    COALESCE((
                        SELECT k.REFERENCED_TABLE_NAME FROM information_schema.KEY_COLUMN_USAGE k
                        WHERE k.TABLE_SCHEMA = c.TABLE_SCHEMA
                          AND k.TABLE_NAME = c.TABLE_NAME
                          AND k.COLUMN_NAME = c.COLUMN_NAME
                          AND k.REFERENCED_TABLE_NAME IS NOT NULL
                        LIMIT 1
                    ), ''),
                    COALESCE((
                        SELECT k.REFERENCED_COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE k
                        WHERE k.TABLE_SCHEMA = c.TABLE_SCHEMA
                          AND k.TABLE_NAME = c.TABLE_NAME
                          AND k.COLUMN_NAME = c.COLUMN_NAME
                          AND k.REFERENCED_TABLE_NAME IS NOT NULL
                        LIMIT 1
                    ), ''),
                    c.EXTRA
                FROM information_schema.COLUMNS c
                WHERE c.TABLE_SCHEMA = ? AND c.TABLE_NAME = ?
                ORDER BY c.ORDINAL_POSITION",
                vec![schema.into(), table.into()],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let foreign_table = string(r, 5);
                // auto_increment, or VIRTUAL/STORED/PERSISTENT GENERATED. MySQL also tags an expression default
                // DEFAULT_GENERATED, and that column still takes values.
                let extra = string(r, 7).to_lowercase().replace("default_generated", "");
                ColumnInfo {
                    name: string(r, 0),
                    data_type: string(r, 1),
                    is_nullable: string(r, 2).eq_ignore_ascii_case("YES"),
                    default_val: string(r, 3),
                    is_primary: string(r, 4) == "PRI",
                    is_foreign: !foreign_table.is_empty(),
                    foreign_table,
                    foreign_column: string(r, 6),
                    is_identity: extra.contains("auto_increment"),
                    is_computed: extra.contains("generated"),
                }
            })
            .collect())
    }

    pub async fn list_indexes(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<IndexInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT INDEX_NAME, NON_UNIQUE, INDEX_TYPE, COLUMN_NAME
                FROM information_schema.STATISTICS
                WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
                ORDER BY INDEX_NAME, SEQ_IN_INDEX",
                vec![schema.clone().into(), table.into()],
            )
            .await?;
        let mut out: Vec<IndexInfo> = Vec::new();
        for r in &rows {
            let name = string(r, 0);
            if !out.iter().any(|idx| idx.name == name) {
                out.push(IndexInfo {
                    is_primary: name == "PRIMARY",
                    is_unique: string(r, 1) == "0",
                    method: string(r, 2).to_lowercase(),
                    name: name.clone(),
                    schema: schema.clone(),
                    table: table.to_string(),
                    columns: Vec::new(),
                });
            }
            if let (Some(column), Some(idx)) = (text(r, 3), out.iter_mut().find(|idx| idx.name == name)) {
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
                "SELECT tc.CONSTRAINT_NAME, tc.CONSTRAINT_TYPE,
                    kcu.COLUMN_NAME, kcu.REFERENCED_TABLE_NAME, kcu.REFERENCED_COLUMN_NAME
                FROM information_schema.TABLE_CONSTRAINTS tc
                LEFT JOIN information_schema.KEY_COLUMN_USAGE kcu
                    ON kcu.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA
                    AND kcu.CONSTRAINT_NAME = tc.CONSTRAINT_NAME
                    AND kcu.TABLE_NAME = tc.TABLE_NAME
                WHERE tc.TABLE_SCHEMA = ? AND tc.TABLE_NAME = ?
                ORDER BY tc.CONSTRAINT_TYPE, tc.CONSTRAINT_NAME, kcu.ORDINAL_POSITION",
                vec![schema.clone().into(), table.into()],
            )
            .await?;
        let mut out: Vec<ConstraintInfo> = Vec::new();
        for r in &rows {
            let name = string(r, 0);
            if !out.iter().any(|c| c.name == name) {
                out.push(ConstraintInfo {
                    name: name.clone(),
                    schema: schema.clone(),
                    table: table.to_string(),
                    kind: string(r, 1).to_uppercase(),
                    ref_table: string(r, 3),
                    ..Default::default()
                });
            }
            let c = out.iter_mut().find(|c| c.name == name).expect("constraint just added");
            if let Some(column) = text(r, 2) {
                c.columns.push(column);
            }
            if let Some(column) = text(r, 4) {
                c.ref_columns.push(column);
            }
        }
        let clauses = self.check_clauses(&schema).await;
        for c in out.iter_mut().filter(|c| c.kind == "CHECK") {
            if let Some(clause) = clauses.get(&c.name) {
                c.definition = format!("CHECK {clause}");
            }
        }
        Ok(out)
    }

    // CHECK_CONSTRAINTS only exists on MySQL 8.0.16+ and MariaDB 10.2.22+, so a failure means no clauses.
    async fn check_clauses(self: &Arc<Self>, schema: &str) -> HashMap<String, String> {
        let rows = self
            .rows(
                "SELECT CONSTRAINT_NAME, CHECK_CLAUSE
                FROM information_schema.CHECK_CONSTRAINTS
                WHERE CONSTRAINT_SCHEMA = ?",
                vec![schema.into()],
            )
            .await
            .unwrap_or_default();
        rows.iter().map(|r| (string(r, 0), string(r, 1))).collect()
    }

    pub async fn list_triggers(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<TriggerInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT TRIGGER_NAME, ACTION_TIMING, EVENT_MANIPULATION
                FROM information_schema.TRIGGERS
                WHERE TRIGGER_SCHEMA = ? AND EVENT_OBJECT_TABLE = ?
                ORDER BY TRIGGER_NAME",
                vec![schema.clone().into(), table.into()],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| TriggerInfo {
                name: string(r, 0),
                schema: schema.clone(),
                table: table.to_string(),
                timing: string(r, 1).to_uppercase(),
                events: string(r, 2).to_uppercase(),
            })
            .collect())
    }

    // Args stay empty because MySQL has no overloading.
    pub async fn list_routines(self: &Arc<Self>, schema: &str) -> Result<Vec<RoutineInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT ROUTINE_NAME, ROUTINE_TYPE, COALESCE(DTD_IDENTIFIER, '')
                FROM information_schema.ROUTINES
                WHERE ROUTINE_SCHEMA = ?
                ORDER BY ROUTINE_TYPE, ROUTINE_NAME",
                vec![schema.clone().into()],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let procedure = string(r, 1).eq_ignore_ascii_case("PROCEDURE");
                RoutineInfo {
                    name: string(r, 0),
                    schema: schema.clone(),
                    kind: if procedure { ObjectKind::Procedure } else { ObjectKind::Function },
                    return_type: if procedure { String::new() } else { string(r, 2) },
                    args: String::new(),
                }
            })
            .collect())
    }

    // Stored functions only, since MySQL doesn't list its built-ins. The sys schema's helpers count as the
    // server's own and need their schema.
    pub async fn list_functions(self: &Arc<Self>) -> Result<FunctionList, QueryError> {
        let (_, about) = self.text_rows("SELECT VERSION(), DATABASE()").await?;
        let (version, database) = about.first().map(|r| (string(r, 0), string(r, 1))).unwrap_or_default();
        let routines = self
            .rows(
                "SELECT ROUTINE_SCHEMA, ROUTINE_NAME, COALESCE(DTD_IDENTIFIER, ''), COALESCE(ROUTINE_COMMENT, '')
                FROM information_schema.ROUTINES
                WHERE ROUTINE_TYPE = 'FUNCTION'
                    AND ROUTINE_SCHEMA NOT IN ('information_schema', 'performance_schema', 'mysql')
                ORDER BY ROUTINE_SCHEMA, ROUTINE_NAME",
                Vec::new(),
            )
            .await?;
        let params = self
            .rows(
                "SELECT SPECIFIC_SCHEMA, SPECIFIC_NAME, COALESCE(PARAMETER_NAME, ''), COALESCE(DTD_IDENTIFIER, '')
                FROM information_schema.PARAMETERS
                WHERE ROUTINE_TYPE = 'FUNCTION' AND ORDINAL_POSITION > 0
                    AND SPECIFIC_SCHEMA NOT IN ('information_schema', 'performance_schema', 'mysql')
                ORDER BY SPECIFIC_SCHEMA, SPECIFIC_NAME, ORDINAL_POSITION",
                Vec::new(),
            )
            .await?;
        let mut args: HashMap<(String, String), Vec<String>> = HashMap::new();
        for r in &params {
            let arg = format!("{} {}", string(r, 2), string(r, 3)).trim().to_string();
            args.entry((string(r, 0), string(r, 1))).or_default().push(arg);
        }
        let functions = routines
            .iter()
            .map(|r| {
                let (schema, name) = (string(r, 0), string(r, 1));
                let args = args.remove(&(schema.clone(), name.clone())).unwrap_or_default().join(", ");
                FunctionInfo {
                    signatures: vec![FunctionSignature { args, returns: string(r, 2) }],
                    description: string(r, 3),
                    qualified_only: schema != database,
                    source: if schema == "sys" { schema.clone() } else { String::new() },
                    name,
                    schema,
                    ..Default::default()
                }
            })
            .collect();
        Ok(FunctionList { functions, combinators: Vec::new(), mariadb: version.contains("MariaDB") })
    }

    pub async fn object_ddl(self: &Arc<Self>, object: &ObjectRef) -> Result<String, QueryError> {
        let schema = self.schema_or(&object.schema).to_string();
        let qualified = qualified_table(&MY, &schema, &object.name);
        match object.kind {
            ObjectKind::Table => self.show_create(&format!("SHOW CREATE TABLE {qualified}"), "Create Table").await,
            ObjectKind::View => self.show_create(&format!("SHOW CREATE VIEW {qualified}"), "Create View").await,
            // Its DDL column isn't named "Create *", and the row also has a "Created" timestamp.
            ObjectKind::Trigger => {
                self.show_create(&format!("SHOW CREATE TRIGGER {qualified}"), "SQL Original Statement").await
            }
            ObjectKind::Function => {
                self.show_create(&format!("SHOW CREATE FUNCTION {qualified}"), "Create Function").await
            }
            ObjectKind::Procedure => {
                self.show_create(&format!("SHOW CREATE PROCEDURE {qualified}"), "Create Procedure").await
            }
            ObjectKind::Index => self.index_ddl(&schema, object).await,
            ObjectKind::Constraint => self.constraint_ddl(&schema, object).await,
            _ => Err(QueryError::message(unsupported_ddl(&MY, &object.kind))),
        }
    }

    // MySQL has no SHOW CREATE INDEX.
    async fn index_ddl(self: &Arc<Self>, schema: &str, object: &ObjectRef) -> Result<String, QueryError> {
        for idx in self.list_indexes(schema, &object.table).await? {
            if idx.name != object.name {
                continue;
            }
            if idx.is_primary {
                return Ok(format!(
                    "ALTER TABLE {} ADD PRIMARY KEY ({});",
                    qualified_table(&MY, schema, &object.table),
                    quote_ident_list(&MY, &idx.columns)
                ));
            }
            let synth = render_create_index(&MY, &idx);
            if !synth.is_empty() {
                return Ok(synth);
            }
        }
        Err(QueryError::message(format!("index {} not found on {}", object.name, object.table)))
    }

    async fn constraint_ddl(self: &Arc<Self>, schema: &str, object: &ObjectRef) -> Result<String, QueryError> {
        for c in self.list_constraints(schema, &object.table).await? {
            if c.name != object.name {
                continue;
            }
            let clause = render_constraint(&MY, &c);
            if clause.is_empty() {
                break;
            }
            return Ok(format!("ALTER TABLE {}\n    ADD {clause};", qualified_table(&MY, schema, &object.table)));
        }
        Err(QueryError::message(format!("constraint {} not found on {}", object.name, object.table)))
    }

    async fn show_create(self: &Arc<Self>, stmt: &str, def_column: &str) -> Result<String, QueryError> {
        let (columns, rows) = self.text_rows(stmt).await?;
        let target = column_index(&columns, def_column)
            .ok_or_else(|| QueryError::message(format!("no {def_column:?} column in {stmt}")))?;
        let row = rows.first().ok_or_else(|| QueryError::message(format!("no definition returned by {stmt}")))?;
        Ok(terminate_statement(&string(row, target)))
    }
}

// Skip "Created" in the fallback. It holds a timestamp, not DDL.
fn column_index(columns: &[String], def_column: &str) -> Option<usize> {
    columns.iter().position(|c| c.eq_ignore_ascii_case(def_column)).or_else(|| {
        columns.iter().position(|c| {
            let lower = c.to_lowercase();
            lower.starts_with("create") && lower != "created"
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_definition_column() {
        let cols = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(column_index(&cols(&["Table", "Create Table"]), "Create Table"), Some(1));
        assert_eq!(
            column_index(
                &cols(&["Trigger", "sql_mode", "SQL Original Statement", "Created"]),
                "SQL Original Statement"
            ),
            Some(2)
        );
        assert_eq!(column_index(&cols(&["Name", "Created", "Create Thing"]), "Create View"), Some(2));
        assert_eq!(column_index(&cols(&["Name", "Created"]), "Create View"), None);
        assert_eq!(column_index(&cols(&["Table", "Create Table"]), "CREATE TABLE"), Some(1));
        assert_eq!(
            column_index(
                &cols(&["View", "Create View", "character_set_client", "collation_connection"]),
                "Create View"
            ),
            Some(1)
        );
        let procedure = [
            "Procedure",
            "sql_mode",
            "Create Procedure",
            "character_set_client",
            "collation_connection",
            "Database Collation",
        ];
        assert_eq!(column_index(&cols(&procedure), "Create Procedure"), Some(2));
    }
}
