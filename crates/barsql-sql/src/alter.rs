use barsql_core::{DriverType, ObjectKind, ObjectRef};

use crate::quote::{quote_ident, table_ref};

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
    if on && *driver == DriverType::Postgres { " CASCADE" } else { "" }
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
    Some(match (driver, kind) {
        (DriverType::MySql, _) => format!("RENAME TABLE {from} TO {}", table_ref(driver, schema, new_name)),
        (DriverType::Sqlite, ObjectKind::Table) => format!("ALTER TABLE {from} RENAME TO {to}"),
        (DriverType::Sqlite, _) => return None,
        (_, ObjectKind::View) => format!("ALTER VIEW {from} RENAME TO {to}"),
        (_, ObjectKind::MaterializedView) => format!("ALTER MATERIALIZED VIEW {from} RENAME TO {to}"),
        _ => format!("ALTER TABLE {from} RENAME TO {to}"),
    })
}

pub fn drop_relation(driver: &DriverType, kind: &ObjectKind, schema: &str, name: &str, cascades: bool) -> String {
    let what = match kind {
        ObjectKind::View => "VIEW",
        ObjectKind::MaterializedView if *driver == DriverType::Postgres => "MATERIALIZED VIEW",
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
    match driver {
        DriverType::Sqlite => format!("DELETE FROM {target}"),
        DriverType::Postgres => {
            let restart = if restart_identity { " RESTART IDENTITY" } else { "" };
            format!("TRUNCATE TABLE {target}{restart}{}", cascade(driver, cascades))
        }
        _ => format!("TRUNCATE TABLE {target}"),
    }
}

pub fn rename_column(driver: &DriverType, schema: &str, table: &str, column: &str, new_name: &str) -> String {
    format!(
        "ALTER TABLE {} RENAME COLUMN {} TO {}",
        table_ref(driver, schema, table),
        quote_ident(driver, column),
        quote_ident(driver, new_name)
    )
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
    let tail = cascade(driver, cascades);
    let sql = match (&object.kind, driver) {
        (ObjectKind::Index, DriverType::Sqlite) if object.name.starts_with("sqlite_autoindex_") => return None,
        (ObjectKind::Index, DriverType::MySql) => format!("DROP INDEX {name} ON {table}"),
        (ObjectKind::Index, _) => format!("DROP INDEX {}{tail}", table_ref(driver, &object.schema, &object.name)),
        (ObjectKind::Constraint, DriverType::Sqlite) => return None,
        (ObjectKind::Constraint, _) if object.name.is_empty() => return None,
        (ObjectKind::Constraint, DriverType::MySql) => match constraint {
            ConstraintKind::PrimaryKey => format!("ALTER TABLE {table} DROP PRIMARY KEY"),
            ConstraintKind::ForeignKey => format!("ALTER TABLE {table} DROP FOREIGN KEY {name}"),
            ConstraintKind::Unique => format!("ALTER TABLE {table} DROP INDEX {name}"),
            ConstraintKind::Check | ConstraintKind::Other => format!("ALTER TABLE {table} DROP CONSTRAINT {name}"),
        },
        (ObjectKind::Constraint, _) => format!("ALTER TABLE {table} DROP CONSTRAINT {name}{tail}"),
        (ObjectKind::Trigger, DriverType::Postgres) => format!("DROP TRIGGER {name} ON {table}{tail}"),
        (ObjectKind::Trigger, _) => format!("DROP TRIGGER {}", table_ref(driver, &object.schema, &object.name)),
        (ObjectKind::Function | ObjectKind::Procedure, DriverType::Sqlite) => return None,
        (ObjectKind::Function | ObjectKind::Procedure, _) => {
            let what = if object.kind == ObjectKind::Procedure { "PROCEDURE" } else { "FUNCTION" };
            let routine = table_ref(driver, &object.schema, &object.name);
            match driver {
                DriverType::Postgres => format!("DROP {what} {routine}({}){tail}", object.args),
                _ => format!("DROP {what} {routine}"),
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
    fn constraint_kinds_parse_case_blind() {
        assert_eq!(ConstraintKind::parse("primary key"), ConstraintKind::PrimaryKey);
        assert_eq!(ConstraintKind::parse("FOREIGN KEY"), ConstraintKind::ForeignKey);
        assert_eq!(ConstraintKind::parse("Unique"), ConstraintKind::Unique);
        assert_eq!(ConstraintKind::parse("CHECK"), ConstraintKind::Check);
        assert_eq!(ConstraintKind::parse("EXCLUDE"), ConstraintKind::Other);
    }
}
