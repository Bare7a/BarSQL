use barsql_core::{QueryError, SqlDialect};
use barsql_sql::structure::Step;

use crate::{Cancel, Session};

const PAUSE_FOREIGN_KEYS: &str = "PRAGMA foreign_keys = OFF";

// A structure plan's steps, in order on one session. A failure rolls back what the plan opened and restores the
// pragmas a SQLite rebuild changed, since a SQLite file shares its one connection with every tab.
pub async fn apply(session: &mut Session, steps: &[Step], cancel: &Cancel) -> Result<(), QueryError> {
    let Err(error) = run(session, steps, cancel).await else { return Ok(()) };
    let sqlite_recovery: &[&str] = match paused_foreign_keys(steps) {
        true => &["ROLLBACK", "PRAGMA legacy_alter_table = OFF", "PRAGMA foreign_keys = ON"],
        false => &["ROLLBACK"],
    };
    let recovery: &[&str] = match session.driver().dialect() {
        Some(SqlDialect::Postgres) => &["ROLLBACK"],
        Some(SqlDialect::Sqlite) => sqlite_recovery,
        Some(SqlDialect::TSql) => &["IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION"],
        Some(SqlDialect::MySql | SqlDialect::ClickHouse) | None => &[],
    };
    // A cancelled plan still has to clean up, so the recovery runs uncancelled. Its own errors don't matter.
    for sql in recovery {
        let _ = session.buffered(sql, &Cancel::new()).await;
    }
    Err(error)
}

async fn run(session: &mut Session, steps: &[Step], cancel: &Cancel) -> Result<(), QueryError> {
    for step in steps {
        let result = session.buffered(&step.sql, cancel).await?;
        if step.expect_no_rows && result.rows() > 0 {
            return Err(QueryError::message("Rows break a foreign key after the rebuild"));
        }
    }
    Ok(())
}

fn paused_foreign_keys(steps: &[Step]) -> bool {
    let spaced = |sql: &str| sql.split_whitespace().collect::<Vec<_>>().join(" ");
    steps.iter().any(|step| spaced(&step.sql).eq_ignore_ascii_case(PAUSE_FOREIGN_KEYS))
}
