use std::sync::Arc;

use barsql_core::{
    ColumnInfo, ConnectionStatus, ConstraintInfo, DriverType, FunctionInfo, FunctionKind, FunctionList,
    FunctionSignature, IndexInfo, ObjectKind, ObjectRef, QueryError, RoutineInfo, SchemaInfo, TableInfo, TriggerInfo,
};
use barsql_sql::ddl::{DdlColumn, compose_create_table, join_ddl, render_constraint, unsupported_ddl};
use barsql_sql::{qualified_table, quote_ident, quote_ident_list};
use tiberius::ToSql;

use super::capture::captured;
use super::{MsEngine, ms_error, values};
use crate::ChunkBuilder;
use crate::dump::DumpColumn;

const MS: DriverType = DriverType::SqlServer;

// A column's type as declared, like nvarchar(50) or decimal(10, 2). Alias types keep their own name.
const COLUMN_TYPE: &str = "TYPE_NAME(c.user_type_id) + CASE
        WHEN c.user_type_id <> c.system_type_id THEN ''
        WHEN TYPE_NAME(c.system_type_id) IN ('varchar', 'char', 'varbinary', 'binary')
            THEN '(' + IIF(c.max_length = -1, 'max', CAST(c.max_length AS varchar(10))) + ')'
        WHEN TYPE_NAME(c.system_type_id) IN ('nvarchar', 'nchar')
            THEN '(' + IIF(c.max_length = -1, 'max', CAST(c.max_length / 2 AS varchar(10))) + ')'
        WHEN TYPE_NAME(c.system_type_id) IN ('decimal', 'numeric')
            THEN '(' + CAST(c.precision AS varchar(10)) + ', ' + CAST(c.scale AS varchar(10)) + ')'
        WHEN TYPE_NAME(c.system_type_id) IN ('datetime2', 'time', 'datetimeoffset')
            THEN '(' + CAST(c.scale AS varchar(10)) + ')'
        ELSE '' END";

// The table `@P1`.`@P2`, by name.
const TABLE_ID: &str = "OBJECT_ID(QUOTENAME(@P1) + '.' + QUOTENAME(@P2))";

type TextRow = Vec<Option<String>>;

fn string(row: &TextRow, ix: usize) -> String {
    row.get(ix).cloned().flatten().unwrap_or_default()
}

fn flag(row: &TextRow, ix: usize) -> bool {
    string(row, ix) == "1"
}

impl MsEngine {
    // Every value as text, on a pooled connection.
    pub(crate) async fn rows(self: &Arc<Self>, sql: &str, params: &[&str]) -> Result<Vec<TextRow>, QueryError> {
        let mut lease = self.lease().await?;
        let params: Vec<&dyn ToSql> = params.iter().map(|p| p as &dyn ToSql).collect();
        let mut tokens = Vec::new();
        let client = lease.client();
        let read = async move { client.query(sql, &params).await?.into_first_result().await };
        let rows = match captured(&mut tokens, read).await {
            Ok(rows) => rows,
            Err(error) => {
                lease.broken = !matches!(error, tiberius::error::Error::Server(_));
                return Err(ms_error(error, sql));
            }
        };
        let Some(columns) = rows.first().map(|r| r.columns().to_vec()) else { return Ok(Vec::new()) };
        let mut builder = ChunkBuilder::new(columns.len(), rows.len());
        for row in &rows {
            for ((_, data), column) in row.cells().zip(&columns) {
                values::push(data, column.column_type(), &mut builder);
            }
            builder.end_row();
        }
        let chunk = builder.finish();
        Ok((0..chunk.rows())
            .map(|r| (0..chunk.columns()).map(|c| chunk.display(r, c).map(str::to_string)).collect())
            .collect())
    }

    pub async fn connection_info(self: &Arc<Self>) -> Result<ConnectionStatus, QueryError> {
        let rows = self.rows("SELECT DB_NAME(), SCHEMA_NAME(), SUSER_SNAME()", &[]).await?;
        let row = rows.first().ok_or_else(|| QueryError::message("no rows in result set"))?;
        Ok(ConnectionStatus {
            connected: true,
            database: string(row, 0),
            schema: string(row, 1),
            user: string(row, 2),
            host: self.options.host.clone(),
        })
    }

    // The ones this login may open, and that are online.
    pub async fn list_databases(self: &Arc<Self>) -> Result<Vec<String>, QueryError> {
        let rows = self
            .rows("SELECT name FROM sys.databases WHERE HAS_DBACCESS(name) = 1 AND state = 0 ORDER BY name", &[])
            .await?;
        Ok(rows.iter().map(|r| string(r, 0)).collect())
    }

    // Without the system schemas and the fixed roles' own, which every database has.
    pub async fn list_schemas(self: &Arc<Self>) -> Result<Vec<SchemaInfo>, QueryError> {
        let rows = self
            .rows(
                "SELECT name FROM sys.schemas
                WHERE name NOT IN ('sys', 'INFORMATION_SCHEMA', 'guest') AND schema_id < 16384
                ORDER BY name",
                &[],
            )
            .await?;
        Ok(rows.iter().map(|r| SchemaInfo { name: string(r, 0) }).collect())
    }

    pub async fn list_tables(self: &Arc<Self>, schema: &str) -> Result<Vec<TableInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT o.name, IIF(o.type = 'V', 'view', 'table')
                FROM sys.objects o
                JOIN sys.schemas s ON s.schema_id = o.schema_id
                WHERE s.name = @P1 AND o.type IN ('U', 'V') AND o.is_ms_shipped = 0
                ORDER BY o.name",
                &[&schema],
            )
            .await?;
        Ok(rows.iter().map(|r| TableInfo { schema: schema.clone(), name: string(r, 0), kind: string(r, 1) }).collect())
    }

    pub async fn list_columns(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
        let schema = self.schema_or(schema);
        let sql = format!(
            "SELECT c.name, {COLUMN_TYPE}, CAST(c.is_nullable AS int), COALESCE(dc.definition, ''),
                IIF(pk.column_id IS NULL, 0, 1), COALESCE(fk.ref_table, ''), COALESCE(fk.ref_column, ''),
                CAST(c.is_identity AS int), CAST(c.is_computed AS int)
            FROM sys.columns c
            LEFT JOIN sys.default_constraints dc ON dc.object_id = c.default_object_id
            OUTER APPLY (
                SELECT TOP (1) ic.column_id FROM sys.indexes i
                JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
                WHERE i.object_id = c.object_id AND i.is_primary_key = 1 AND ic.column_id = c.column_id
            ) pk
            OUTER APPLY (
                SELECT TOP (1) OBJECT_NAME(fkc.referenced_object_id) AS ref_table,
                    COL_NAME(fkc.referenced_object_id, fkc.referenced_column_id) AS ref_column
                FROM sys.foreign_key_columns fkc
                WHERE fkc.parent_object_id = c.object_id AND fkc.parent_column_id = c.column_id
            ) fk
            WHERE c.object_id = {TABLE_ID}
            ORDER BY c.column_id"
        );
        let rows = self.rows(&sql, &[schema, table]).await?;
        Ok(rows
            .iter()
            .map(|r| {
                let foreign_table = string(r, 5);
                ColumnInfo {
                    name: string(r, 0),
                    data_type: string(r, 1),
                    is_nullable: flag(r, 2),
                    default_val: string(r, 3),
                    is_primary: flag(r, 4),
                    is_foreign: !foreign_table.is_empty(),
                    foreign_table,
                    foreign_column: string(r, 6),
                    is_identity: flag(r, 7),
                    is_computed: flag(r, 8),
                }
            })
            .collect())
    }

    // Key columns in key order. A heap has no index row, and included columns aren't keys.
    pub async fn list_indexes(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<IndexInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let sql = format!(
            "SELECT i.name, CAST(i.is_primary_key AS int), CAST(i.is_unique AS int), LOWER(i.type_desc),
                COL_NAME(ic.object_id, ic.column_id)
            FROM sys.indexes i
            JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
            WHERE i.object_id = {TABLE_ID} AND i.type > 0 AND ic.is_included_column = 0
            ORDER BY i.name, ic.key_ordinal, ic.index_column_id"
        );
        let rows = self.rows(&sql, &[&schema, table]).await?;
        let mut out: Vec<IndexInfo> = Vec::new();
        for r in &rows {
            let name = string(r, 0);
            if out.last().is_none_or(|idx| idx.name != name) {
                out.push(IndexInfo {
                    name: name.clone(),
                    schema: schema.clone(),
                    table: table.to_string(),
                    columns: Vec::new(),
                    is_primary: flag(r, 1),
                    is_unique: flag(r, 2),
                    method: string(r, 3),
                });
            }
            if let (Some(column), Some(idx)) = (r.get(4).cloned().flatten(), out.last_mut()) {
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
        // Each definition is whole, as the server keeps it: the key's index kind, and the foreign key's schema and
        // actions, which the columns alone don't say. The catalog's own collation differs from the database's, which
        // UNION won't mix.
        let sql = format!(
            "SELECT kc.name, IIF(kc.type = 'PK', 'PRIMARY KEY', 'UNIQUE'), COL_NAME(ic.object_id, ic.column_id), '', '',
                IIF(kc.type = 'PK', 'PRIMARY KEY ', 'UNIQUE ') + i.type_desc + ' ('
                    + (SELECT STRING_AGG(QUOTENAME(COL_NAME(k.object_id, k.column_id)), ', ')
                        WITHIN GROUP (ORDER BY k.key_ordinal)
                        FROM sys.index_columns k
                        WHERE k.object_id = i.object_id AND k.index_id = i.index_id AND k.is_included_column = 0)
                    + ')' COLLATE DATABASE_DEFAULT,
                ic.key_ordinal
            FROM sys.key_constraints kc
            JOIN sys.indexes i ON i.object_id = kc.parent_object_id AND i.index_id = kc.unique_index_id
            JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
            WHERE kc.parent_object_id = {TABLE_ID} AND ic.is_included_column = 0
            UNION ALL
            SELECT fk.name, 'FOREIGN KEY', COL_NAME(fkc.parent_object_id, fkc.parent_column_id),
                OBJECT_NAME(fkc.referenced_object_id), COL_NAME(fkc.referenced_object_id, fkc.referenced_column_id),
                'FOREIGN KEY ('
                    + (SELECT STRING_AGG(QUOTENAME(COL_NAME(k.parent_object_id, k.parent_column_id)), ', ')
                        WITHIN GROUP (ORDER BY k.constraint_column_id)
                        FROM sys.foreign_key_columns k WHERE k.constraint_object_id = fk.object_id)
                    + ') REFERENCES ' + QUOTENAME(OBJECT_SCHEMA_NAME(fk.referenced_object_id)) + '.'
                    + QUOTENAME(OBJECT_NAME(fk.referenced_object_id)) + ' ('
                    + (SELECT STRING_AGG(QUOTENAME(COL_NAME(k.referenced_object_id, k.referenced_column_id)), ', ')
                        WITHIN GROUP (ORDER BY k.constraint_column_id)
                        FROM sys.foreign_key_columns k WHERE k.constraint_object_id = fk.object_id)
                    + ')'
                    + IIF(fk.delete_referential_action = 0, '',
                        ' ON DELETE ' + REPLACE(fk.delete_referential_action_desc, '_', ' '))
                    + IIF(fk.update_referential_action = 0, '',
                        ' ON UPDATE ' + REPLACE(fk.update_referential_action_desc, '_', ' ')) COLLATE DATABASE_DEFAULT,
                fkc.constraint_column_id
            FROM sys.foreign_keys fk
            JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
            WHERE fk.parent_object_id = {TABLE_ID}
            UNION ALL
            SELECT cc.name, 'CHECK', COL_NAME(cc.parent_object_id, NULLIF(cc.parent_column_id, 0)), '', '',
                'CHECK ' + cc.definition COLLATE DATABASE_DEFAULT, 1
            FROM sys.check_constraints cc
            WHERE cc.parent_object_id = {TABLE_ID}
            ORDER BY 2, 1, 7"
        );
        let rows = self.rows(&sql, &[&schema, table]).await?;
        let mut out: Vec<ConstraintInfo> = Vec::new();
        for r in &rows {
            let name = string(r, 0);
            if out.last().is_none_or(|c| c.name != name) {
                out.push(ConstraintInfo {
                    name: name.clone(),
                    schema: schema.clone(),
                    table: table.to_string(),
                    kind: string(r, 1),
                    ref_table: string(r, 3),
                    definition: string(r, 5),
                    ..Default::default()
                });
            }
            let c = out.last_mut().expect("pushed above");
            if let Some(column) = r.get(2).cloned().flatten() {
                c.columns.push(column);
            }
            if let Some(column) = r.get(4).cloned().flatten().filter(|c| !c.is_empty()) {
                c.ref_columns.push(column);
            }
        }
        Ok(out)
    }

    pub async fn list_triggers(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<TriggerInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let sql = format!(
            "SELECT t.name, IIF(t.is_instead_of_trigger = 1, 'INSTEAD OF', 'AFTER'),
                COALESCE((SELECT STRING_AGG(te.type_desc, ', ') WITHIN GROUP (ORDER BY te.type)
                    FROM sys.trigger_events te WHERE te.object_id = t.object_id), '')
            FROM sys.triggers t
            WHERE t.parent_id = {TABLE_ID}
            ORDER BY t.name"
        );
        let rows = self.rows(&sql, &[&schema, table]).await?;
        Ok(rows
            .iter()
            .map(|r| TriggerInfo {
                name: string(r, 0),
                schema: schema.clone(),
                table: table.to_string(),
                timing: string(r, 1),
                events: string(r, 2),
            })
            .collect())
    }

    // Procedures and functions, CLR ones included. Args are for show, since T-SQL has no overloading.
    pub async fn list_routines(self: &Arc<Self>, schema: &str) -> Result<Vec<RoutineInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT o.name, RTRIM(o.type),
                    COALESCE((SELECT STRING_AGG(p.name + ' ' + TYPE_NAME(p.user_type_id), ', ')
                        WITHIN GROUP (ORDER BY p.parameter_id)
                        FROM sys.parameters p WHERE p.object_id = o.object_id AND p.parameter_id > 0), ''),
                    COALESCE((SELECT TYPE_NAME(p.user_type_id)
                        FROM sys.parameters p WHERE p.object_id = o.object_id AND p.parameter_id = 0), '')
                FROM sys.objects o
                JOIN sys.schemas s ON s.schema_id = o.schema_id
                WHERE s.name = @P1 AND o.type IN ('P', 'PC', 'FN', 'IF', 'TF', 'FS', 'FT') AND o.is_ms_shipped = 0
                ORDER BY o.name",
                &[&schema],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let kind = string(r, 1);
                let procedure = matches!(kind.as_str(), "P" | "PC");
                let returns = match kind.as_str() {
                    "IF" | "TF" | "FT" => "TABLE".to_string(),
                    _ => string(r, 3),
                };
                RoutineInfo {
                    name: string(r, 0),
                    schema: schema.clone(),
                    kind: if procedure { ObjectKind::Procedure } else { ObjectKind::Function },
                    return_type: if procedure { String::new() } else { returns },
                    args: string(r, 2),
                }
            })
            .collect())
    }

    // The database's own functions. SQL Server lists none of its built-ins, so those come from the pack.
    // A scalar function is only ever called with its schema.
    pub async fn list_functions(self: &Arc<Self>) -> Result<FunctionList, QueryError> {
        let rows = self
            .rows(
                "SELECT s.name, o.name, RTRIM(o.type),
                    COALESCE((SELECT STRING_AGG(p.name + ' ' + TYPE_NAME(p.user_type_id), ', ')
                        WITHIN GROUP (ORDER BY p.parameter_id)
                        FROM sys.parameters p WHERE p.object_id = o.object_id AND p.parameter_id > 0), ''),
                    COALESCE((SELECT TYPE_NAME(p.user_type_id)
                        FROM sys.parameters p WHERE p.object_id = o.object_id AND p.parameter_id = 0), ''),
                    COALESCE(CAST(ep.value AS nvarchar(4000)), '')
                FROM sys.objects o
                JOIN sys.schemas s ON s.schema_id = o.schema_id
                LEFT JOIN sys.extended_properties ep
                    ON ep.major_id = o.object_id AND ep.minor_id = 0 AND ep.class = 1 AND ep.name = 'MS_Description'
                WHERE o.type IN ('FN', 'IF', 'TF', 'FS', 'FT', 'AF') AND o.is_ms_shipped = 0
                ORDER BY s.name, o.name",
                &[],
            )
            .await?;
        let functions = rows
            .iter()
            .map(|r| {
                let kind = match string(r, 2).as_str() {
                    "IF" | "TF" | "FT" => FunctionKind::Table,
                    "AF" => FunctionKind::Aggregate,
                    _ => FunctionKind::Scalar,
                };
                let returns = if kind == FunctionKind::Table { "TABLE".to_string() } else { string(r, 4) };
                FunctionInfo {
                    schema: string(r, 0),
                    name: string(r, 1),
                    kind,
                    signatures: vec![FunctionSignature { args: string(r, 3), returns }],
                    description: string(r, 5),
                    qualified_only: kind == FunctionKind::Scalar,
                    ..Default::default()
                }
            })
            .collect();
        Ok(FunctionList { functions, ..Default::default() })
    }

    pub async fn object_ddl(self: &Arc<Self>, object: &ObjectRef) -> Result<String, QueryError> {
        let schema = self.schema_or(&object.schema).to_string();
        match &object.kind {
            ObjectKind::Table => Ok(self.table_ddl(&schema, &object.name, false).await?.0),
            // A module's own text, as created.
            ObjectKind::View | ObjectKind::Function | ObjectKind::Procedure | ObjectKind::Trigger => {
                let rows =
                    self.rows(&format!("SELECT OBJECT_DEFINITION({TABLE_ID})"), &[&schema, &object.name]).await?;
                match rows.first().map(|r| string(r, 0)).filter(|d| !d.is_empty()) {
                    Some(definition) => Ok(definition),
                    None => Err(QueryError::message(format!(
                        "{} has no definition to show: it doesn't exist, or it was created WITH ENCRYPTION",
                        object.name
                    ))),
                }
            }
            ObjectKind::Index => {
                let indexes = self.list_indexes(&schema, &object.table).await?;
                let index = indexes
                    .iter()
                    .find(|i| i.name == object.name)
                    .ok_or_else(|| QueryError::message(format!("index {} not found", object.name)))?;
                Ok(self.index_ddl(index).await?)
            }
            ObjectKind::Constraint => {
                let constraints = self.list_constraints(&schema, &object.table).await?;
                let constraint = constraints
                    .iter()
                    .find(|c| c.name == object.name)
                    .ok_or_else(|| QueryError::message(format!("constraint {} not found", object.name)))?;
                Ok(format!(
                    "ALTER TABLE {} ADD {};",
                    qualified_table(&MS, &schema, &object.table),
                    render_constraint(&MS, constraint)
                ))
            }
            ObjectKind::MaterializedView | ObjectKind::Other(_) => {
                Err(QueryError::message(unsupported_ddl(&MS, &object.kind)))
            }
        }
    }

    // Columns and their keys, then the indexes no constraint stands for. With `split_foreign_keys`, the foreign keys
    // come back as ALTERs for after a backup's rows, which may refer to each other in any order.
    pub(crate) async fn table_ddl(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
        split_foreign_keys: bool,
    ) -> Result<(String, Vec<String>), QueryError> {
        let columns = self.ddl_columns(schema, table).await?;
        if columns.is_empty() {
            return Err(QueryError::message(format!("table {table} not found")));
        }
        let constraints = self.list_constraints(schema, table).await?;
        let (foreign, inline): (Vec<_>, Vec<_>) =
            constraints.iter().partition(|c| split_foreign_keys && c.kind == "FOREIGN KEY");
        let clauses: Vec<String> = inline.iter().map(|c| render_constraint(&MS, c)).filter(|c| !c.is_empty()).collect();
        let target = qualified_table(&MS, schema, table);
        let foreign_keys = foreign
            .iter()
            .map(|c| render_constraint(&MS, c))
            .filter(|c| !c.is_empty())
            .map(|c| format!("ALTER TABLE {target} ADD {c};"))
            .collect();
        let mut blocks = vec![compose_create_table(&MS, schema, table, &columns, &clauses)];
        for index in self.list_indexes(schema, table).await? {
            if index.is_primary || constraints.iter().any(|c| c.name == index.name) {
                continue;
            }
            blocks.push(self.index_ddl(&index).await?);
        }
        Ok((join_ddl(&blocks), foreign_keys))
    }

    async fn ddl_columns(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<DdlColumn>, QueryError> {
        // COLLATE only where it differs from the database's.
        let sql = format!(
            "SELECT c.name, {COLUMN_TYPE}, CAST(c.is_nullable AS int), COALESCE(dc.definition, ''),
                COALESCE(CAST(ic.seed_value AS nvarchar(40)) + ', ' + CAST(ic.increment_value AS nvarchar(40)), ''),
                COALESCE(cc.definition + IIF(cc.is_persisted = 1, ' PERSISTED', ''), ''),
                IIF(c.collation_name <> CAST(DATABASEPROPERTYEX(DB_NAME(), 'Collation') AS sysname),
                    c.collation_name, '')
            FROM sys.columns c
            LEFT JOIN sys.default_constraints dc ON dc.object_id = c.default_object_id
            LEFT JOIN sys.identity_columns ic ON ic.object_id = c.object_id AND ic.column_id = c.column_id
            LEFT JOIN sys.computed_columns cc ON cc.object_id = c.object_id AND cc.column_id = c.column_id
            WHERE c.object_id = {TABLE_ID}
            ORDER BY c.column_id"
        );
        let rows = self.rows(&sql, &[schema, table]).await?;
        Ok(rows
            .iter()
            .map(|r| DdlColumn {
                name: string(r, 0),
                data_type: string(r, 1),
                not_null: !flag(r, 2),
                default: string(r, 3),
                identity: string(r, 4),
                generated: string(r, 5),
                collation: string(r, 6),
            })
            .collect())
    }

    // Clustered or not, with the included columns and a filtered index's WHERE.
    async fn index_ddl(self: &Arc<Self>, index: &IndexInfo) -> Result<String, QueryError> {
        let table = qualified_table(&MS, &index.schema, &index.table);
        if index.is_primary {
            return Ok(format!(
                "ALTER TABLE {table} ADD CONSTRAINT {} PRIMARY KEY {} ({});",
                quote_ident(&MS, &index.name),
                index.method.to_uppercase(),
                quote_ident_list(&MS, &index.columns)
            ));
        }
        let sql = format!(
            "SELECT COALESCE(i.filter_definition, ''),
                COALESCE((SELECT STRING_AGG(QUOTENAME(COL_NAME(ic.object_id, ic.column_id)), ', ')
                    WITHIN GROUP (ORDER BY ic.index_column_id)
                    FROM sys.index_columns ic
                    WHERE ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.is_included_column = 1), '')
            FROM sys.indexes i
            WHERE i.object_id = {TABLE_ID} AND i.name = @P3"
        );
        let rows = self.rows(&sql, &[&index.schema, &index.table, &index.name]).await?;
        let (filter, included) = rows.first().map(|r| (string(r, 0), string(r, 1))).unwrap_or_default();
        // NONCLUSTERED is the default, so it goes unsaid.
        let method = match index.method.as_str() {
            "nonclustered" => String::new(),
            other => format!("{} ", other.to_uppercase()),
        };
        let mut ddl = format!(
            "CREATE {}{method}INDEX {} ON {table} ({})",
            if index.is_unique { "UNIQUE " } else { "" },
            quote_ident(&MS, &index.name),
            quote_ident_list(&MS, &index.columns)
        );
        if !included.is_empty() {
            ddl.push_str(&format!(" INCLUDE ({included})"));
        }
        if !filter.is_empty() {
            ddl.push_str(&format!(" WHERE {filter}"));
        }
        ddl.push(';');
        Ok(ddl)
    }

    // Computed and rowversion columns take no value, so a backup leaves them out.
    pub(crate) async fn dump_columns(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
    ) -> Result<Vec<DumpColumn>, QueryError> {
        let sql = format!(
            "SELECT c.name, TYPE_NAME(c.system_type_id),
                IIF(c.is_computed = 1 OR TYPE_NAME(c.system_type_id) = 'timestamp', 1, 0), CAST(c.is_identity AS int)
            FROM sys.columns c
            WHERE c.object_id = {TABLE_ID}
            ORDER BY c.column_id"
        );
        let rows = self.rows(&sql, &[schema, table]).await?;
        Ok(rows
            .iter()
            .map(|r| DumpColumn {
                name: string(r, 0),
                data_type: string(r, 1),
                generated: flag(r, 2),
                identity_always: false,
                serial: flag(r, 3),
            })
            .collect())
    }
}
