use barsql_core::{DriverType, Identity, StructColumn, TableStructure};

use super::{Changes, Draft, check_clause, created_constraints, created_indexes, foreign_key_clause};
use crate::quote::{qualified_table, quote_ident, quote_ident_list, table_ref};

// MySQL can't roll DDL back, so the changes go into one ALTER TABLE that applies whole or not at all. Only dropped
// foreign keys go first, on their own: one statement can't drop a key and add one by the same name. MODIFY and
// CHANGE replace the column's whole definition, so it is written out in full.
pub(super) fn column_definition(column: &StructColumn) -> String {
    let mut out = column.data_type.clone();
    if !column.collation.is_empty() {
        out.push_str(&format!(" COLLATE {}", column.collation));
    }
    out.push_str(if column.nullable { " NULL" } else { " NOT NULL" });
    if !column.default.is_empty() {
        out.push_str(&format!(" DEFAULT {}", column.default));
    }
    if column.identity == Identity::AutoIncrement {
        out.push_str(" AUTO_INCREMENT");
    }
    if !column.extra.is_empty() {
        out.push_str(&format!(" {}", column.extra));
    }
    if !column.comment.is_empty() {
        out.push_str(&format!(" COMMENT {}", string(&column.comment)));
    }
    out
}

// Backslashes escape in MySQL's default sql_mode.
fn string(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
}

pub(super) fn alter(driver: &DriverType, original: &TableStructure, changes: &Changes) -> Vec<String> {
    let quote = |name: &str| quote_ident(driver, name);
    let mut clauses: Vec<String> =
        changes.dropped_indexes.iter().map(|index| format!("DROP INDEX {}", quote(&index.name))).collect();
    clauses.extend(changes.dropped_checks.iter().map(|check| format!("DROP CONSTRAINT {}", quote(&check.name))));
    for (old, new) in &changes.renamed {
        let column = changes.modified.iter().find(|(_, column)| &column.name == new).map(|(_, column)| column.clone());
        let column = column.or_else(|| {
            let kept = original.columns.iter().find(|column| &column.name == old)?;
            Some(StructColumn { name: new.clone(), ..kept.clone() })
        });
        if let Some(column) = column {
            clauses.push(format!("CHANGE COLUMN {} {} {}", quote(old), quote(new), column_definition(&column)));
        }
    }
    let renamed = |name: &str| changes.renamed.iter().any(|(_, new)| new == name);
    for (_, column) in changes.modified.iter().filter(|(_, column)| !renamed(&column.name)) {
        clauses.push(format!("MODIFY COLUMN {} {}", quote(&column.name), column_definition(column)));
    }
    for column in &changes.dropped {
        clauses.push(format!("DROP COLUMN {}", quote(&column.name)));
    }
    for column in &changes.added {
        clauses.push(format!("ADD COLUMN {} {}", quote(&column.name), column_definition(column)));
    }
    if let Some((old, new)) = &changes.primary_key {
        if old.is_some() {
            clauses.push("DROP PRIMARY KEY".to_string());
        }
        if let Some(key) = new {
            clauses.push(format!("ADD PRIMARY KEY ({})", quote_ident_list(driver, &key.columns)));
        }
    }
    for index in &changes.added_indexes {
        let unique = if index.unique { "UNIQUE " } else { "" };
        clauses.push(format!(
            "ADD {unique}INDEX {} ({})",
            quote(&index.name),
            quote_ident_list(driver, &index.columns)
        ));
    }
    clauses.extend(changes.added_checks.iter().map(|check| format!("ADD {}", check_clause(driver, check))));
    let keys = changes.added_foreign_keys.iter().map(|key| foreign_key_clause(driver, key, &original.schema));
    clauses.extend(keys.map(|key| format!("ADD {key}")));
    let table = table_ref(driver, &original.schema, &original.name);
    let drops: Vec<String> =
        changes.dropped_foreign_keys.iter().map(|key| format!("DROP FOREIGN KEY {}", quote(&key.name))).collect();
    [drops, clauses]
        .into_iter()
        .filter(|list| !list.is_empty())
        .map(|list| format!("ALTER TABLE {table} {}", list.join(", ")))
        .collect()
}

pub(super) fn create(driver: &DriverType, draft: &Draft) -> Vec<String> {
    let mut items: Vec<String> = draft
        .columns
        .iter()
        .filter(|row| !row.dropped)
        .map(|row| format!("{} {}", quote_ident(driver, &row.column.name), column_definition(&row.column)))
        .collect();
    let keys = draft.primary_columns();
    if !keys.is_empty() {
        items.push(format!("PRIMARY KEY ({})", quote_ident_list(driver, &keys)));
    }
    for index in created_indexes(draft) {
        let unique = if index.unique { "UNIQUE " } else { "" };
        let columns = quote_ident_list(driver, &index.columns);
        items.push(format!("{unique}INDEX {} ({columns})", quote_ident(driver, &index.name)));
    }
    items.extend(created_constraints(driver, draft));
    let table = qualified_table(driver, &draft.schema, &draft.name);
    vec![format!("CREATE TABLE {table} (\n    {}\n)", items.join(",\n    "))]
}

#[cfg(test)]
mod tests {
    use barsql_core::{DriverType, ForeignKey, Identity, RefAction, StructColumn, TableStructure};

    use crate::structure::fixtures::*;
    use crate::structure::{Draft, Row, Warning, plan_changes, plan_create};

    const MY: DriverType = DriverType::MySql;

    fn users() -> TableStructure {
        TableStructure {
            schema: "shop".into(),
            name: "users".into(),
            columns: vec![
                StructColumn { nullable: false, identity: Identity::AutoIncrement, ..column("id", "int") },
                StructColumn {
                    collation: "utf8mb4_bin".into(),
                    comment: "Display name".into(),
                    ..column("name", "varchar(50)")
                },
                StructColumn {
                    nullable: false,
                    default: "CURRENT_TIMESTAMP".into(),
                    extra: "on update CURRENT_TIMESTAMP".into(),
                    ..column("updated", "timestamp")
                },
                column("legacy", "text"),
                StructColumn { nullable: false, ..column("org_id", "int") },
            ],
            primary_key: Some(pk("", &["id"])),
            ..Default::default()
        }
    }

    #[test]
    fn mysql_combines_alterations_into_one_statement() {
        let original = users();
        let mut draft = Draft::from_structure(&original);
        draft.rename_column(1, "full_name".into());
        draft.columns[1].column.data_type = "varchar(100)".into();
        draft.columns[2].column.nullable = true;
        draft.columns[3].dropped = true;
        draft.columns.push(added(StructColumn { nullable: false, default: "0".into(), ..column("age", "int") }));
        let plan = plan_changes(&MY, &original, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "ALTER TABLE `shop`.`users` CHANGE COLUMN `name` `full_name` varchar(100) COLLATE utf8mb4_bin NULL COMMENT 'Display name', MODIFY COLUMN `updated` timestamp NULL DEFAULT CURRENT_TIMESTAMP on update CURRENT_TIMESTAMP, DROP COLUMN `legacy`, ADD COLUMN `age` int NOT NULL DEFAULT 0"
            ]
        );
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn mysql_adds_and_drops_indexes() {
        let mut original = users();
        original.indexes = vec![index("idx_users_name", &["name"], false)];
        let mut draft = Draft::from_structure(&original);
        draft.indexes[0].dropped = true;
        draft.columns.push(added(column("email", "varchar(320)")));
        draft.indexes.push(Row::added(index("idx_users_email", &["email"], true)));
        let plan = plan_changes(&MY, &original, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "ALTER TABLE `shop`.`users` DROP INDEX `idx_users_name`, ADD COLUMN `email` varchar(320) NULL, ADD UNIQUE INDEX `idx_users_email` (`email`)"
            ]
        );
    }

    #[test]
    fn mysql_adds_and_drops_checks() {
        let mut original = users();
        original.checks = vec![check("users_chk_1", "`org_id` > 0")];
        let mut draft = Draft::from_structure(&original);
        draft.checks[0].dropped = true;
        draft.checks.push(Row::added(check("users_name_check", "name <> ''")));
        draft.checks.push(Row::added(check("", "id > 0")));
        let plan = plan_changes(&MY, &original, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "ALTER TABLE `shop`.`users` DROP CONSTRAINT `users_chk_1`, ADD CONSTRAINT `users_name_check` CHECK (name <> ''), ADD CHECK (id > 0)"
            ]
        );
    }

    #[test]
    fn mysql_adds_and_drops_foreign_keys() {
        let mut original = users();
        original.foreign_keys = vec![foreign_key("fk_users_orgs", &["org_id"], "orgs", &["id"])];
        let mut draft = Draft::from_structure(&original);
        draft.foreign_keys[0].dropped = true;
        let key = ForeignKey {
            on_delete: RefAction::Cascade,
            ..foreign_key("fk_users_teams", &["org_id"], "teams", &["id"])
        };
        draft.foreign_keys.push(Row::added(key));
        let plan = plan_changes(&MY, &original, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "ALTER TABLE `shop`.`users` DROP FOREIGN KEY `fk_users_orgs`",
                "ALTER TABLE `shop`.`users` ADD CONSTRAINT `fk_users_teams` FOREIGN KEY (`org_id`) REFERENCES `shop`.`teams` (`id`) ON DELETE CASCADE",
            ]
        );
    }

    #[test]
    fn mysql_drops_foreign_keys_in_their_own_statement() {
        let mut original = users();
        original.foreign_keys = vec![foreign_key("fk_users_orgs", &["org_id"], "orgs", &["id"])];
        let mut draft = Draft::from_structure(&original);
        draft.foreign_keys[0].value.on_delete = RefAction::Cascade;
        let plan = plan_changes(&MY, &original, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "ALTER TABLE `shop`.`users` DROP FOREIGN KEY `fk_users_orgs`",
                "ALTER TABLE `shop`.`users` ADD CONSTRAINT `fk_users_orgs` FOREIGN KEY (`org_id`) REFERENCES `shop`.`orgs` (`id`) ON DELETE CASCADE",
            ]
        );
        assert_eq!(plan.warnings, [Warning::NotAtomic]);
    }

    #[test]
    fn mysql_creates_a_table_with_its_indexes_checks_and_keys() {
        let mut draft = Draft::new_table(&MY, "shop");
        draft.name = "people".into();
        draft.columns.push(added(column("org_id", "bigint")));
        draft.checks.push(Row::added(check("people_id_check", "id > 0")));
        draft.foreign_keys.push(Row::added(foreign_key("fk_people_orgs", &["org_id"], "orgs", &["id"])));
        draft.indexes.push(Row::added(index("idx_people_org_id", &["org_id"], false)));
        let plan = plan_create(&MY, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "CREATE TABLE `shop`.`people` (\n    `id` BIGINT NOT NULL AUTO_INCREMENT,\n    `org_id` bigint NULL,\n    PRIMARY KEY (`id`),\n    INDEX `idx_people_org_id` (`org_id`),\n    CONSTRAINT `people_id_check` CHECK (id > 0),\n    CONSTRAINT `fk_people_orgs` FOREIGN KEY (`org_id`) REFERENCES `shop`.`orgs` (`id`)\n)"
            ]
        );
    }

    #[test]
    fn mysql_changes_the_primary_key() {
        let original = users();
        let mut draft = Draft::from_structure(&original);
        draft.columns[4].primary = true;
        let plan = plan_changes(&MY, &original, &draft).expect("valid");
        let steps = sql(&plan);
        assert_eq!(steps.len(), 1);
        assert!(steps[0].ends_with("DROP PRIMARY KEY, ADD PRIMARY KEY (`id`, `org_id`)"), "{steps:?}");
    }

    #[test]
    fn mysql_quotes_backticks() {
        let mut original = users();
        original.columns.push(column("a`b", "int"));
        let mut draft = Draft::from_structure(&original);
        draft.rename_column(5, "ab".into());
        let plan = plan_changes(&MY, &original, &draft).expect("valid");
        assert!(sql(&plan)[0].contains("CHANGE COLUMN `a``b` `ab` int NULL"), "{:?}", sql(&plan));
    }

    #[test]
    fn mysql_creates_a_table() {
        let mut draft = Draft::new_table(&MY, "shop");
        draft.name = "people".into();
        let plan = plan_create(&MY, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            ["CREATE TABLE `shop`.`people` (\n    `id` BIGINT NOT NULL AUTO_INCREMENT,\n    PRIMARY KEY (`id`)\n)"]
        );
    }
}
