// The catalog, from system.* with `{name:Type}` parameters. ClickHouse's databases are BarSQL's schemas.

use std::sync::Arc;

use barsql_core::{
    ColumnInfo, ConnectionStatus, ConstraintInfo, FunctionInfo, FunctionKind, FunctionList, FunctionSignature,
    IndexInfo, ObjectKind, ObjectRef, QueryError, RoutineInfo, SchemaInfo, TableInfo, TriggerInfo,
};
use barsql_sql::ddl::{terminate_statement, unsupported_ddl};
use barsql_sql::{qualified_table, quote_ident};

use super::{CH, ChEngine};
use crate::dump::DumpColumn;

fn text(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

// The engine decides what a relation is. Views of every sort read as views.
fn relation_kind(engine: &str) -> String {
    match engine {
        "View" | "LiveView" | "WindowView" => "view".into(),
        "MaterializedView" => "materialized view".into(),
        "Dictionary" => "dictionary".into(),
        _ => "table".into(),
    }
}

// A sorting key's expressions, split at the top-level commas.
fn key_columns(key: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in key.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(key[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(key[start..].trim().to_string());
    parts.retain(|p| !p.is_empty());
    parts
}

// The first sentence of a system.functions description, without its Markdown.
fn first_sentence(markdown: &str) -> String {
    let paragraph = markdown.trim().split("\n\n").next().unwrap_or_default().replace('\n', " ");
    let mut out = String::new();
    let mut rest = paragraph.as_str();
    // [text](link) keeps its text.
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find("](").and_then(|mid| after[mid..].find(')').map(|close| (mid, mid + close))) {
            Some((mid, close)) => {
                out.push_str(&after[..mid]);
                rest = &after[close + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let plain = out.replace('`', "");
    match plain.find(". ") {
        Some(end) => plain[..=end].trim().to_string(),
        None => plain.trim().to_string(),
    }
}

// A SQL function's arguments, from `CREATE FUNCTION add AS (a, b) -> a + b`.
fn lambda_args(create: &str) -> Option<String> {
    let (_, lambda) = create.split_once(" AS ")?;
    let open = lambda.find('(')?;
    let close = lambda.find(')')?;
    (close > open && lambda[close..].trim_start_matches(')').trim_start().starts_with("->"))
        .then(|| lambda[open + 1..close].trim().to_string())
}

// The arguments inside `name(args)`, from a syntax line like `toStartOfDay(datetime)`.
fn syntax_args(syntax: &str) -> Option<String> {
    let line = syntax.trim().lines().next()?;
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    (close > open).then(|| line[open + 1..close].trim().to_string())
}

impl ChEngine {
    pub async fn connection_info(self: &Arc<Self>) -> Result<ConnectionStatus, QueryError> {
        let rows = self.rows("SELECT currentDatabase(), currentUser()", &[]).await?;
        let row = rows.first().ok_or_else(|| QueryError::message("no rows in result set"))?;
        Ok(ConnectionStatus {
            connected: true,
            database: text(row, 0),
            schema: text(row, 0),
            user: text(row, 1),
            host: self.options.host.clone(),
        })
    }

    pub async fn list_databases(self: &Arc<Self>) -> Result<Vec<String>, QueryError> {
        let rows = self
            .rows(
                "SELECT name FROM system.databases
                WHERE name NOT IN ('INFORMATION_SCHEMA', 'information_schema')
                ORDER BY name = 'system', name",
                &[],
            )
            .await?;
        Ok(rows.iter().map(|r| text(r, 0)).collect())
    }

    pub async fn list_schemas(self: &Arc<Self>) -> Result<Vec<SchemaInfo>, QueryError> {
        Ok(self.list_databases().await?.into_iter().map(|name| SchemaInfo { name }).collect())
    }

    // A materialized view's `.inner` storage table belongs to the view.
    pub async fn list_tables(self: &Arc<Self>, schema: &str) -> Result<Vec<TableInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT name, engine FROM system.tables
                WHERE database = {db:String} AND NOT is_temporary AND NOT startsWith(name, '.inner')
                ORDER BY name",
                &[("db", &schema)],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| TableInfo { schema: schema.clone(), name: text(r, 0), kind: relation_kind(&text(r, 1)) })
            .collect())
    }

    // Keys aren't unique in ClickHouse, so no column counts as a primary key.
    pub async fn list_columns(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT name, type, default_kind, default_expression FROM system.columns
                WHERE database = {db:String} AND table = {t:String}
                ORDER BY position",
                &[("db", &schema), ("t", table)],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let (data_type, default_kind) = (text(r, 1), text(r, 2));
                ColumnInfo {
                    name: text(r, 0),
                    is_nullable: data_type.starts_with("Nullable("),
                    data_type,
                    default_val: text(r, 3),
                    is_computed: matches!(default_kind.as_str(), "MATERIALIZED" | "ALIAS"),
                    ..Default::default()
                }
            })
            .collect())
    }

    // The sorting key orders the data and the primary key indexes a prefix of it. Data-skipping indexes come after.
    pub async fn list_indexes(self: &Arc<Self>, schema: &str, table: &str) -> Result<Vec<IndexInfo>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let params = [("db", schema.as_str()), ("t", table)];
        let keys = self
            .rows("SELECT primary_key FROM system.tables WHERE database = {db:String} AND name = {t:String}", &params)
            .await?;
        let mut out: Vec<IndexInfo> = keys
            .iter()
            .map(|r| text(r, 0))
            .filter(|key| !key.is_empty())
            .map(|key| IndexInfo {
                name: "PRIMARY".into(),
                schema: schema.clone(),
                table: table.to_string(),
                columns: key_columns(&key),
                is_primary: true,
                is_unique: false,
                method: "primary key".into(),
            })
            .collect();
        let skipping = self
            .rows(
                "SELECT name, type_full, expr FROM system.data_skipping_indices
                WHERE database = {db:String} AND table = {t:String} ORDER BY name",
                &params,
            )
            .await?;
        out.extend(skipping.iter().map(|r| IndexInfo {
            name: text(r, 0),
            schema: schema.clone(),
            table: table.to_string(),
            columns: vec![text(r, 2)],
            is_primary: false,
            is_unique: false,
            method: text(r, 1),
        }));
        Ok(out)
    }

    pub async fn list_constraints(
        self: &Arc<Self>,
        _schema: &str,
        _table: &str,
    ) -> Result<Vec<ConstraintInfo>, QueryError> {
        Ok(Vec::new())
    }

    pub async fn list_triggers(self: &Arc<Self>, _schema: &str, _table: &str) -> Result<Vec<TriggerInfo>, QueryError> {
        Ok(Vec::new())
    }

    // SQL functions belong to the server rather than a database, so they show under the connection's own.
    pub async fn list_routines(self: &Arc<Self>, schema: &str) -> Result<Vec<RoutineInfo>, QueryError> {
        if self.schema_or(schema) != self.options.database {
            return Ok(Vec::new());
        }
        let rows =
            self.rows("SELECT name FROM system.functions WHERE origin = 'SQLUserDefined' ORDER BY name", &[]).await?;
        Ok(rows
            .iter()
            .map(|r| RoutineInfo {
                name: text(r, 0),
                schema: self.options.database.clone(),
                kind: ObjectKind::Function,
                ..Default::default()
            })
            .collect())
    }

    pub async fn object_ddl(self: &Arc<Self>, object: &ObjectRef) -> Result<String, QueryError> {
        let schema = self.schema_or(&object.schema).to_string();
        let one = |rows: Vec<Vec<Option<String>>>, what: &str| match rows.first() {
            Some(row) => Ok(terminate_statement(&text(row, 0))),
            None => Err(QueryError::message(format!("{what} {} not found", object.name))),
        };
        match object.kind {
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
                let sql = format!("SHOW CREATE TABLE {}", qualified_table(&CH, &schema, &object.name));
                one(self.rows(&sql, &[]).await?, "table")
            }
            ObjectKind::Function => {
                let sql = "SELECT create_query FROM system.functions WHERE name = {name:String}";
                one(self.rows(sql, &[("name", &object.name)]).await?, "function")
            }
            ObjectKind::Index => {
                let rows = self
                    .rows(
                        "SELECT expr, type_full, granularity FROM system.data_skipping_indices
                        WHERE database = {db:String} AND table = {t:String} AND name = {name:String}",
                        &[("db", &schema), ("t", &object.table), ("name", &object.name)],
                    )
                    .await?;
                let row = rows.first().ok_or_else(|| {
                    QueryError::message(format!("index {} is the table's primary key, part of its DDL", object.name))
                })?;
                Ok(format!(
                    "ALTER TABLE {} ADD INDEX {} {} TYPE {} GRANULARITY {};",
                    qualified_table(&CH, &schema, &object.table),
                    quote_ident(&CH, &object.name),
                    text(row, 0),
                    text(row, 1),
                    text(row, 2)
                ))
            }
            _ => Err(QueryError::message(unsupported_ddl(&CH, &object.kind))),
        }
    }

    // Columns a backup can write: MATERIALIZED, ALIAS and EPHEMERAL ones take no value.
    pub(crate) async fn dump_columns(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
    ) -> Result<Vec<DumpColumn>, QueryError> {
        let schema = self.schema_or(schema).to_string();
        let rows = self
            .rows(
                "SELECT name, type, default_kind FROM system.columns
                WHERE database = {db:String} AND table = {t:String} ORDER BY position",
                &[("db", &schema), ("t", table)],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| DumpColumn {
                name: text(r, 0),
                data_type: text(r, 1),
                generated: matches!(text(r, 2).as_str(), "MATERIALIZED" | "ALIAS" | "EPHEMERAL"),
                ..Default::default()
            })
            .collect())
    }

    // system.functions has every function with its docs. Table functions and combinators are listed apart.
    pub async fn list_functions(self: &Arc<Self>) -> Result<FunctionList, QueryError> {
        let rows = self
            .rows(
                "SELECT name, is_aggregate, case_insensitive, alias_to, origin, description, syntax, create_query
                FROM system.functions ORDER BY name",
                &[],
            )
            .await?;
        let mut functions: Vec<FunctionInfo> = rows
            .iter()
            .map(|r| {
                let builtin = text(r, 4) == "System";
                let args = if builtin { syntax_args(&text(r, 6)) } else { lambda_args(&text(r, 7)) };
                let signatures =
                    args.map(|args| vec![FunctionSignature { args, returns: String::new() }]).unwrap_or_default();
                FunctionInfo {
                    name: text(r, 0),
                    kind: if text(r, 1) == "1" { FunctionKind::Aggregate } else { FunctionKind::Scalar },
                    signatures,
                    description: first_sentence(&text(r, 5)),
                    builtin,
                    case_sensitive: text(r, 2) != "1",
                    alias_to: text(r, 3),
                    ..Default::default()
                }
            })
            .collect();
        let tables = self.rows("SELECT name, description FROM system.table_functions ORDER BY name", &[]).await?;
        functions.extend(tables.iter().map(|r| FunctionInfo {
            name: text(r, 0),
            kind: FunctionKind::Table,
            description: first_sentence(&text(r, 1)),
            builtin: true,
            case_sensitive: true,
            ..Default::default()
        }));
        let combinators = self.rows("SELECT name FROM system.aggregate_function_combinators", &[]).await?;
        Ok(FunctionList { functions, combinators: combinators.iter().map(|r| text(r, 0)).collect(), mariadb: false })
    }

    pub async fn primary_keys(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
    ) -> Result<(Vec<ColumnInfo>, Vec<String>), QueryError> {
        Ok((self.list_columns(schema, table).await?, Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_split_at_top_level_commas() {
        assert_eq!(key_columns("id, toStartOfDay(ts, 'UTC'), name"), ["id", "toStartOfDay(ts, 'UTC')", "name"]);
        assert!(key_columns("").is_empty());
    }

    #[test]
    fn descriptions_shrink_to_a_sentence() {
        let md = "\nRounds down a date with time to the start of the day. More text.\n\n:::note\nIgnored";
        assert_eq!(first_sentence(md), "Rounds down a date with time to the start of the day.");
        assert_eq!(first_sentence("Uses [`uniq`](/link) for `x`"), "Uses uniq for x");
        assert_eq!(syntax_args("toStartOfDay(datetime)"), Some("datetime".into()));
        assert_eq!(lambda_args("CREATE FUNCTION add AS (a, b) -> a + b"), Some("a, b".into()));
        assert_eq!(lambda_args("CREATE FUNCTION one AS () -> 1"), Some(String::new()));
        assert_eq!(syntax_args("quantile(level)(x)"), Some("level)(x".into()));
        assert_eq!(syntax_args("pi()"), Some(String::new()));
        assert_eq!(syntax_args(""), None);
    }
}
