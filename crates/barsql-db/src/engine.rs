use std::sync::Arc;

use barsql_core::schema::primary_keys;
use barsql_core::{
    ColumnInfo, ConnectionConfig, ConnectionStatus, ConstraintInfo, DriverType, FunctionList, IndexInfo, ObjectKind,
    ObjectRef, QueryError, RoutineInfo, Row, RowDelete, RowUpdate, SchemaInfo, TableDataRequest, TableInfo,
    TriggerInfo, Value,
};
use barsql_sql::dml::{build_delete, build_insert, build_table_select, build_update, first_integer_primary_key};
use barsql_sql::{
    ExplainStrategy, PlanRows, READ_ONLY_ERROR, assert_read_only, quote_ident, table_ref, validate_table_filter,
};
use mysql_async::prelude::Queryable;

use crate::clickhouse::{ChEngine, ChOptions, ChSession};
use crate::dump::{DumpQuery, dump_query};
use crate::event::{ScriptEvent, Sink, StatementResult};
use crate::mssql::{MsEngine, MsOptions, MsSession};
use crate::mysql::{MyConnectOptions, MyEngine, MySession};
use crate::postgres::{PgConnectOptions, PgEngine, PgSession};
use crate::script::{Buffered, StatementRunner};
use crate::sqlite::{SqliteConnectOptions, SqliteEngine, SqliteSession};
use crate::turso::{TursoEngine, TursoOptions, TursoSession};
use crate::{Cancel, ChunkBuilder, ResultChunk};

// A pool for Postgres, MySQL and SQL Server, a single connection for SQLite, an HTTP client for Turso and
// ClickHouse.
#[derive(Clone)]
pub enum Engine {
    Postgres(Arc<PgEngine>),
    MySql(Arc<MyEngine>),
    Sqlite(Arc<SqliteEngine>),
    Turso(Arc<TursoEngine>),
    ClickHouse(Arc<ChEngine>),
    SqlServer(Arc<MsEngine>),
}

// One per editor tab, import or transaction.
pub enum Session {
    Postgres(Box<PgSession>),
    MySql(Box<MySession>),
    Sqlite(SqliteSession),
    Turso(Box<TursoSession>),
    ClickHouse(Box<ChSession>),
    SqlServer(Box<MsSession>),
}

#[derive(Debug, Clone)]
pub struct TableQuery {
    pub sql: String,
    pub schema: String,
    pub columns: Vec<ColumnInfo>,
    pub primary_keys: Vec<String>,
}

macro_rules! dispatch {
    ($self:expr, $engine:ident => $body:expr) => {
        match $self {
            Engine::Postgres($engine) => $body,
            Engine::MySql($engine) => $body,
            Engine::Sqlite($engine) => $body,
            Engine::Turso($engine) => $body,
            Engine::ClickHouse($engine) => $body,
            Engine::SqlServer($engine) => $body,
        }
    };
}

macro_rules! dispatch_session {
    ($self:expr, $session:ident => $body:expr) => {
        match $self {
            Session::Postgres($session) => $body,
            Session::MySql($session) => $body,
            Session::Sqlite($session) => $body,
            Session::Turso($session) => $body,
            Session::ClickHouse($session) => $body,
            Session::SqlServer($session) => $body,
        }
    };
}

impl Engine {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self, QueryError> {
        Self::connect_in_zone(cfg, jiff::tz::TimeZone::system()).await
    }

    // Only Postgres uses `local`, to render timestamptz values.
    pub async fn connect_in_zone(cfg: &ConnectionConfig, local: jiff::tz::TimeZone) -> Result<Self, QueryError> {
        crate::tls::install_default_provider();
        let mut cfg = cfg.clone();
        cfg.normalize();
        cfg.validate().map_err(QueryError::message)?;
        Ok(match cfg.driver {
            DriverType::Postgres => {
                let mut options = PgConnectOptions::from_config(&cfg)?;
                options.local = local;
                Self::Postgres(PgEngine::connect(options).await?)
            }
            DriverType::MySql => Self::MySql(MyEngine::connect(MyConnectOptions::from_config(&cfg)?).await?),
            DriverType::Sqlite => Self::Sqlite(SqliteEngine::connect(SqliteConnectOptions::from_config(&cfg)).await?),
            DriverType::Turso => Self::Turso(TursoEngine::connect(TursoOptions::from_config(&cfg)?).await?),
            DriverType::ClickHouse => Self::ClickHouse(ChEngine::connect(ChOptions::from_config(&cfg)?).await?),
            DriverType::SqlServer => Self::SqlServer(MsEngine::connect(MsOptions::from_config(&cfg)?).await?),
            DriverType::Other(_) | DriverType::Unset => {
                return Err(QueryError::message(format!("unsupported driver: {}", cfg.driver)));
            }
        })
    }

    pub async fn test(cfg: &ConnectionConfig) -> Result<(), QueryError> {
        let engine = Self::connect(cfg).await?;
        let result = engine.ping().await;
        engine.close().await;
        result
    }

    // Falls back to databases every server has when the configured one is empty or missing.
    pub async fn databases(cfg: &ConnectionConfig) -> Result<Vec<String>, QueryError> {
        let mut cfg = cfg.clone();
        cfg.normalize();
        let caps = cfg.driver.capabilities();
        if !caps.database_picker {
            return Err(QueryError::message(format!("{} has no databases to list", cfg.driver)));
        }
        let fallbacks = caps.database_fallbacks;
        let typed = Some(cfg.database.clone()).filter(|d| !d.is_empty());
        let mut missing = None;
        // MySQL would connect to the browse schema instead of the database.
        cfg.schema.clear();
        for database in typed.into_iter().chain(fallbacks.iter().map(|d| d.to_string())) {
            cfg.database = database;
            let listed = async {
                let engine = Self::connect(&cfg).await?;
                let listed = engine.list_databases().await;
                engine.close().await;
                listed
            }
            .await;
            match listed {
                // Postgres' invalid_catalog_name, MySQL's ER_BAD_DB_ERROR, ClickHouse's UNKNOWN_DATABASE, SQL Server's
                // "Cannot open database".
                Err(error) if matches!(error.code.as_str(), "3D000" | "1049" | "81" | "4060") => missing = Some(error),
                other => return other,
            }
        }
        Err(missing.unwrap_or_else(|| QueryError::message("no database to connect to")))
    }

    pub fn driver(&self) -> DriverType {
        match self {
            Self::Postgres(_) => DriverType::Postgres,
            Self::MySql(_) => DriverType::MySql,
            Self::Sqlite(_) => DriverType::Sqlite,
            Self::Turso(_) => DriverType::Turso,
            Self::ClickHouse(_) => DriverType::ClickHouse,
            Self::SqlServer(_) => DriverType::SqlServer,
        }
    }

    pub fn read_only(&self) -> bool {
        dispatch!(self, e => e.read_only())
    }

    pub fn default_schema(&self) -> String {
        dispatch!(self, e => e.default_schema().to_string())
    }

    fn schema_or(&self, schema: &str) -> String {
        if schema.is_empty() { self.default_schema() } else { schema.to_string() }
    }

    pub async fn session(&self) -> Result<Session, QueryError> {
        Ok(match self {
            Self::Postgres(e) => Session::Postgres(Box::new(e.session().await?)),
            Self::MySql(e) => Session::MySql(Box::new(e.session().await?)),
            Self::Sqlite(e) => Session::Sqlite(e.session().await?),
            Self::Turso(e) => Session::Turso(Box::new(e.session().await?)),
            Self::ClickHouse(e) => Session::ClickHouse(Box::new(e.session().await?)),
            Self::SqlServer(e) => Session::SqlServer(Box::new(e.session().await?)),
        })
    }

    // Only for SQL the app generates, since the connection goes back to the pool.
    pub async fn pooled_session(&self) -> Result<Session, QueryError> {
        match self {
            Self::Postgres(e) => Ok(Session::Postgres(Box::new(e.pooled_session().await?))),
            Self::ClickHouse(e) => Ok(Session::ClickHouse(Box::new(e.pooled_session().await?))),
            Self::SqlServer(e) => Ok(Session::SqlServer(Box::new(e.pooled_session().await?))),
            Self::MySql(_) | Self::Sqlite(_) | Self::Turso(_) => self.session().await,
        }
    }

    pub async fn close(&self) {
        match self {
            Self::Postgres(e) => e.close(),
            Self::SqlServer(e) => e.close(),
            Self::MySql(e) => e.close().await,
            Self::Sqlite(_) | Self::Turso(_) | Self::ClickHouse(_) => {}
        }
    }

    pub async fn ping(&self) -> Result<(), QueryError> {
        let mut session = self.session().await?;
        session.buffered("SELECT 1", &Cancel::new()).await.map(|_| ())
    }

    pub async fn connection_info(&self) -> Result<ConnectionStatus, QueryError> {
        dispatch!(self, e => e.connection_info().await)
    }

    pub async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, QueryError> {
        dispatch!(self, e => e.list_schemas().await)
    }

    pub async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>, QueryError> {
        dispatch!(self, e => e.list_tables(schema).await)
    }

    pub async fn list_columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
        dispatch!(self, e => e.list_columns(schema, table).await)
    }

    pub async fn list_indexes(&self, schema: &str, table: &str) -> Result<Vec<IndexInfo>, QueryError> {
        dispatch!(self, e => e.list_indexes(schema, table).await)
    }

    pub async fn list_constraints(&self, schema: &str, table: &str) -> Result<Vec<ConstraintInfo>, QueryError> {
        dispatch!(self, e => e.list_constraints(schema, table).await)
    }

    pub async fn list_triggers(&self, schema: &str, table: &str) -> Result<Vec<TriggerInfo>, QueryError> {
        dispatch!(self, e => e.list_triggers(schema, table).await)
    }

    pub async fn list_routines(&self, schema: &str) -> Result<Vec<RoutineInfo>, QueryError> {
        dispatch!(self, e => e.list_routines(schema).await)
    }

    // Every schema at once, built-ins included where the server lists them.
    pub async fn list_functions(&self) -> Result<FunctionList, QueryError> {
        dispatch!(self, e => e.list_functions().await)
    }

    // Reads the catalog only, so it stays available on read-only connections.
    pub async fn object_ddl(&self, object: &ObjectRef) -> Result<String, QueryError> {
        if object.name.is_empty() {
            return Err(QueryError::message("object name is required"));
        }
        dispatch!(self, e => e.object_ddl(object).await)
    }

    pub async fn list_databases(&self) -> Result<Vec<String>, QueryError> {
        match self {
            Self::Postgres(e) => e.list_databases().await,
            Self::MySql(e) => Ok(e.list_schemas().await?.into_iter().map(|s| s.name).collect()),
            Self::ClickHouse(e) => e.list_databases().await,
            Self::SqlServer(e) => e.list_databases().await,
            Self::Sqlite(_) | Self::Turso(_) => Ok(Vec::new()),
        }
    }

    pub async fn dump_query(&self, schema: &str, table: &str) -> Result<DumpQuery, QueryError> {
        let schema = self.schema_or(schema);
        let driver = self.driver();
        let (columns, target) = match self {
            Self::Postgres(e) => (e.dump_columns(&schema, table).await?, table_ref(&driver, &schema, table)),
            Self::MySql(e) => (e.dump_columns(&schema, table).await?, quote_ident(&driver, table)),
            Self::Sqlite(e) => (e.dump_columns(table).await?, quote_ident(&driver, table)),
            Self::Turso(e) => (e.dump_columns(table).await?, quote_ident(&driver, table)),
            Self::ClickHouse(e) => (e.dump_columns(&schema, table).await?, quote_ident(&driver, table)),
            Self::SqlServer(e) => (e.dump_columns(&schema, table).await?, table_ref(&driver, &schema, table)),
        };
        if columns.is_empty() {
            return Err(QueryError::message(format!("table {table} not found")));
        }
        dump_query(&driver, &table_ref(&driver, &schema, table), &target, &columns).map_err(QueryError::message)
    }

    // Postgres returns its foreign keys separately, to add after the rows. The other engines' files turn FK
    // checks off instead.
    pub async fn backup_ddl(&self, schema: &str, table: &str) -> Result<(String, Vec<String>), QueryError> {
        let schema = self.schema_or(schema);
        match self {
            Self::Postgres(e) => e.table_ddl(&schema, table, true).await,
            Self::SqlServer(e) => e.table_ddl(&schema, table, true).await,
            _ => {
                let object =
                    ObjectRef { schema, name: table.to_string(), kind: ObjectKind::Table, ..Default::default() };
                Ok((self.object_ddl(&object).await?, Vec::new()))
            }
        }
    }

    // Validated even on writable connections so the filter can never escape the WHERE clause.
    pub async fn table_query(&self, req: &TableDataRequest) -> Result<TableQuery, QueryError> {
        validate_table_filter(&self.driver(), &req.filter).map_err(QueryError::message)?;
        let schema = self.schema_or(&req.schema);
        let columns = self.list_columns(&schema, &req.table).await?;
        let pks = primary_keys(&columns);
        let sql = build_table_select(&self.driver(), &schema, req, &columns, &pks);
        Ok(TableQuery { sql, schema, columns, primary_keys: pks })
    }

    pub async fn update_row(&self, update: &RowUpdate) -> Result<(), QueryError> {
        self.require_writable()?;
        let schema = self.schema_or(&update.schema);
        let pks = primary_keys(&self.list_columns(&schema, &update.table).await?);
        if pks.is_empty() {
            return Err(QueryError::message("table has no primary key"));
        }
        let (sql, args) =
            build_update(&self.driver(), &schema, &update.table, &update.changes, &update.primary_key, &pks)
                .map_err(QueryError::message)?;
        self.execute_params(&sql, &args).await.map(|_| ())
    }

    // One statement per key. A failure keeps the rows already deleted.
    pub async fn delete_rows(&self, delete: &RowDelete) -> (i64, Option<QueryError>) {
        if let Err(err) = self.require_writable() {
            return (0, Some(err));
        }
        let schema = self.schema_or(&delete.schema);
        let pks = match self.list_columns(&schema, &delete.table).await {
            Ok(cols) => primary_keys(&cols),
            Err(err) => return (0, Some(err)),
        };
        if pks.is_empty() {
            return (0, Some(QueryError::message("table has no primary key")));
        }
        let mut total = 0;
        for key in &delete.primary_keys {
            let (sql, args) = match build_delete(&self.driver(), &schema, &delete.table, &pks, key) {
                Ok(built) => built,
                Err(err) => return (total, Some(QueryError::message(err))),
            };
            match self.execute_params(&sql, &args).await {
                Ok(n) => total += n as i64,
                Err(err) => return (total, Some(err)),
            }
        }
        (total, None)
    }

    // Returns the stored row with defaults and computed columns, like Postgres RETURNING *.
    pub async fn insert_row(&self, schema: &str, table: &str, values: &Row) -> Result<Row, QueryError> {
        self.require_writable()?;
        let driver = self.driver();
        match self {
            Self::Postgres(_) => {
                let schema = self.schema_or(schema);
                let sql = postgres_insert_sql(&schema, table, values)?;
                let mut session = self.session().await?;
                let res = session.buffered(&format!("{sql} RETURNING *"), &Cancel::new()).await?;
                Ok(first_row(&res))
            }
            Self::MySql(engine) => {
                let schema = self.schema_or(schema);
                let (sql, args) = build_insert(&driver, &schema, table, values).map_err(QueryError::message)?;
                let mut session = engine.session().await?;
                session.execute_params(&sql, &args).await?;
                let id = session.conn().last_insert_id().unwrap_or(0);
                if id == 0 {
                    return Ok(Row::new());
                }
                let cols = self.list_columns(&schema, table).await.unwrap_or_default();
                match reselect_mysql(&mut session, &schema, table, &cols, id as i64).await {
                    Some(row) => Ok(row),
                    None => Ok(Row::from([("id".to_string(), Value::Int(id as i64))])),
                }
            }
            // OUTPUT goes before VALUES. A table with triggers takes no plain OUTPUT (error 334), so an identity
            // key finds the row instead, in the same scope as the insert.
            Self::SqlServer(engine) => {
                let schema = self.schema_or(schema);
                let (sql, args) = build_insert(&driver, &schema, table, values).map_err(QueryError::message)?;
                let mut session = engine.session().await?;
                let output = sql.replacen(") VALUES (", ") OUTPUT INSERTED.* VALUES (", 1);
                match session.query_params(&output, &args).await {
                    Ok(res) => Ok(first_row(&res)),
                    Err(error) if error.code == "334" => {
                        let cols = self.list_columns(&schema, table).await.unwrap_or_default();
                        let Some(pk) = cols.iter().find(|c| c.is_primary && c.is_identity) else {
                            session.execute_params(&sql, &args).await?;
                            return Ok(Row::new());
                        };
                        let target = table_ref(&driver, &schema, table);
                        let reselect = format!(
                            "{sql}; SELECT * FROM {target} WHERE {} = SCOPE_IDENTITY()",
                            quote_ident(&driver, &pk.name)
                        );
                        Ok(first_row(&session.query_params(&reselect, &args).await?))
                    }
                    Err(error) => Err(error),
                }
            }
            // No RETURNING and no unique keys to look the row up by, so the view reloads instead.
            Self::ClickHouse(_) => {
                let schema = self.schema_or(schema);
                let (sql, args) = build_insert(&driver, &schema, table, values).map_err(QueryError::message)?;
                self.execute_params(&sql, &args).await?;
                Ok(Row::new())
            }
            // libSQL has RETURNING, so the stored row comes back with the insert.
            Self::Turso(engine) => {
                let (sql, args) = build_insert(&driver, schema, table, values).map_err(QueryError::message)?;
                let mut session = engine.session().await?;
                let res = session.buffered_params(&format!("{sql} RETURNING *"), &args).await?;
                Ok(first_row(&res))
            }
            Self::Sqlite(engine) => {
                let (sql, args) = build_insert(&driver, schema, table, values).map_err(QueryError::message)?;
                let params: Vec<rusqlite::types::Value> = args.iter().map(crate::sqlite::lite_value).collect();
                let rowid = engine
                    .with_conn(move |conn| {
                        conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
                            .map_err(|err| crate::sqlite::lite_error(&err))?;
                        Ok(conn.last_insert_rowid())
                    })
                    .await?;
                let mut session = self.session().await?;
                if rowid > 0 {
                    let cols = self.list_columns(schema, table).await.unwrap_or_default();
                    if let Some(pk) = first_integer_primary_key(&cols) {
                        let sql = format!(
                            "SELECT * FROM {} WHERE {} = {} LIMIT 1",
                            table_ref(&driver, schema, table),
                            quote_ident(&driver, pk),
                            rowid
                        );
                        if let Ok(res) = session.buffered(&sql, &Cancel::new()).await
                            && res.rows() > 0
                        {
                            return Ok(first_row(&res));
                        }
                    }
                    return Ok(Row::from([("rowid".to_string(), Value::Int(rowid))]));
                }
                if values.is_empty() {
                    return Ok(Row::new());
                }
                // No integer key, so look the row up by the values just written.
                let filters: Vec<String> = values
                    .iter()
                    .map(|(col, value)| format!("{} = {}", quote_ident(&driver, col), crate::lite::literal(value)))
                    .collect();
                let sql =
                    format!("SELECT * FROM {} WHERE {} LIMIT 1", quote_ident(&driver, table), filters.join(" AND "));
                match session.buffered(&sql, &Cancel::new()).await {
                    Ok(res) if res.rows() > 0 => Ok(first_row(&res)),
                    _ => Ok(Row::new()),
                }
            }
        }
    }

    // Parameters can't change the statement, but the read-only gate still applies.
    pub async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<u64, QueryError> {
        if self.read_only() {
            assert_read_only(&self.driver(), sql).map_err(QueryError::message)?;
        }
        self.session().await?.execute_params(sql, params).await
    }

    fn require_writable(&self) -> Result<(), QueryError> {
        if self.read_only() { Err(QueryError::message(READ_ONLY_ERROR)) } else { Ok(()) }
    }
}

impl Session {
    pub fn driver(&self) -> DriverType {
        dispatch_session!(self, s => s.driver())
    }

    pub fn read_only(&self) -> bool {
        dispatch_session!(self, s => s.read_only())
    }

    // Repeats the app's gate for read-only sessions. Internal `buffered` queries aren't user SQL and skip it.
    fn assert_allowed(&self, sql: &str) -> Result<(), QueryError> {
        if self.read_only() { assert_read_only(&self.driver(), sql).map_err(QueryError::message) } else { Ok(()) }
    }

    pub async fn run_script(
        &mut self,
        statements: &[String],
        sink: &Sink,
        cancel: &Cancel,
    ) -> Result<usize, QueryError> {
        if let Some((stmt, error)) = statements.iter().find_map(|s| self.assert_allowed(s).err().map(|e| (s, e))) {
            let result = StatementResult { statement: stmt.clone(), error: Some(error.clone()), ..Default::default() };
            crate::event::emit(sink, ScriptEvent::Result(Box::new(result))).await;
            return Err(error);
        }
        dispatch_session!(self, s => s.run_script(statements, sink, cancel).await)
    }

    // One statement as typed, no plan detection. For table pages and helpers.
    pub async fn stream(&mut self, sql: &str, sink: &Sink, cancel: &Cancel) -> Result<(), QueryError> {
        self.assert_allowed(sql)?;
        let run =
            dispatch_session!(self, s => s.stream_statement(sql, 0, crate::event::BATCH_ROWS, sink, cancel).await);
        match run.error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    pub async fn buffered(&mut self, sql: &str, cancel: &Cancel) -> Result<Buffered, QueryError> {
        dispatch_session!(self, s => s.buffered(sql, cancel).await)
    }

    pub async fn plan_rows(&mut self, strategy: &ExplainStrategy, cancel: &Cancel) -> Result<PlanRows, QueryError> {
        // SQLite's session gives the file's one connection back afterwards, as its `buffered` does.
        if let Self::Sqlite(s) = self {
            return s.plan_rows(strategy, cancel).await;
        }
        dispatch_session!(self, s => crate::script::plan_rows(s, strategy, cancel).await)
    }

    pub async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64, QueryError> {
        dispatch_session!(self, s => s.execute_params(sql, params).await)
    }

    pub async fn begin(&mut self) -> Result<(), QueryError> {
        dispatch_session!(self, s => s.begin().await)
    }

    pub async fn commit(&mut self) -> Result<(), QueryError> {
        dispatch_session!(self, s => s.commit().await)
    }

    pub async fn rollback(&mut self) -> Result<(), QueryError> {
        dispatch_session!(self, s => s.rollback().await)
    }

    pub fn in_transaction(&self) -> bool {
        dispatch_session!(self, s => s.in_transaction())
    }

    pub fn is_broken(&self) -> bool {
        dispatch_session!(self, s => s.is_broken())
    }
}

fn first_row(res: &Buffered) -> Row {
    if res.rows() == 0 {
        return Row::new();
    }
    res.columns.iter().enumerate().map(|(i, c)| (c.name.clone(), res.chunk.cell(0, i).to_value())).collect()
}

// Inlined literals, not parameters, so RETURNING comes back as server text instead of binary.
fn postgres_insert_sql(schema: &str, table: &str, values: &Row) -> Result<String, QueryError> {
    if values.is_empty() {
        return Err(QueryError::message("no column values provided"));
    }
    let driver = DriverType::Postgres;
    let cols: Vec<String> = values.keys().map(|c| quote_ident(&driver, c)).collect();
    let literals: Vec<String> = values.values().map(crate::postgres::pg_literal).collect();
    Ok(format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_ref(&driver, schema, table),
        cols.join(", "),
        literals.join(", ")
    ))
}

// Goes through a prepared statement, so values arrive in the binary protocol.
async fn reselect_mysql(
    session: &mut MySession,
    schema: &str,
    table: &str,
    cols: &[ColumnInfo],
    id: i64,
) -> Option<Row> {
    let pk = first_integer_primary_key(cols)?;
    let driver = DriverType::MySql;
    let sql =
        format!("SELECT * FROM {} WHERE {} = ? LIMIT 1", table_ref(&driver, schema, table), quote_ident(&driver, pk));
    let mut result = session.conn().exec_iter(sql, vec![mysql_async::Value::Int(id)]).await.ok()?;
    let columns = result.columns()?;
    let decoders: Vec<crate::mysql::Decoder> = columns.iter().map(crate::mysql::Decoder::for_column).collect();
    let rows: Vec<mysql_async::Row> = result.collect().await.ok()?;
    let row = rows.first()?;
    let mut builder = ChunkBuilder::new(columns.len(), 1);
    for (i, decoder) in decoders.iter().enumerate() {
        match row.as_ref(i) {
            Some(value) => crate::mysql::push_binary_value(value, decoder, &mut builder),
            None => builder.push_null(),
        }
    }
    builder.end_row();
    let chunk: ResultChunk = builder.finish();
    Some(columns.iter().enumerate().map(|(i, c)| (c.name_str().into_owned(), chunk.cell(0, i).to_value())).collect())
}
