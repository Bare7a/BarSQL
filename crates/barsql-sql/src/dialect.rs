use barsql_core::{DriverType, SqlDialect};

use crate::lex::LexRules;
use crate::readonly::{self, ReadOnlyRules};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentQuote {
    // "x" with "" for a quote.
    Double,
    // `x` with `` for a backtick.
    Backtick,
    // `x` with \` and \\ escapes, as ClickHouse writes them.
    BacktickBackslash,
    // [x] with ]] for a bracket.
    Bracket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder {
    // $1, $2, ...
    Dollar,
    Question,
    // @P1, @P2, ... as tiberius names them.
    AtP,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paging {
    LimitOffset,
    // ORDER BY ... OFFSET n ROWS FETCH NEXT m ROWS ONLY, which needs an ORDER BY.
    OffsetFetch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preview {
    Limit,
    Top,
}

// Column types for imported CSV columns, by the import's logical type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportTypes {
    pub boolean: &'static str,
    pub int: &'static str,
    pub float: &'static str,
    pub date: &'static str,
    pub timestamp: &'static str,
    pub text: &'static str,
}

// How a dialect writes SQL. Every field is spelled out for every dialect, so a new one is a checklist the compiler
// enforces rather than a fallback that quietly behaves like Postgres.
#[derive(Debug, Clone, Copy)]
pub struct Dialect {
    // None for a driver this build doesn't know.
    pub id: Option<SqlDialect>,
    pub lex: LexRules,
    pub ident_quote: IdentQuote,
    pub placeholder: Placeholder,
    // SQLite tables are never schema-qualified.
    pub qualify_tables: bool,
    pub paging: Paging,
    pub preview: Preview,
    pub import_types: ImportTypes,
    pub max_bind_params: Option<usize>,
    // UPDATE and DELETE by primary key. ClickHouse keys aren't unique.
    pub row_edits: bool,
    pub cascade: bool,
    pub restart_identity: bool,
    pub materialized_views: bool,
    pub index_using: bool,
    // Appended to CREATE TABLE, for ClickHouse's table engine.
    pub create_table_suffix: &'static str,
    // Where an unqualified table resolves when the catalog can't tell. None takes the first listed schema.
    pub default_schema: Option<&'static str>,
    pub read_only: &'static ReadOnlyRules,
}

const ANSI_TYPES: ImportTypes = ImportTypes {
    boolean: "boolean",
    int: "bigint",
    float: "double precision",
    date: "date",
    timestamp: "timestamp",
    text: "text",
};

pub static POSTGRES: Dialect = Dialect {
    id: Some(SqlDialect::Postgres),
    lex: LexRules::POSTGRES,
    ident_quote: IdentQuote::Double,
    placeholder: Placeholder::Dollar,
    qualify_tables: true,
    paging: Paging::LimitOffset,
    preview: Preview::Limit,
    import_types: ANSI_TYPES,
    max_bind_params: Some(65535),
    row_edits: true,
    cascade: true,
    restart_identity: true,
    materialized_views: true,
    index_using: true,
    create_table_suffix: "",
    default_schema: Some("public"),
    read_only: &readonly::STANDARD,
};

pub static MYSQL: Dialect = Dialect {
    id: Some(SqlDialect::MySql),
    lex: LexRules::MYSQL,
    ident_quote: IdentQuote::Backtick,
    placeholder: Placeholder::Question,
    qualify_tables: true,
    paging: Paging::LimitOffset,
    preview: Preview::Limit,
    import_types: ImportTypes {
        boolean: "TINYINT(1)",
        int: "BIGINT",
        float: "DOUBLE",
        date: "DATE",
        timestamp: "DATETIME",
        text: "TEXT",
    },
    max_bind_params: Some(65535),
    row_edits: true,
    cascade: false,
    restart_identity: false,
    materialized_views: false,
    index_using: false,
    create_table_suffix: "",
    default_schema: None,
    read_only: &readonly::STANDARD,
};

pub static SQLITE: Dialect = Dialect {
    id: Some(SqlDialect::Sqlite),
    lex: LexRules::SQLITE,
    ident_quote: IdentQuote::Double,
    placeholder: Placeholder::Question,
    qualify_tables: false,
    paging: Paging::LimitOffset,
    preview: Preview::Limit,
    import_types: ImportTypes {
        boolean: "INTEGER",
        int: "INTEGER",
        float: "REAL",
        date: "TEXT",
        timestamp: "TEXT",
        text: "TEXT",
    },
    max_bind_params: Some(32766),
    row_edits: true,
    cascade: false,
    restart_identity: false,
    materialized_views: false,
    index_using: false,
    create_table_suffix: "",
    default_schema: None,
    read_only: &readonly::STANDARD,
};

pub static TSQL: Dialect = Dialect {
    id: Some(SqlDialect::TSql),
    lex: LexRules::TSQL,
    ident_quote: IdentQuote::Bracket,
    placeholder: Placeholder::AtP,
    qualify_tables: true,
    paging: Paging::OffsetFetch,
    preview: Preview::Top,
    import_types: ImportTypes {
        boolean: "BIT",
        int: "BIGINT",
        float: "FLOAT",
        date: "DATE",
        timestamp: "DATETIME2",
        text: "NVARCHAR(MAX)",
    },
    // 2100 per request, one of which tiberius keeps for the statement text.
    max_bind_params: Some(2099),
    row_edits: true,
    cascade: false,
    restart_identity: false,
    materialized_views: false,
    index_using: false,
    create_table_suffix: "",
    default_schema: Some("dbo"),
    read_only: &readonly::TSQL,
};

pub static CLICKHOUSE: Dialect = Dialect {
    id: Some(SqlDialect::ClickHouse),
    lex: LexRules::CLICKHOUSE,
    ident_quote: IdentQuote::BacktickBackslash,
    // The engine inlines the values, since the HTTP interface has no positional parameters.
    placeholder: Placeholder::Question,
    qualify_tables: true,
    paging: Paging::LimitOffset,
    preview: Preview::Limit,
    // Columns aren't nullable by default, and a partly blank source column must not fail the load.
    import_types: ImportTypes {
        boolean: "Nullable(Bool)",
        int: "Nullable(Int64)",
        float: "Nullable(Float64)",
        date: "Nullable(Date32)",
        timestamp: "Nullable(DateTime64(6))",
        text: "Nullable(String)",
    },
    max_bind_params: None,
    row_edits: false,
    cascade: false,
    restart_identity: false,
    materialized_views: true,
    index_using: false,
    create_table_suffix: " ENGINE = MergeTree ORDER BY tuple()",
    default_schema: None,
    read_only: &readonly::CLICKHOUSE,
};

// For a driver this build doesn't know: ANSI quoting with Postgres-style types, as before dialects existed.
pub static ANSI: Dialect = Dialect {
    id: None,
    lex: LexRules::COMMON,
    ident_quote: IdentQuote::Double,
    placeholder: Placeholder::Question,
    qualify_tables: true,
    paging: Paging::LimitOffset,
    preview: Preview::Limit,
    import_types: ANSI_TYPES,
    max_bind_params: None,
    row_edits: true,
    cascade: false,
    restart_identity: false,
    materialized_views: false,
    index_using: false,
    create_table_suffix: "",
    default_schema: None,
    read_only: &readonly::STANDARD,
};

impl Dialect {
    pub fn of(dialect: SqlDialect) -> &'static Dialect {
        match dialect {
            SqlDialect::Postgres => &POSTGRES,
            SqlDialect::MySql => &MYSQL,
            SqlDialect::Sqlite => &SQLITE,
            SqlDialect::TSql => &TSQL,
            SqlDialect::ClickHouse => &CLICKHOUSE,
        }
    }

    pub fn for_driver(driver: &DriverType) -> &'static Dialect {
        driver.dialect().map_or(&ANSI, Self::of)
    }
}
