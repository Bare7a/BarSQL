use barsql_core::{DriverType, SqlDialect};

use crate::quote::quote_literal_in;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewScope {
    // What the whole server is doing right now.
    Server,
    // One table's storage and usage.
    Table,
}

// A read-only query over a server's own statistics, opened in a new tab and run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemView {
    // Its title is `systemViews.<id>`.
    pub id: &'static str,
    pub scope: ViewScope,
    // Indented to fit this file. `{schema}` and `{table}` stand for the table's names as string literals.
    sql: &'static str,
}

impl SystemView {
    pub fn title_key(&self) -> String {
        format!("systemViews.{}", self.id)
    }

    // A server view ignores the names.
    pub fn sql(&self, driver: &DriverType, schema: &str, table: &str) -> String {
        let Some(dialect) = driver.dialect() else { return String::new() };
        fill(&dedent(self.sql), &quote_literal_in(dialect, schema), &quote_literal_in(dialect, table))
    }
}

pub fn views(driver: &DriverType, scope: ViewScope) -> impl Iterator<Item = &'static SystemView> {
    driver.dialect().map_or(&[][..], all).iter().filter(move |view| view.scope == scope)
}

fn all(dialect: SqlDialect) -> &'static [SystemView] {
    match dialect {
        SqlDialect::Postgres => POSTGRES,
        SqlDialect::MySql => MYSQL,
        // SQLite keeps no statistics of its own.
        SqlDialect::Sqlite => &[],
        SqlDialect::TSql => TSQL,
        SqlDialect::ClickHouse => CLICKHOUSE,
    }
}

// One pass, so a name that itself reads `{table}` stays as it is.
fn fill(template: &str, schema: &str, table: &str) -> String {
    let mut out = String::with_capacity(template.len() + schema.len() + table.len());
    let mut rest = template;
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some(after) = rest.strip_prefix("{schema}") {
            out.push_str(schema);
            rest = after;
        } else if let Some(after) = rest.strip_prefix("{table}") {
            out.push_str(table);
            rest = after;
        } else {
            out.push('{');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

// Drops the indentation the statements have here, so they read flush left in the editor.
fn dedent(sql: &str) -> String {
    let lines: Vec<&str> = sql.lines().skip_while(|line| line.trim().is_empty()).collect();
    let indent = lines.iter().filter(|line| !line.trim().is_empty()).map(|line| line.len() - line.trim_start().len());
    let indent = indent.min().unwrap_or(0);
    lines.iter().map(|line| line.get(indent..).unwrap_or(line.trim_start())).collect::<Vec<_>>().join("\n")
}

const POSTGRES: &[SystemView] = &[
    SystemView {
        id: "activity",
        scope: ViewScope::Server,
        sql: "
            SELECT pid, usename AS user_name, datname AS database_name, application_name, client_addr, state,
                wait_event_type, wait_event, now() - query_start AS query_age, query
            FROM pg_stat_activity
            WHERE backend_type = 'client backend' AND pid <> pg_backend_pid()
            ORDER BY query_start NULLS LAST",
    },
    SystemView {
        id: "blocking",
        scope: ViewScope::Server,
        sql: "
            SELECT blocked.pid AS blocked_pid, blocked.usename AS blocked_user,
                now() - blocked.query_start AS waiting_for, blocked.wait_event_type, blocked.query AS blocked_query,
                blocking.pid AS blocking_pid, blocking.usename AS blocking_user, blocking.state AS blocking_state,
                blocking.query AS blocking_query
            FROM pg_stat_activity AS blocked
            CROSS JOIN LATERAL unnest(pg_blocking_pids(blocked.pid)) AS holder(pid)
            JOIN pg_stat_activity AS blocking ON blocking.pid = holder.pid
            ORDER BY waiting_for DESC",
    },
    SystemView {
        id: "longRunning",
        scope: ViewScope::Server,
        sql: "
            SELECT pid, usename AS user_name, datname AS database_name, state,
                now() - xact_start AS transaction_age, now() - query_start AS query_age, wait_event_type,
                wait_event, query
            FROM pg_stat_activity
            WHERE backend_type = 'client backend' AND pid <> pg_backend_pid() AND state <> 'idle'
                AND now() - COALESCE(xact_start, query_start) > interval '1 minute'
            ORDER BY COALESCE(xact_start, query_start)",
    },
    SystemView {
        id: "tableStats",
        scope: ViewScope::Table,
        sql: "
            SELECT n_live_tup AS live_rows, n_dead_tup AS dead_rows, seq_scan, seq_tup_read, idx_scan,
                idx_tup_fetch, n_tup_ins AS inserted, n_tup_upd AS updated, n_tup_hot_upd AS hot_updated,
                n_tup_del AS deleted, last_vacuum, last_autovacuum, last_analyze, last_autoanalyze,
                pg_size_pretty(pg_table_size(relid)) AS table_size,
                pg_size_pretty(pg_indexes_size(relid)) AS indexes_size,
                pg_size_pretty(pg_total_relation_size(relid)) AS total_size
            FROM pg_stat_user_tables
            WHERE schemaname = {schema} AND relname = {table}",
    },
    SystemView {
        id: "indexUsage",
        scope: ViewScope::Table,
        sql: "
            SELECT s.indexrelname AS index_name, s.idx_scan AS scans, s.idx_tup_read AS entries_read,
                s.idx_tup_fetch AS rows_fetched, pg_size_pretty(pg_relation_size(s.indexrelid)) AS size,
                i.indisunique AS is_unique, i.indisprimary AS is_primary
            FROM pg_stat_user_indexes AS s
            JOIN pg_index AS i ON i.indexrelid = s.indexrelid
            WHERE s.schemaname = {schema} AND s.relname = {table}
            ORDER BY s.idx_scan, s.indexrelname",
    },
];

const MYSQL: &[SystemView] = &[
    SystemView { id: "processList", scope: ViewScope::Server, sql: "SHOW FULL PROCESSLIST" },
    SystemView {
        id: "transactions",
        scope: ViewScope::Server,
        sql: "
            SELECT trx_mysql_thread_id AS thread_id, trx_state AS state, trx_started AS started,
                TIMESTAMPDIFF(SECOND, trx_started, NOW()) AS seconds, trx_rows_locked AS rows_locked,
                trx_rows_modified AS rows_modified, trx_tables_locked AS tables_locked,
                trx_isolation_level AS isolation_level, trx_query AS query
            FROM information_schema.INNODB_TRX
            ORDER BY trx_started",
    },
    SystemView {
        id: "tableStatus",
        scope: ViewScope::Table,
        sql: "
            SELECT ENGINE AS engine, TABLE_ROWS AS approx_rows, AVG_ROW_LENGTH AS avg_row_bytes,
                DATA_LENGTH AS data_bytes, INDEX_LENGTH AS index_bytes, DATA_FREE AS free_bytes,
                AUTO_INCREMENT AS next_auto_increment, CREATE_TIME AS created, UPDATE_TIME AS updated,
                TABLE_COLLATION AS table_collation
            FROM information_schema.TABLES
            WHERE TABLE_SCHEMA = {schema} AND TABLE_NAME = {table}",
    },
];

const TSQL: &[SystemView] = &[
    SystemView {
        id: "requests",
        scope: ViewScope::Server,
        sql: "
            SELECT r.session_id, s.login_name, DB_NAME(r.database_id) AS database_name, r.status, r.command,
                r.wait_type, r.wait_time AS wait_ms, r.blocking_session_id, r.total_elapsed_time AS elapsed_ms,
                r.cpu_time AS cpu_ms, r.logical_reads, t.text AS sql_text
            FROM sys.dm_exec_requests AS r
            JOIN sys.dm_exec_sessions AS s ON s.session_id = r.session_id
            OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) AS t
            WHERE s.is_user_process = 1 AND r.session_id <> @@SPID
            ORDER BY r.total_elapsed_time DESC",
    },
    SystemView {
        id: "sessions",
        scope: ViewScope::Server,
        sql: "
            SELECT session_id, login_name, host_name, program_name, DB_NAME(database_id) AS database_name, status,
                open_transaction_count, last_request_start_time, last_request_end_time, cpu_time AS cpu_ms,
                memory_usage * 8 AS memory_kb, reads, writes
            FROM sys.dm_exec_sessions
            WHERE is_user_process = 1
            ORDER BY last_request_start_time DESC",
    },
    SystemView {
        id: "blocking",
        scope: ViewScope::Server,
        sql: "
            SELECT r.session_id AS blocked_session, r.blocking_session_id AS blocking_session, r.wait_type,
                r.wait_time AS wait_ms, r.wait_resource, DB_NAME(r.database_id) AS database_name,
                blocked.text AS blocked_sql, holder.text AS blocking_sql
            FROM sys.dm_exec_requests AS r
            OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) AS blocked
            LEFT JOIN sys.dm_exec_connections AS c ON c.session_id = r.blocking_session_id
            OUTER APPLY sys.dm_exec_sql_text(c.most_recent_sql_handle) AS holder
            WHERE r.blocking_session_id <> 0
            ORDER BY r.wait_time DESC",
    },
    SystemView {
        id: "indexUsage",
        scope: ViewScope::Table,
        sql: "
            SELECT i.name AS index_name, i.type_desc AS index_type, i.is_unique, i.is_primary_key,
                ISNULL(u.user_seeks, 0) AS seeks, ISNULL(u.user_scans, 0) AS scans,
                ISNULL(u.user_lookups, 0) AS lookups, ISNULL(u.user_updates, 0) AS updates, u.last_user_seek,
                u.last_user_scan, size.row_count, size.used_kb
            FROM sys.indexes AS i
            LEFT JOIN sys.dm_db_index_usage_stats AS u
                ON u.database_id = DB_ID() AND u.object_id = i.object_id AND u.index_id = i.index_id
            OUTER APPLY (
                SELECT SUM(p.rows) AS row_count,
                    (SELECT SUM(a.used_pages) * 8 FROM sys.allocation_units AS a
                        JOIN sys.partitions AS ap ON a.container_id = ap.partition_id
                        WHERE ap.object_id = i.object_id AND ap.index_id = i.index_id) AS used_kb
                FROM sys.partitions AS p
                WHERE p.object_id = i.object_id AND p.index_id = i.index_id
            ) AS size
            WHERE i.object_id = OBJECT_ID(QUOTENAME({schema}) + N'.' + QUOTENAME({table}))
            ORDER BY ISNULL(u.user_seeks + u.user_scans + u.user_lookups, 0), i.index_id",
    },
];

const CLICKHOUSE: &[SystemView] = &[
    SystemView {
        id: "processes",
        scope: ViewScope::Server,
        sql: "
            SELECT query_id, user, elapsed, read_rows, formatReadableSize(read_bytes) AS read_size, written_rows,
                formatReadableSize(memory_usage) AS memory, query
            FROM system.processes
            WHERE query_id != queryID()
            ORDER BY elapsed DESC",
    },
    SystemView {
        id: "parts",
        scope: ViewScope::Table,
        sql: "
            SELECT partition, name, part_type, rows, formatReadableSize(bytes_on_disk) AS size,
                formatReadableSize(data_uncompressed_bytes) AS uncompressed, level, modification_time
            FROM system.parts
            WHERE database = {schema} AND table = {table} AND active
            ORDER BY partition, min_block_number",
    },
    SystemView {
        id: "mutations",
        scope: ViewScope::Table,
        sql: "
            SELECT mutation_id, command, create_time, is_done, parts_to_do, latest_failed_part, latest_fail_time,
                latest_fail_reason
            FROM system.mutations
            WHERE database = {schema} AND table = {table}
            ORDER BY create_time DESC",
    },
    SystemView {
        id: "merges",
        scope: ViewScope::Table,
        sql: "
            SELECT elapsed, round(progress * 100, 1) AS percent, num_parts, result_part_name, is_mutation,
                formatReadableSize(total_size_bytes_compressed) AS size, rows_read, rows_written
            FROM system.merges
            WHERE database = {schema} AND table = {table}
            ORDER BY elapsed DESC",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assert_read_only, split_statements};

    const DRIVERS: [DriverType; 6] = DriverType::KNOWN;

    fn every_view() -> impl Iterator<Item = (DriverType, &'static SystemView)> {
        DRIVERS.into_iter().flat_map(|driver| {
            let views: Vec<_> = views(&driver, ViewScope::Server).chain(views(&driver, ViewScope::Table)).collect();
            views.into_iter().map(move |view| (driver.clone(), view))
        })
    }

    #[test]
    fn every_view_is_one_read_only_statement() {
        // Names that try to end the literal early.
        for (schema, table) in [("public", "orders"), ("it's", "a\\'b"), ("x\\", "'; DROP TABLE t; --")] {
            for (driver, view) in every_view() {
                let sql = view.sql(&driver, schema, table);
                assert_eq!(assert_read_only(&driver, &sql), Ok(()), "{driver} {}: {sql}", view.id);
                assert_eq!(split_statements(&driver, &sql).len(), 1, "{driver} {}: {sql}", view.id);
            }
        }
    }

    #[test]
    fn views_read_flush_left() {
        for (driver, view) in every_view() {
            let sql = view.sql(&driver, "s", "t");
            assert!(sql.starts_with("SELECT") || sql.starts_with("SHOW"), "{sql}");
            assert!(sql.lines().count() == 1 || sql.lines().any(|line| line.starts_with("FROM ")), "{sql}");
            assert!(!sql.contains("{schema}") && !sql.contains("{table}"), "{sql}");
        }
    }

    #[test]
    fn ids_are_unique_per_driver() {
        for driver in DRIVERS {
            let mut ids: Vec<_> = every_view().filter(|(d, _)| *d == driver).map(|(_, view)| view.id).collect();
            let count = ids.len();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), count, "{driver}");
        }
    }

    #[test]
    fn sqlite_and_turso_have_no_views() {
        for driver in [DriverType::Sqlite, DriverType::Turso] {
            assert_eq!(every_view().filter(|(d, _)| *d == driver).count(), 0);
        }
    }

    #[test]
    fn names_go_in_as_literals_for_each_dialect() {
        let view = POSTGRES.iter().find(|view| view.id == "tableStats").unwrap();
        let sql = view.sql(&DriverType::Postgres, "it's", "a\\b");
        assert!(sql.ends_with("WHERE schemaname = 'it''s' AND relname = E'a\\\\b'"), "{sql}");
        let view = MYSQL.iter().find(|view| view.id == "tableStatus").unwrap();
        let sql = view.sql(&DriverType::MySql, "db", "a\\b");
        assert!(sql.ends_with("TABLE_SCHEMA = 'db' AND TABLE_NAME = CONVERT(X'615C62' USING utf8mb4)"), "{sql}");
        let view = TSQL.iter().find(|view| view.id == "indexUsage").unwrap();
        let sql = view.sql(&DriverType::SqlServer, "dbo", "it's");
        assert!(sql.contains("OBJECT_ID(QUOTENAME(N'dbo') + N'.' + QUOTENAME(N'it''s'))"), "{sql}");
        let view = CLICKHOUSE.iter().find(|view| view.id == "parts").unwrap();
        let sql = view.sql(&DriverType::ClickHouse, "db", "it's\\");
        assert!(sql.contains("database = 'db' AND table = 'it\\'s\\\\'"), "{sql}");
    }

    #[test]
    fn a_name_that_looks_like_a_placeholder_stays_put() {
        assert_eq!(fill("{schema}.{table}", "'{table}'", "'t'"), "'{table}'.'t'");
        assert_eq!(fill("a {b} {schema", "s", "t"), "a {b} {schema");
    }
}
