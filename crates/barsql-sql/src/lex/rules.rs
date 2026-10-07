use barsql_core::{DriverType, SqlDialect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashComment {
    Never,
    Always,
    // ClickHouse: `# ` and `#!` start a comment.
    SpaceOrBang,
}

// How a dialect lexes SQL text. The splitter, the editor and the read-only classifier all read the same rules,
// so they can't disagree about where a string or a statement ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexRules {
    pub hash_comments: HashComment,
    // MySQL needs whitespace after `--` for a comment.
    pub dash_needs_space: bool,
    pub nested_block_comments: bool,
    // Backslash escapes in '...'.
    pub backslash_escapes: bool,
    // E'...' has backslash escapes, as on Postgres.
    pub escape_strings: bool,
    // "..." is a string literal, as on MySQL.
    pub double_quote_strings: bool,
    // Backslash escapes in "..." and `...` identifiers, as on ClickHouse.
    pub ident_backslash: bool,
    // [ident] with ]] for a `]`.
    pub bracket_idents: bool,
    // N'...' is one string token.
    pub national_strings: bool,
    pub dollar_quotes: bool,
    // `$` continues a word, so `a$x$` is one identifier rather than `a` and a dollar quote.
    pub dollar_in_words: bool,
    // MySQL runs the body of /*! ... */ and MariaDB the body of /*M! ... */.
    pub exec_comments: bool,
    // `@var`, `@@ROWCOUNT` and `#temp` are words.
    pub at_hash_words: bool,
    // mysql client `DELIMITER xx` lines.
    pub client_delimiters: bool,
    // `GO [n]` lines split batches.
    pub batch_separators: bool,
    // Like sqlite3_complete, a CREATE TRIGGER ends at `; END ;`.
    pub trigger_bodies: bool,
    // T-SQL: `;` inside BEGIN ... END doesn't end an editor statement, and a CREATE PROCEDURE runs to GO.
    pub block_depth: bool,
    // `->` is one operator (ClickHouse lambdas).
    pub arrow_op: bool,
}

impl LexRules {
    // Without a driver the editor and classifier use this conservative common subset.
    pub const COMMON: Self = Self {
        hash_comments: HashComment::Never,
        dash_needs_space: false,
        nested_block_comments: false,
        backslash_escapes: false,
        escape_strings: true,
        double_quote_strings: false,
        ident_backslash: false,
        bracket_idents: false,
        national_strings: false,
        dollar_quotes: true,
        dollar_in_words: false,
        // Only MySQL and MariaDB run them, but reading the body as code is the cautious side.
        exec_comments: true,
        at_hash_words: false,
        client_delimiters: false,
        batch_separators: false,
        trigger_bodies: false,
        block_depth: false,
        arrow_op: false,
    };

    pub const POSTGRES: Self = Self { nested_block_comments: true, dollar_in_words: true, ..Self::COMMON };

    pub const MYSQL: Self = Self {
        hash_comments: HashComment::Always,
        dash_needs_space: true,
        backslash_escapes: true,
        double_quote_strings: true,
        dollar_quotes: false,
        dollar_in_words: true,
        client_delimiters: true,
        ..Self::COMMON
    };

    pub const SQLITE: Self = Self {
        bracket_idents: true,
        escape_strings: false,
        dollar_in_words: true,
        trigger_bodies: true,
        ..Self::COMMON
    };

    pub const TSQL: Self = Self {
        nested_block_comments: true,
        escape_strings: false,
        exec_comments: false,
        bracket_idents: true,
        national_strings: true,
        dollar_quotes: false,
        dollar_in_words: true,
        at_hash_words: true,
        batch_separators: true,
        block_depth: true,
        ..Self::COMMON
    };

    pub const CLICKHOUSE: Self = Self {
        hash_comments: HashComment::SpaceOrBang,
        nested_block_comments: true,
        backslash_escapes: true,
        escape_strings: false,
        exec_comments: false,
        ident_backslash: true,
        arrow_op: true,
        ..Self::COMMON
    };

    pub const fn of(dialect: SqlDialect) -> Self {
        match dialect {
            SqlDialect::Postgres => Self::POSTGRES,
            SqlDialect::MySql => Self::MYSQL,
            SqlDialect::Sqlite => Self::SQLITE,
            SqlDialect::TSql => Self::TSQL,
            SqlDialect::ClickHouse => Self::CLICKHOUSE,
        }
    }

    pub fn for_driver(driver: Option<&DriverType>) -> Self {
        match driver.and_then(DriverType::dialect) {
            Some(dialect) => Self::of(dialect),
            None => Self::COMMON,
        }
    }
}
