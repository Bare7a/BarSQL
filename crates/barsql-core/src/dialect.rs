// The SQL a connection speaks. Several drivers can share one: Turso speaks SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SqlDialect {
    Postgres,
    MySql,
    Sqlite,
    TSql,
    ClickHouse,
}

impl SqlDialect {
    pub const ALL: [SqlDialect; 5] = [Self::Postgres, Self::MySql, Self::Sqlite, Self::TSql, Self::ClickHouse];

    // Stable ids. The golden fixtures key their per-dialect outputs by these.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::Sqlite => "sqlite",
            Self::TSql => "tsql",
            Self::ClickHouse => "clickhouse",
        }
    }
}
