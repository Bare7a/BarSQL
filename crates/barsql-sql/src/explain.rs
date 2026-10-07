use std::fmt;
use std::sync::LazyLock;

use barsql_core::{DriverType, SqlDialect};
use regex::Regex;

use crate::split::split_statements;
use crate::sql_text::{first_keyword, strip_leading_comments, to_upper};

// MariaDB and MySQL ask for a measured plan differently, and MySQL only supports it from 8.0.18.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerVersion {
    pub mariadb: bool,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl ServerVersion {
    pub fn parse(raw: &str) -> Self {
        static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([0-9]+)\.([0-9]+)(?:\.([0-9]+))?").unwrap());
        let mut raw = raw.trim();
        let mariadb = raw.to_lowercase().contains("mariadb");
        if mariadb {
            // Old MariaDB builds prefix "5.5.5-" so clients gating on 5.x keep working.
            raw = raw.strip_prefix("5.5.5-").unwrap_or(raw);
        }
        let mut version = Self { mariadb, ..Default::default() };
        if let Some(caps) = NUMBER.captures(raw) {
            let part = |i: usize| caps.get(i).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
            version.major = part(1);
            version.minor = part(2);
            version.patch = part(3);
        }
        version
    }

    fn at_least(&self, major: u32, minor: u32, patch: u32) -> bool {
        (self.major, self.minor, self.patch) >= (major, minor, patch)
    }
}

impl fmt::Display for ServerVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

pub fn single_statement(driver: &DriverType, sql: &str) -> Result<String, String> {
    let stmts = split_statements(driver, sql);
    match stmts.as_slice() {
        [] => Err("no statement to explain".into()),
        [only] => Ok(only.text.to_string()),
        _ => Err("select a single statement to explain".into()),
    }
}

static PG_EXPLAIN_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)^explain[\t\n\f\r ]*(?:\(.*?\)[\t\n\f\r ]*)?(?:analyze[\t\n\f\r ]+)?(?:verbose[\t\n\f\r ]+)?")
        .unwrap()
});
static MYSQL_EXPLAIN_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)^(?:explain|analyze)[\t\n\f\r ]+(?:analyze[\t\n\f\r ]+)?(?:format[\t\n\f\r ]*=[\t\n\f\r ]*[0-9A-Za-z_]+[\t\n\f\r ]+)?",
    )
    .unwrap()
});
static SQLITE_EXPLAIN_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)^explain[\t\n\f\r ]+(?:query[\t\n\f\r ]+plan[\t\n\f\r ]+)?").unwrap());
// EXPLAIN [kind] [setting = value, ...]. The kinds other than PLAN show something other than a plan.
static CLICKHOUSE_EXPLAIN_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)^explain[\t\n\f\r ]+(?:(plan|ast|syntax|query[\t\n\f\r ]+tree|pipeline|estimate|table[\t\n\f\r ]+override)[\t\n\f\r ]+)?(?:[0-9A-Za-z_]+[\t\n\f\r ]*=[\t\n\f\r ]*[0-9A-Za-z_']+[\t\n\f\r ]*,?[\t\n\f\r ]*)*",
    )
    .unwrap()
});

// Explaining an EXPLAIN plans the inner statement instead of nesting a second EXPLAIN.
fn strip_explain_prefix<'a>(driver: &DriverType, stmt: &'a str) -> &'a str {
    let re = match driver.dialect() {
        Some(SqlDialect::Postgres) => &*PG_EXPLAIN_PREFIX,
        Some(SqlDialect::MySql) => &*MYSQL_EXPLAIN_PREFIX,
        Some(SqlDialect::Sqlite) => &*SQLITE_EXPLAIN_PREFIX,
        Some(SqlDialect::ClickHouse) => &*CLICKHOUSE_EXPLAIN_HEAD,
        Some(SqlDialect::TSql) | None => return stmt,
    };
    let Some(prefix) = re.find(stmt).map(|m| m.as_str()).filter(|p| !p.is_empty()) else {
        return stmt;
    };
    let rest = stmt[prefix.len()..].trim();
    if !explains_a_statement(&to_upper(prefix.trim()), rest) {
        return stmt;
    }
    rest
}

fn is_explainable_start(stmt: &str) -> bool {
    matches!(
        first_keyword(stmt).as_str(),
        "SELECT" | "WITH" | "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "MERGE" | "VALUES" | "TABLE"
    )
}

// `ANALYZE TABLE t` is MySQL's statistics command, not a plan of a TABLE statement.
fn explains_a_statement(keyword: &str, rest: &str) -> bool {
    is_explainable_start(rest) && !(keyword == "ANALYZE" && first_keyword(rest) == "TABLE")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRequest {
    pub sql: String,
    pub analyze: bool,
}

#[derive(Debug, Default)]
struct ExplainIntent {
    analyze: bool,
    format: String,
    inner: String,
    // SQLite's bare EXPLAIN dumps bytecode, not a plan.
    query_plan: bool,
}

static PG_EXPLAIN_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)^explain[\t\n\f\r ]*(?:\(([^)]*)\)[\t\n\f\r ]*)?((?:(?:analyze|verbose)[\t\n\f\r ]+)*)").unwrap()
});
static PG_ANALYZE_OPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)(?-u:\b)analyze(?-u:\b)(?:[\t\n\f\r ]+([0-9A-Za-z_]+))?").unwrap());
static PG_FORMAT_OPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)(?-u:\b)format[\t\n\f\r ]+([0-9A-Za-z_]+)").unwrap());
static MYSQL_EXPLAIN_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)^(explain|analyze)[\t\n\f\r ]+(analyze[\t\n\f\r ]+)?(?:format[\t\n\f\r ]*=[\t\n\f\r ]*([0-9A-Za-z_]+)[\t\n\f\r ]+)?",
    )
    .unwrap()
});
static SQLITE_EXPLAIN_HEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)^explain[\t\n\f\r ]+(query[\t\n\f\r ]+plan[\t\n\f\r ]+)?").unwrap());

fn is_falsey(word: &str) -> bool {
    matches!(word.to_lowercase().as_str(), "false" | "off" | "0")
}

fn parse_explain_intent(driver: &DriverType, stmt: &str) -> Option<ExplainIntent> {
    match driver.dialect() {
        Some(SqlDialect::Postgres) => {
            let caps = PG_EXPLAIN_HEAD.captures(stmt)?;
            let mut intent =
                ExplainIntent { inner: stmt[caps.get(0)?.end()..].trim().to_string(), ..Default::default() };
            if let Some(options) = caps.get(1).map(|m| m.as_str()).filter(|o| !o.is_empty()) {
                if let Some(analyze) = PG_ANALYZE_OPTION.captures(options) {
                    intent.analyze = !is_falsey(analyze.get(1).map_or("", |m| m.as_str()));
                }
                if let Some(format) = PG_FORMAT_OPTION.captures(options) {
                    intent.format = format[1].to_lowercase();
                }
            }
            // Legacy syntax without parentheses, e.g. EXPLAIN ANALYZE VERBOSE <stmt>.
            if caps.get(2).is_some_and(|m| m.as_str().to_lowercase().contains("analyze")) {
                intent.analyze = true;
            }
            Some(intent)
        }
        Some(SqlDialect::MySql) => {
            let caps = MYSQL_EXPLAIN_HEAD.captures(stmt)?;
            Some(ExplainIntent {
                analyze: caps[1].eq_ignore_ascii_case("analyze") || caps.get(2).is_some(),
                format: caps.get(3).map_or(String::new(), |m| m.as_str().to_lowercase()),
                inner: stmt[caps.get(0)?.end()..].trim().to_string(),
                query_plan: false,
            })
        }
        Some(SqlDialect::Sqlite) => {
            let caps = SQLITE_EXPLAIN_HEAD.captures(stmt)?;
            Some(ExplainIntent {
                inner: stmt[caps.get(0)?.end()..].trim().to_string(),
                query_plan: caps.get(1).is_some(),
                ..Default::default()
            })
        }
        Some(SqlDialect::ClickHouse) => {
            let caps = CLICKHOUSE_EXPLAIN_HEAD.captures(stmt)?;
            Some(ExplainIntent {
                inner: stmt[caps.get(0)?.end()..].trim().to_string(),
                query_plan: caps.get(1).is_none_or(|kind| kind.as_str().eq_ignore_ascii_case("plan")),
                ..Default::default()
            })
        }
        Some(SqlDialect::TSql) | None => None,
    }
}

// Structured plans (FORMAT JSON, MySQL's EXPLAIN ANALYZE tree, SQLite's EXPLAIN QUERY PLAN) run as typed.
// Postgres text and MySQL's traditional table are re-run as JSON. None when the user wants raw output.
pub fn detect_plan_request(driver: &DriverType, stmt: &str) -> Option<PlanRequest> {
    let stmt = strip_leading_comments(stmt).trim();
    let first = first_keyword(stmt);
    let dialect = driver.dialect();
    if first != "EXPLAIN" && !(dialect == Some(SqlDialect::MySql) && first == "ANALYZE") {
        return None;
    }
    let intent = parse_explain_intent(driver, stmt)?;
    if !explains_a_statement(&first, &intent.inner) {
        return None;
    }
    match dialect {
        Some(SqlDialect::Postgres) => match intent.format.as_str() {
            "json" => Some(PlanRequest { sql: stmt.to_string(), analyze: intent.analyze }),
            "" => build_explain_sql(driver, ServerVersion::default(), &intent.inner, intent.analyze)
                .ok()
                .map(|sql| PlanRequest { sql, analyze: intent.analyze }),
            _ => None,
        },
        Some(SqlDialect::MySql) => match intent.format.as_str() {
            "json" | "tree" => Some(PlanRequest { sql: stmt.to_string(), analyze: intent.analyze }),
            // EXPLAIN ANALYZE already returns the tree. Other bare forms return the traditional table.
            "" if intent.analyze && first == "EXPLAIN" => Some(PlanRequest { sql: stmt.to_string(), analyze: true }),
            "" if intent.analyze => {
                Some(PlanRequest { sql: format!("ANALYZE FORMAT=JSON {}", intent.inner), analyze: true })
            }
            "" => Some(PlanRequest { sql: format!("EXPLAIN FORMAT=JSON {}", intent.inner), analyze: false }),
            _ => None,
        },
        Some(SqlDialect::Sqlite) => intent.query_plan.then(|| PlanRequest { sql: stmt.to_string(), analyze: false }),
        // A typed EXPLAIN [PLAN] runs as JSON, whatever settings it named. AST, PIPELINE and the rest stay a grid.
        Some(SqlDialect::ClickHouse) => intent
            .query_plan
            .then(|| PlanRequest { sql: format!("{CLICKHOUSE_PLAN} {}", intent.inner), analyze: false }),
        Some(SqlDialect::TSql) | None => None,
    }
}

const CLICKHOUSE_PLAN: &str = "EXPLAIN PLAN json = 1, indexes = 1";
// The column SQL Server's XML plans come in, whichever SET option asked for them.
pub const SHOWPLAN_COLUMN: &str = "Microsoft SQL Server 2005 XML Showplan";

// How to ask for a statement's plan. Most engines put an EXPLAIN in front of it. SQL Server turns a session option
// on, sends the statement (which then only gets planned, or runs and reports its plan too), and turns it off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplainStrategy {
    Query(String),
    // The plan comes in a result set with `plan_column`. Each of `setup` and `teardown` goes as its own batch.
    Session { setup: Vec<String>, statement: String, teardown: Vec<String>, plan_column: &'static str },
}

impl ExplainStrategy {
    // The SQL the plan view shows as what it ran.
    pub fn display_sql(&self) -> String {
        match self {
            Self::Query(sql) => sql.clone(),
            Self::Session { setup, statement, teardown, .. } => {
                setup.iter().chain([statement]).chain(teardown).map(String::as_str).collect::<Vec<_>>().join("\nGO\n")
            }
        }
    }
}

// `analyze` means the statement actually runs, so callers must isolate a write themselves.
pub fn build_explain(
    driver: &DriverType,
    version: ServerVersion,
    stmt: &str,
    analyze: bool,
) -> Result<ExplainStrategy, String> {
    if driver.dialect() != Some(SqlDialect::TSql) {
        return build_explain_sql(driver, version, stmt, analyze).map(ExplainStrategy::Query);
    }
    let stmt = stmt.trim();
    let stmt = stmt.strip_suffix(';').unwrap_or(stmt).trim();
    if stmt.is_empty() {
        return Err("no statement to explain".into());
    }
    // SHOWPLAN_XML only plans. STATISTICS XML runs the statement and adds its plan, with the actual counts and times.
    let option = if analyze { "STATISTICS XML" } else { "SHOWPLAN_XML" };
    Ok(ExplainStrategy::Session {
        setup: vec![format!("SET {option} ON")],
        statement: stmt.to_string(),
        teardown: vec![format!("SET {option} OFF")],
        plan_column: SHOWPLAN_COLUMN,
    })
}

// The EXPLAIN in front of the statement, for every engine but SQL Server.
pub fn build_explain_sql(
    driver: &DriverType,
    version: ServerVersion,
    stmt: &str,
    analyze: bool,
) -> Result<String, String> {
    let stmt = stmt.trim();
    let stmt = stmt.strip_suffix(';').unwrap_or(stmt).trim();
    if stmt.is_empty() {
        return Err("no statement to explain".into());
    }
    let stmt = strip_explain_prefix(driver, stmt);
    match driver.dialect() {
        Some(SqlDialect::Postgres) if analyze => Ok(format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {stmt}")),
        Some(SqlDialect::Postgres) => Ok(format!("EXPLAIN (FORMAT JSON) {stmt}")),
        Some(SqlDialect::MySql) if !analyze => Ok(format!("EXPLAIN FORMAT=JSON {stmt}")),
        Some(SqlDialect::MySql) if version.mariadb => Ok(format!("ANALYZE FORMAT=JSON {stmt}")),
        // MySQL takes FORMAT=JSON on EXPLAIN ANALYZE only from 8.3, but TREE works from 8.0.18.
        Some(SqlDialect::MySql) if version.at_least(8, 0, 18) => Ok(format!("EXPLAIN ANALYZE {stmt}")),
        Some(SqlDialect::MySql) => {
            Err(format!("EXPLAIN ANALYZE needs MySQL 8.0.18 or newer (server reports {version})"))
        }
        Some(SqlDialect::Sqlite) if analyze => {
            Err("SQLite has no EXPLAIN ANALYZE; explain without it for the plan shape".into())
        }
        Some(SqlDialect::Sqlite) => Ok(format!("EXPLAIN QUERY PLAN {stmt}")),
        Some(SqlDialect::ClickHouse) if analyze => {
            Err("ClickHouse has no EXPLAIN ANALYZE; explain without it for the plan shape".into())
        }
        Some(SqlDialect::ClickHouse) => Ok(format!("{CLICKHOUSE_PLAN} {stmt}")),
        Some(SqlDialect::TSql) => Err("SQL Server plans come from a session option; see build_explain".into()),
        None => Err(format!("unsupported driver: {driver}")),
    }
}
