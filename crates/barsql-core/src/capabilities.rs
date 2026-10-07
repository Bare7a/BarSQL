// What a driver offers, so the UI and the app ask about a feature instead of matching on the driver.
// SQL text differences live in barsql-sql's `Dialect`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    LocalFile,
    Network,
    // A single URL plus a token, like Turso.
    Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultSchema {
    Named(&'static str),
    // The schema is the database, as on MySQL.
    Database,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseField {
    Required,
    Optional,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelKind {
    // SQLite's sqlite3_interrupt.
    Interrupt,
    // Postgres' out-of-band cancel request.
    CancelRequest,
    // MySQL's KILL QUERY from a second connection.
    KillQuery,
    // The client drops the request. The server may still finish the statement.
    AbortRequest,
    // ClickHouse's KILL QUERY WHERE query_id = ...
    KillQueryById,
    // SQL Server's TDS attention packet.
    Attention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerReadOnly {
    // SQLite's PRAGMA query_only.
    QueryOnlyPragma,
    // ClickHouse's readonly setting.
    ReadonlySetting,
    // Only the client-side classifier guards a read-only connection.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogGroups {
    pub indexes: bool,
    pub constraints: bool,
    pub triggers: bool,
    pub routines: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    // Off until the engine ships: the dialog hides the driver and connecting fails.
    pub supported: bool,
    pub location: Location,
    pub ssh_tunnel: bool,
    pub tls_modes: &'static [&'static str],
    pub default_port: Option<u16>,
    pub default_user: &'static str,
    pub default_database: &'static str,
    pub default_schema: DefaultSchema,
    // Schemas loaded with the connection besides the browse schema.
    pub preload_schemas: &'static [&'static str],
    pub database: DatabaseField,
    pub database_picker: bool,
    // Tried in order when the configured database is empty or missing while listing databases.
    pub database_fallbacks: &'static [&'static str],
    // BEGIN in a tab keeps a transaction open across runs.
    pub interactive_transactions: bool,
    pub savepoints: bool,
    // SET, temporary tables and USE survive between runs in a tab.
    pub session_state: bool,
    pub row_editing: bool,
    pub explain: bool,
    pub explain_analyze: bool,
    // An EXPLAIN typed in the editor renders as a plan.
    pub typed_explain: bool,
    // Plans carry costs and row estimates.
    pub plan_metrics: bool,
    pub server_cancel: CancelKind,
    // One request can run more than one statement. The read-only classifier must then see them all.
    pub multi_statement_requests: bool,
    pub server_read_only: ServerReadOnly,
    pub catalog: CatalogGroups,
    // PRINT, NOTICE and warnings reach the Messages tab.
    pub server_messages: bool,
    pub max_rows_per_insert: Option<usize>,
}

const ALL_GROUPS: CatalogGroups = CatalogGroups { indexes: true, constraints: true, triggers: true, routines: true };
const NETWORK_TLS: &[&str] = &["disable", "require", "verify-full"];

pub const SQLITE: Capabilities = Capabilities {
    supported: true,
    location: Location::LocalFile,
    ssh_tunnel: false,
    tls_modes: &[],
    default_port: None,
    default_user: "",
    default_database: "",
    default_schema: DefaultSchema::Named("main"),
    preload_schemas: &[],
    database: DatabaseField::Absent,
    database_picker: false,
    database_fallbacks: &[],
    interactive_transactions: true,
    savepoints: true,
    session_state: true,
    row_editing: true,
    explain: true,
    explain_analyze: false,
    typed_explain: true,
    plan_metrics: false,
    server_cancel: CancelKind::Interrupt,
    multi_statement_requests: false,
    server_read_only: ServerReadOnly::QueryOnlyPragma,
    catalog: ALL_GROUPS,
    server_messages: false,
    max_rows_per_insert: None,
};

pub const POSTGRES: Capabilities = Capabilities {
    supported: true,
    location: Location::Network,
    ssh_tunnel: true,
    tls_modes: NETWORK_TLS,
    default_port: Some(5432),
    default_user: "postgres",
    default_database: "",
    default_schema: DefaultSchema::Named("public"),
    preload_schemas: &["public"],
    database: DatabaseField::Required,
    database_picker: true,
    database_fallbacks: &["postgres", "template1"],
    interactive_transactions: true,
    savepoints: true,
    session_state: true,
    row_editing: true,
    explain: true,
    explain_analyze: true,
    typed_explain: true,
    plan_metrics: true,
    server_cancel: CancelKind::CancelRequest,
    multi_statement_requests: false,
    server_read_only: ServerReadOnly::None,
    catalog: ALL_GROUPS,
    server_messages: false,
    max_rows_per_insert: None,
};

pub const MYSQL: Capabilities = Capabilities {
    supported: true,
    location: Location::Network,
    ssh_tunnel: true,
    tls_modes: NETWORK_TLS,
    default_port: Some(3306),
    default_user: "root",
    default_database: "",
    default_schema: DefaultSchema::Database,
    preload_schemas: &[],
    database: DatabaseField::Required,
    database_picker: true,
    database_fallbacks: &["information_schema"],
    interactive_transactions: true,
    savepoints: true,
    session_state: true,
    row_editing: true,
    explain: true,
    explain_analyze: true,
    typed_explain: true,
    plan_metrics: true,
    server_cancel: CancelKind::KillQuery,
    multi_statement_requests: true,
    server_read_only: ServerReadOnly::None,
    catalog: ALL_GROUPS,
    server_messages: false,
    max_rows_per_insert: None,
};

// Streams expire after about 10 s idle and transactions after 5 s, so a tab can't hold either.
pub const TURSO: Capabilities = Capabilities {
    supported: true,
    location: Location::Url,
    ssh_tunnel: true,
    tls_modes: &["require", "verify-full"],
    default_port: None,
    default_user: "",
    default_database: "",
    default_schema: DefaultSchema::Named("main"),
    preload_schemas: &[],
    database: DatabaseField::Absent,
    database_picker: false,
    database_fallbacks: &[],
    interactive_transactions: false,
    savepoints: false,
    session_state: false,
    row_editing: true,
    explain: true,
    explain_analyze: false,
    typed_explain: true,
    plan_metrics: false,
    server_cancel: CancelKind::AbortRequest,
    multi_statement_requests: false,
    server_read_only: ServerReadOnly::None,
    catalog: CatalogGroups { indexes: true, constraints: true, triggers: true, routines: false },
    server_messages: false,
    max_rows_per_insert: None,
};

// No transactions, and MergeTree keys aren't unique, so rows can't be edited by key.
pub const CLICKHOUSE: Capabilities = Capabilities {
    supported: true,
    location: Location::Network,
    ssh_tunnel: true,
    tls_modes: NETWORK_TLS,
    default_port: Some(8123),
    default_user: "default",
    default_database: "default",
    default_schema: DefaultSchema::Database,
    preload_schemas: &[],
    database: DatabaseField::Optional,
    database_picker: true,
    database_fallbacks: &["default", "system"],
    interactive_transactions: false,
    savepoints: false,
    session_state: true,
    row_editing: false,
    explain: true,
    explain_analyze: false,
    typed_explain: true,
    plan_metrics: false,
    server_cancel: CancelKind::KillQueryById,
    multi_statement_requests: false,
    server_read_only: ServerReadOnly::ReadonlySetting,
    catalog: CatalogGroups { indexes: true, constraints: false, triggers: false, routines: true },
    server_messages: false,
    max_rows_per_insert: None,
};

pub const SQL_SERVER: Capabilities = Capabilities {
    supported: true,
    location: Location::Network,
    ssh_tunnel: true,
    tls_modes: &["disable", "require", "verify-full", "strict"],
    default_port: Some(1433),
    default_user: "sa",
    default_database: "master",
    default_schema: DefaultSchema::Named("dbo"),
    preload_schemas: &["dbo"],
    database: DatabaseField::Optional,
    database_picker: true,
    database_fallbacks: &["master"],
    interactive_transactions: true,
    savepoints: true,
    session_state: true,
    row_editing: true,
    explain: true,
    explain_analyze: true,
    typed_explain: false,
    plan_metrics: true,
    server_cancel: CancelKind::Attention,
    multi_statement_requests: true,
    server_read_only: ServerReadOnly::None,
    catalog: ALL_GROUPS,
    server_messages: true,
    max_rows_per_insert: Some(1000),
};

// For drivers this build doesn't know. Everything is off.
pub const UNSUPPORTED: Capabilities = Capabilities {
    supported: false,
    location: Location::Network,
    ssh_tunnel: false,
    tls_modes: &[],
    default_port: None,
    default_user: "",
    default_database: "",
    default_schema: DefaultSchema::Named("public"),
    preload_schemas: &[],
    database: DatabaseField::Optional,
    database_picker: false,
    database_fallbacks: &[],
    interactive_transactions: false,
    savepoints: false,
    session_state: false,
    row_editing: false,
    explain: false,
    explain_analyze: false,
    typed_explain: false,
    plan_metrics: false,
    server_cancel: CancelKind::AbortRequest,
    multi_statement_requests: false,
    server_read_only: ServerReadOnly::None,
    catalog: CatalogGroups { indexes: false, constraints: false, triggers: false, routines: false },
    server_messages: false,
    max_rows_per_insert: None,
};
