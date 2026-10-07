use barsql_core::{DriverType, ObjectKind, ObjectRef, SqlDialect};

use crate::dialect::Dialect;
use crate::quote::{quote_ident, quote_literal, table_ref};

// SQL for the schema tree's Rename, Truncate and Drop actions. None of it ends with a semicolon.

// MySQL drops each constraint kind with a different statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    PrimaryKey,
    ForeignKey,
    Unique,
    Check,
    Other,
}

impl ConstraintKind {
    pub fn parse(kind: &str) -> Self {
        match kind.to_uppercase().as_str() {
            "PRIMARY KEY" => Self::PrimaryKey,
            "FOREIGN KEY" => Self::ForeignKey,
            "UNIQUE" => Self::Unique,
            "CHECK" => Self::Check,
            _ => Self::Other,
        }
    }
}

// Only Postgres gets CASCADE. The other engines lack the clause or ignore it.
fn cascade(driver: &DriverType, on: bool) -> &'static str {
    if on && Dialect::for_driver(driver).cascade { " CASCADE" } else { "" }
}

// SQL Server renames through sp_rename. Its first argument may use brackets, but its second is the bare new
// name, so brackets there would become part of it.
fn sp_rename(object: &str, new_name: &str, kind: Option<&str>) -> String {
    let kind = kind.map(|k| format!(", {}", quote_literal(k))).unwrap_or_default();
    format!("EXEC sp_rename N{}, N{}{kind}", quote_literal(object), quote_literal(new_name))
}

// SQLite can't rename views, so those return None.
pub fn rename_relation(
    driver: &DriverType,
    kind: &ObjectKind,
    schema: &str,
    name: &str,
    new_name: &str,
) -> Option<String> {
    let from = table_ref(driver, schema, name);
    let to = quote_ident(driver, new_name);
    Some(match Dialect::for_driver(driver).id {
        Some(SqlDialect::MySql | SqlDialect::ClickHouse) => {
            format!("RENAME TABLE {from} TO {}", table_ref(driver, schema, new_name))
        }
        Some(SqlDialect::Sqlite) if *kind == ObjectKind::Table => format!("ALTER TABLE {from} RENAME TO {to}"),
        Some(SqlDialect::Sqlite) => return None,
        Some(SqlDialect::TSql) => sp_rename(&from, new_name, None),
        Some(SqlDialect::Postgres) | None => match kind {
            ObjectKind::View => format!("ALTER VIEW {from} RENAME TO {to}"),
            ObjectKind::MaterializedView => format!("ALTER MATERIALIZED VIEW {from} RENAME TO {to}"),
            _ => format!("ALTER TABLE {from} RENAME TO {to}"),
        },
    })
}

pub fn drop_relation(driver: &DriverType, kind: &ObjectKind, schema: &str, name: &str, cascades: bool) -> String {
    let postgres = Dialect::for_driver(driver).id == Some(SqlDialect::Postgres);
    let what = match kind {
        ObjectKind::View => "VIEW",
        ObjectKind::MaterializedView if postgres => "MATERIALIZED VIEW",
        ObjectKind::MaterializedView => "VIEW",
        _ => "TABLE",
    };
    format!("DROP {what} {}{}", table_ref(driver, schema, name), cascade(driver, cascades))
}

// SQLite has no TRUNCATE, so it deletes every row.
pub fn truncate_table(
    driver: &DriverType,
    schema: &str,
    table: &str,
    restart_identity: bool,
    cascades: bool,
) -> String {
    let target = table_ref(driver, schema, table);
    match Dialect::for_driver(driver).id {
        Some(SqlDialect::Sqlite) => format!("DELETE FROM {target}"),
        Some(SqlDialect::Postgres) => {
            let restart = if restart_identity { " RESTART IDENTITY" } else { "" };
            format!("TRUNCATE TABLE {target}{restart}{}", cascade(driver, cascades))
        }
        Some(SqlDialect::MySql | SqlDialect::TSql | SqlDialect::ClickHouse) | None => {
            format!("TRUNCATE TABLE {target}")
        }
    }
}

pub fn rename_column(driver: &DriverType, schema: &str, table: &str, column: &str, new_name: &str) -> String {
    let table = table_ref(driver, schema, table);
    match Dialect::for_driver(driver).id {
        Some(SqlDialect::TSql) => {
            sp_rename(&format!("{table}.{}", quote_ident(driver, column)), new_name, Some("COLUMN"))
        }
        Some(SqlDialect::Postgres | SqlDialect::MySql | SqlDialect::Sqlite | SqlDialect::ClickHouse) | None => {
            format!(
                "ALTER TABLE {table} RENAME COLUMN {} TO {}",
                quote_ident(driver, column),
                quote_ident(driver, new_name)
            )
        }
    }
}

pub fn drop_column(driver: &DriverType, schema: &str, table: &str, column: &str, cascades: bool) -> String {
    format!(
        "ALTER TABLE {} DROP COLUMN {}{}",
        table_ref(driver, schema, table),
        quote_ident(driver, column),
        cascade(driver, cascades)
    )
}

// None when no statement can drop it, e.g. SQLite constraints and auto indexes or an unnamed constraint.
pub fn drop_object(
    driver: &DriverType,
    object: &ObjectRef,
    constraint: ConstraintKind,
    cascades: bool,
) -> Option<String> {
    let name = quote_ident(driver, &object.name);
    let table = table_ref(driver, &object.schema, &object.table);
    let qualified = table_ref(driver, &object.schema, &object.name);
    let tail = cascade(driver, cascades);
    let dialect = Dialect::for_driver(driver).id;
    let sql = match &object.kind {
        ObjectKind::Index => match dialect {
            Some(SqlDialect::Sqlite) if object.name.starts_with("sqlite_autoindex_") => return None,
            Some(SqlDialect::MySql | SqlDialect::TSql) => format!("DROP INDEX {name} ON {table}"),
            // A ClickHouse data-skipping index belongs to its table.
            Some(SqlDialect::ClickHouse) => format!("ALTER TABLE {table} DROP INDEX {name}"),
            Some(SqlDialect::Postgres | SqlDialect::Sqlite) | None => format!("DROP INDEX {qualified}{tail}"),
        },
        ObjectKind::Constraint if object.name.is_empty() => return None,
        ObjectKind::Constraint => match dialect {
            Some(SqlDialect::Sqlite) => return None,
            Some(SqlDialect::MySql) => match constraint {
                ConstraintKind::PrimaryKey => format!("ALTER TABLE {table} DROP PRIMARY KEY"),
                ConstraintKind::ForeignKey => format!("ALTER TABLE {table} DROP FOREIGN KEY {name}"),
                ConstraintKind::Unique => format!("ALTER TABLE {table} DROP INDEX {name}"),
                ConstraintKind::Check | ConstraintKind::Other => format!("ALTER TABLE {table} DROP CONSTRAINT {name}"),
            },
            Some(SqlDialect::Postgres | SqlDialect::TSql | SqlDialect::ClickHouse) | None => {
                format!("ALTER TABLE {table} DROP CONSTRAINT {name}{tail}")
            }
        },
        ObjectKind::Trigger => match dialect {
            Some(SqlDialect::Postgres) => format!("DROP TRIGGER {name} ON {table}{tail}"),
            Some(SqlDialect::ClickHouse) => return None,
            Some(SqlDialect::MySql | SqlDialect::Sqlite | SqlDialect::TSql) | None => {
                format!("DROP TRIGGER {qualified}")
            }
        },
        ObjectKind::Function | ObjectKind::Procedure => {
            let what = if object.kind == ObjectKind::Procedure { "PROCEDURE" } else { "FUNCTION" };
            match dialect {
                Some(SqlDialect::Sqlite) => return None,
                Some(SqlDialect::Postgres) => format!("DROP {what} {qualified}({}){tail}", object.args),
                // ClickHouse user functions are global and there are no procedures.
                Some(SqlDialect::ClickHouse) if object.kind == ObjectKind::Function => format!("DROP FUNCTION {name}"),
                Some(SqlDialect::ClickHouse) => return None,
                Some(SqlDialect::MySql | SqlDialect::TSql) | None => format!("DROP {what} {qualified}"),
            }
        }
        _ => return None,
    };
    Some(sql)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: DriverType = DriverType::Postgres;
    const MY: DriverType = DriverType::MySql;
    const LITE: DriverType = DriverType::Sqlite;

    fn object(kind: ObjectKind, name: &str) -> ObjectRef {
        ObjectRef { schema: "app".into(), name: name.into(), kind, table: "users".into(), args: String::new() }
    }

    #[test]
    fn renames_per_engine_and_kind() {
        let rename = |driver: &DriverType, kind: ObjectKind| rename_relation(driver, &kind, "app", "users", "people");
        assert_eq!(rename(&PG, ObjectKind::Table).unwrap(), r#"ALTER TABLE "app"."users" RENAME TO "people""#);
        assert_eq!(rename(&PG, ObjectKind::View).unwrap(), r#"ALTER VIEW "app"."users" RENAME TO "people""#);
        assert_eq!(
            rename(&PG, ObjectKind::MaterializedView).unwrap(),
            r#"ALTER MATERIALIZED VIEW "app"."users" RENAME TO "people""#
        );
        assert_eq!(rename(&MY, ObjectKind::View).unwrap(), "RENAME TABLE `app`.`users` TO `app`.`people`");
        assert_eq!(rename(&LITE, ObjectKind::Table).unwrap(), r#"ALTER TABLE "users" RENAME TO "people""#);
        assert_eq!(rename(&LITE, ObjectKind::View), None);
        assert_eq!(
            rename_column(&MY, "app", "users", "e`mail", "email"),
            "ALTER TABLE `app`.`users` RENAME COLUMN `e``mail` TO `email`"
        );
        assert_eq!(
            rename_column(&LITE, "main", "users", "a\"b", "c"),
            r#"ALTER TABLE "users" RENAME COLUMN "a""b" TO "c""#
        );
    }

    #[test]
    fn drops_and_truncates_cascade_only_on_postgres() {
        assert_eq!(drop_relation(&PG, &ObjectKind::Table, "app", "users", true), r#"DROP TABLE "app"."users" CASCADE"#);
        assert_eq!(
            drop_relation(&PG, &ObjectKind::MaterializedView, "app", "totals", false),
            r#"DROP MATERIALIZED VIEW "app"."totals""#
        );
        assert_eq!(drop_relation(&MY, &ObjectKind::View, "app", "v", true), "DROP VIEW `app`.`v`");
        assert_eq!(drop_relation(&LITE, &ObjectKind::Table, "main", "t", true), r#"DROP TABLE "t""#);
        assert_eq!(
            truncate_table(&PG, "app", "users", true, true),
            r#"TRUNCATE TABLE "app"."users" RESTART IDENTITY CASCADE"#
        );
        assert_eq!(truncate_table(&PG, "app", "users", false, false), r#"TRUNCATE TABLE "app"."users""#);
        assert_eq!(truncate_table(&MY, "app", "users", true, true), "TRUNCATE TABLE `app`.`users`");
        assert_eq!(truncate_table(&LITE, "main", "users", true, true), r#"DELETE FROM "users""#);
        assert_eq!(
            drop_column(&PG, "app", "users", "age", true),
            r#"ALTER TABLE "app"."users" DROP COLUMN "age" CASCADE"#
        );
        assert_eq!(drop_column(&LITE, "main", "users", "age", true), r#"ALTER TABLE "users" DROP COLUMN "age""#);
    }

    #[test]
    fn object_drops_follow_each_engines_syntax() {
        let drop =
            |driver: &DriverType, object: &ObjectRef, kind: ConstraintKind| drop_object(driver, object, kind, false);
        let index = object(ObjectKind::Index, "users_email_idx");
        assert_eq!(drop(&PG, &index, ConstraintKind::Other).unwrap(), r#"DROP INDEX "app"."users_email_idx""#);
        assert_eq!(drop(&MY, &index, ConstraintKind::Other).unwrap(), "DROP INDEX `users_email_idx` ON `app`.`users`");
        assert_eq!(drop(&LITE, &index, ConstraintKind::Other).unwrap(), r#"DROP INDEX "users_email_idx""#);
        assert_eq!(drop(&LITE, &object(ObjectKind::Index, "sqlite_autoindex_users_1"), ConstraintKind::Other), None);

        let fk = object(ObjectKind::Constraint, "users_org_fk");
        assert_eq!(
            drop_object(&PG, &fk, ConstraintKind::ForeignKey, true).unwrap(),
            r#"ALTER TABLE "app"."users" DROP CONSTRAINT "users_org_fk" CASCADE"#
        );
        assert_eq!(
            drop(&MY, &fk, ConstraintKind::ForeignKey).unwrap(),
            "ALTER TABLE `app`.`users` DROP FOREIGN KEY `users_org_fk`"
        );
        assert_eq!(drop(&MY, &fk, ConstraintKind::PrimaryKey).unwrap(), "ALTER TABLE `app`.`users` DROP PRIMARY KEY");
        assert_eq!(
            drop(&MY, &fk, ConstraintKind::Unique).unwrap(),
            "ALTER TABLE `app`.`users` DROP INDEX `users_org_fk`"
        );
        assert_eq!(
            drop(&MY, &fk, ConstraintKind::Check).unwrap(),
            "ALTER TABLE `app`.`users` DROP CONSTRAINT `users_org_fk`"
        );
        assert_eq!(drop(&LITE, &fk, ConstraintKind::ForeignKey), None);
        assert_eq!(drop(&PG, &object(ObjectKind::Constraint, ""), ConstraintKind::Check), None);

        let trigger = object(ObjectKind::Trigger, "users_touch");
        assert_eq!(
            drop(&PG, &trigger, ConstraintKind::Other).unwrap(),
            r#"DROP TRIGGER "users_touch" ON "app"."users""#
        );
        assert_eq!(drop(&MY, &trigger, ConstraintKind::Other).unwrap(), "DROP TRIGGER `app`.`users_touch`");
        assert_eq!(drop(&LITE, &trigger, ConstraintKind::Other).unwrap(), r#"DROP TRIGGER "users_touch""#);

        let mut function = object(ObjectKind::Function, "add");
        function.args = "a integer, b integer".into();
        assert_eq!(
            drop(&PG, &function, ConstraintKind::Other).unwrap(),
            r#"DROP FUNCTION "app"."add"(a integer, b integer)"#
        );
        assert_eq!(drop(&MY, &function, ConstraintKind::Other).unwrap(), "DROP FUNCTION `app`.`add`");
        let procedure = object(ObjectKind::Procedure, "tidy");
        assert_eq!(drop(&PG, &procedure, ConstraintKind::Other).unwrap(), r#"DROP PROCEDURE "app"."tidy"()"#);
        assert_eq!(drop(&MY, &procedure, ConstraintKind::Other).unwrap(), "DROP PROCEDURE `app`.`tidy`");
    }

    #[test]
    fn new_dialects_rename_and_drop_their_way() {
        let mssql = DriverType::SqlServer;
        let ch = DriverType::ClickHouse;
        assert_eq!(
            rename_relation(&mssql, &ObjectKind::Table, "dbo", "users", "people").unwrap(),
            "EXEC sp_rename N'[dbo].[users]', N'people'"
        );
        assert_eq!(
            rename_column(&mssql, "dbo", "users", "e'mail", "email"),
            "EXEC sp_rename N'[dbo].[users].[e''mail]', N'email', 'COLUMN'"
        );
        assert_eq!(
            rename_relation(&ch, &ObjectKind::Table, "db", "users", "people").unwrap(),
            "RENAME TABLE `db`.`users` TO `db`.`people`"
        );
        assert_eq!(truncate_table(&mssql, "dbo", "t", true, true), "TRUNCATE TABLE [dbo].[t]");
        assert_eq!(drop_relation(&ch, &ObjectKind::MaterializedView, "db", "mv", true), "DROP VIEW `db`.`mv`");
        let index = object(ObjectKind::Index, "ix");
        assert_eq!(
            drop_object(&mssql, &index, ConstraintKind::Other, true).unwrap(),
            "DROP INDEX [ix] ON [app].[users]"
        );
        assert_eq!(
            drop_object(&ch, &index, ConstraintKind::Other, true).unwrap(),
            "ALTER TABLE `app`.`users` DROP INDEX `ix`"
        );
        assert_eq!(drop_object(&ch, &object(ObjectKind::Trigger, "t"), ConstraintKind::Other, false), None);
        assert_eq!(
            drop_object(&ch, &object(ObjectKind::Function, "f"), ConstraintKind::Other, false).unwrap(),
            "DROP FUNCTION `f`"
        );
        let turso = DriverType::Turso;
        assert_eq!(rename_relation(&turso, &ObjectKind::View, "main", "v", "w"), None);
    }

    #[test]
    fn constraint_kinds_parse_case_blind() {
        assert_eq!(ConstraintKind::parse("primary key"), ConstraintKind::PrimaryKey);
        assert_eq!(ConstraintKind::parse("FOREIGN KEY"), ConstraintKind::ForeignKey);
        assert_eq!(ConstraintKind::parse("Unique"), ConstraintKind::Unique);
        assert_eq!(ConstraintKind::parse("CHECK"), ConstraintKind::Check);
        assert_eq!(ConstraintKind::parse("EXCLUDE"), ConstraintKind::Other);
    }
}
