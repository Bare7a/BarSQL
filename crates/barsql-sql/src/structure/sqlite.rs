use std::collections::{HashMap, HashSet};

use barsql_core::{DriverType, Identity, StructColumn, TableStructure};

use super::create_sql::{column_item, item_column, split_create_table};
use super::{Changes, Draft, Step, Warning};
use crate::alter::{drop_column, rename_column};
use crate::lang::tokens::{TokenKind, tokenize};
use crate::quote::{quote_ident, quote_ident_list, table_ref};

const LITE: DriverType = DriverType::Sqlite;
// Kept from a column's original definition when the column itself is rendered again.
const KEPT_CLAUSES: [&str; 3] = ["unique", "check", "references"];

// None when a rebuild is needed but the table's CREATE TABLE can't be read.
pub(super) fn alter(
    driver: &DriverType,
    original: &TableStructure,
    changes: &Changes,
    draft: &Draft,
) -> Option<(Vec<Step>, Vec<Warning>)> {
    if plain(original, changes, draft) {
        let (schema, name) = (original.schema.as_str(), original.name.as_str());
        let table = table_ref(driver, schema, name);
        let mut steps = vec![Step::sql("BEGIN")];
        steps.extend(changes.renamed.iter().map(|(old, new)| Step::sql(rename_column(driver, schema, name, old, new))));
        steps.extend(changes.dropped.iter().map(|c| Step::sql(drop_column(driver, schema, name, &c.name, false))));
        let added =
            changes.added.iter().map(|c| format!("ALTER TABLE {table} ADD COLUMN {}", render(c, &c.name, false, &[])));
        steps.extend(added.map(Step::sql));
        steps.push(Step::sql("COMMIT"));
        return Some((steps, Vec::new()));
    }
    rebuild(driver, original, changes, draft).map(|steps| (steps, vec![Warning::Rebuild]))
}

pub(super) fn create(driver: &DriverType, draft: &Draft) -> Vec<String> {
    let keys = draft.primary_columns();
    let live = draft.columns.iter().filter(|row| !row.dropped).map(|row| &row.column);
    let mut items: Vec<String> = live.map(|c| render(c, &c.name, keys.len() == 1 && keys[0] == c.name, &[])).collect();
    if keys.len() > 1 {
        items.push(format!("PRIMARY KEY ({})", quote_ident_list(driver, &keys)));
    }
    let table = table_ref(driver, &draft.schema, &draft.name);
    vec![format!("CREATE TABLE {table} (\n    {}\n)", items.join(",\n    "))]
}

fn render(column: &StructColumn, name: &str, inline_key: bool, kept: &[&str]) -> String {
    let mut out = quote_ident(&LITE, name);
    if !column.data_type.is_empty() {
        out.push_str(&format!(" {}", column.data_type));
    }
    if inline_key {
        out.push_str(" PRIMARY KEY");
        if column.identity == Identity::AutoIncrement {
            out.push_str(" AUTOINCREMENT");
        }
    }
    if !column.nullable {
        out.push_str(" NOT NULL");
    }
    if !column.default.is_empty() {
        out.push_str(&format!(" DEFAULT {}", column.default));
    }
    if !column.collation.is_empty() {
        out.push_str(&format!(" COLLATE {}", column.collation));
    }
    for clause in kept {
        out.push(' ');
        out.push_str(clause);
    }
    out
}

// ALTER TABLE ADD COLUMN takes only a constant default.
fn literal_default(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || ["null", "true", "false"].iter().any(|word| text.eq_ignore_ascii_case(word)) {
        return true;
    }
    let tokens = tokenize(text, Some(&LITE));
    match tokens.as_slice() {
        [one] => matches!(one.kind, TokenKind::String | TokenKind::Number) && !one.unterminated,
        [sign, number] => matches!(sign.text, "-" | "+") && number.kind == TokenKind::Number,
        _ => false,
    }
}

fn names_in(expression: &str) -> HashSet<String> {
    let tokens = tokenize(expression, Some(&LITE));
    let names = tokens.iter().filter(|t| matches!(t.kind, TokenKind::Ident | TokenKind::Quoted));
    names.map(|t| t.ident_text().to_lowercase()).collect()
}

fn plain(original: &TableStructure, changes: &Changes, draft: &Draft) -> bool {
    let shaped = changes.modified.is_empty()
        && changes.primary_key.is_none()
        && changes.dropped_foreign_keys.is_empty()
        && changes.added_foreign_keys.is_empty()
        && changes.dropped_checks.is_empty()
        && changes.added_checks.is_empty()
        && changes.dropped_indexes.iter().all(|index| !index.constraint);
    let addable = |c: &StructColumn| {
        literal_default(&c.default)
            && (c.nullable || !c.default.trim().is_empty())
            && c.identity == Identity::None
            && c.generated.is_empty()
    };
    let key = original.primary_key.as_ref().map(|key| key.columns.as_slice()).unwrap_or_default();
    let mut used: HashSet<String> = key.iter().map(|c| c.to_lowercase()).collect();
    let undropped_indexes = draft.indexes.iter().filter(|row| !row.dropped).map(|row| &row.value.columns);
    let constraint_indexes = original.indexes.iter().filter(|index| index.constraint).map(|index| &index.columns);
    let key_columns = original.foreign_keys.iter().map(|key| &key.columns);
    for columns in undropped_indexes.chain(constraint_indexes).chain(key_columns) {
        used.extend(columns.iter().map(|c| c.to_lowercase()));
    }
    for check in &original.checks {
        used.extend(names_in(&check.expression));
    }
    shaped
        && changes.added.iter().all(addable)
        && changes.dropped.iter().all(|c| !used.contains(&c.name.to_lowercase()))
}

fn starts_primary_key(item: &str) -> bool {
    let tokens = tokenize(item, Some(&LITE));
    let mut words = tokens.iter().filter(|t| t.kind != TokenKind::Comment);
    match words.next() {
        Some(first) if first.lower == "primary" => true,
        Some(first) if first.lower == "constraint" => words.nth(1).is_some_and(|kind| kind.lower == "primary"),
        _ => false,
    }
}

// SQLite's 12-step rebuild. Renamed columns keep their old names until the last step renames them, so SQLite itself
// rewrites the indexes, triggers and checks that use them.
fn rebuild(driver: &DriverType, original: &TableStructure, changes: &Changes, draft: &Draft) -> Option<Vec<Step>> {
    let split = split_create_table(&original.create_sql)?;
    let (schema, name) = (original.schema.as_str(), original.name.as_str());
    let (table, new_table) = (table_ref(driver, schema, name), quote_ident(driver, &format!("barsql_new_{name}")));
    let back: HashMap<&str, &str> = changes.renamed.iter().map(|(old, new)| (new.as_str(), old.as_str())).collect();
    let old_name = |current: &str| back.get(current).map_or_else(|| current.to_string(), |old| old.to_string());

    let dropped: HashSet<String> = changes.dropped.iter().map(|c| c.name.to_lowercase()).collect();
    let modified: HashSet<String> = changes.modified.iter().map(|(old, _)| old.name.to_lowercase()).collect();
    let new_key: Vec<String> = draft.primary_columns().iter().map(|c| old_name(c)).collect();
    let key_changed = changes.primary_key.is_some();
    let mut key_members: HashSet<String> = HashSet::new();
    if key_changed {
        let old_key = original.primary_key.as_ref().map(|key| key.columns.clone()).unwrap_or_default();
        key_members.extend(old_key.iter().chain(&new_key).map(|c| c.to_lowercase()));
    }
    let inline_key = |column: &str| key_changed && new_key.len() == 1 && new_key[0].eq_ignore_ascii_case(column);
    let current: HashMap<String, &StructColumn> = draft
        .columns
        .iter()
        .filter_map(|row| Some((original.columns.get(row.origin?)?.name.to_lowercase(), &row.column)))
        .collect();

    let mut items: Vec<String> = Vec::new();
    let mut after_columns = 0;
    let mut key_placed = !key_changed || new_key.len() < 2;
    for item in &split.items {
        let Some(column) = item_column(item) else {
            if key_changed && starts_primary_key(item) {
                if !key_placed {
                    items.push(format!("PRIMARY KEY ({})", quote_ident_list(driver, &new_key)));
                    key_placed = true;
                }
                continue;
            }
            items.push(item.to_string());
            continue;
        };
        let lower = column.to_lowercase();
        if dropped.contains(&lower) {
            continue;
        }
        if modified.contains(&lower) || key_members.contains(&lower) {
            let new = current.get(&lower).copied()?;
            let parsed = column_item(item);
            let kept: Vec<&str> = parsed
                .iter()
                .flat_map(|parsed| parsed.clauses.iter())
                .filter(|(kind, _)| KEPT_CLAUSES.contains(&kind.as_str()))
                .map(|(_, text)| *text)
                .collect();
            items.push(render(new, &column, inline_key(&column), &kept));
        } else {
            items.push(item.to_string());
        }
        after_columns = items.len();
    }
    let added: Vec<String> = changes.added.iter().map(|c| render(c, &c.name, inline_key(&c.name), &[])).collect();
    items.splice(after_columns..after_columns, added);
    if !key_placed {
        items.push(format!("PRIMARY KEY ({})", quote_ident_list(driver, &new_key)));
    }

    let copied: Vec<String> = original
        .columns
        .iter()
        .filter(|c| !dropped.contains(&c.name.to_lowercase()) && c.generated.is_empty())
        .map(|c| c.name.clone())
        .collect();
    let copied = quote_ident_list(driver, &copied);
    let skipped: HashSet<&str> = changes.dropped_indexes.iter().map(|index| index.name.as_str()).collect();

    let mut steps = vec![Step::sql("PRAGMA foreign_keys = OFF"), Step::sql("BEGIN")];
    steps.push(Step::sql(format!("CREATE TABLE {new_table} (\n    {}\n){}", items.join(",\n    "), split.options)));
    steps.push(Step::sql(format!("INSERT INTO {new_table} ({copied}) SELECT {copied} FROM {table}")));
    steps.push(Step::sql("PRAGMA legacy_alter_table = ON"));
    steps.push(Step::sql(format!("DROP TABLE {table}")));
    steps.push(Step::sql(format!("ALTER TABLE {new_table} RENAME TO {}", quote_ident(driver, name))));
    steps.push(Step::sql("PRAGMA legacy_alter_table = OFF"));
    let dependents = original.dependents.iter().filter(|d| !(d.kind == "index" && skipped.contains(d.name.as_str())));
    steps.extend(dependents.map(|dependent| Step::sql(dependent.sql.clone())));
    steps.extend(changes.renamed.iter().map(|(old, new)| Step::sql(rename_column(driver, schema, name, old, new))));
    let check = format!("PRAGMA foreign_key_check({})", quote_ident(driver, name));
    steps.push(Step { sql: check, expect_no_rows: true });
    steps.push(Step::sql("COMMIT"));
    steps.push(Step::sql("PRAGMA foreign_keys = ON"));
    Some(steps)
}

#[cfg(test)]
mod tests {
    use barsql_core::{Check, Dependent, DriverType, Identity, StructColumn, StructIndex, TableStructure};

    use crate::structure::fixtures::*;
    use crate::structure::{Draft, Warning, plan_changes, plan_create};

    const LITE: DriverType = DriverType::Sqlite;

    fn table(name: &str, create_sql: &str, columns: Vec<StructColumn>, key: &[&str]) -> TableStructure {
        TableStructure {
            schema: "main".into(),
            name: name.into(),
            columns,
            primary_key: (!key.is_empty()).then(|| pk("", key)),
            create_sql: create_sql.into(),
            ..Default::default()
        }
    }

    fn simple() -> TableStructure {
        table(
            "users",
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, legacy TEXT)",
            vec![column("id", "INTEGER"), column("name", "TEXT"), column("legacy", "TEXT")],
            &["id"],
        )
    }

    #[test]
    fn sqlite_uses_plain_alters_when_it_can() {
        let original = simple();
        let mut draft = Draft::from_structure(&original);
        draft.rename_column(1, "full_name".into());
        draft.columns[2].dropped = true;
        draft.columns.push(added(StructColumn { default: "0".into(), ..column("age", "INTEGER") }));
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "BEGIN",
                r#"ALTER TABLE "users" RENAME COLUMN "name" TO "full_name""#,
                r#"ALTER TABLE "users" DROP COLUMN "legacy""#,
                r#"ALTER TABLE "users" ADD COLUMN "age" INTEGER DEFAULT 0"#,
                "COMMIT",
            ]
        );
        assert!(plan.warnings.is_empty());
    }

    const REBUILT: &str = r#"CREATE TABLE "users" (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT COLLATE NOCASE /* shown in lists */,
  age TEXT,
  total REAL GENERATED ALWAYS AS (age * 2) STORED,
  CHECK (length(name) > 0)
) STRICT"#;

    fn rebuilt() -> TableStructure {
        let mut original = table(
            "users",
            REBUILT,
            vec![
                StructColumn { identity: Identity::AutoIncrement, ..column("id", "INTEGER") },
                StructColumn { collation: "NOCASE".into(), ..column("name", "TEXT") },
                column("age", "TEXT"),
                StructColumn { generated: "age * 2".into(), ..column("total", "REAL") },
            ],
            &["id"],
        );
        original.checks = vec![Check { name: String::new(), expression: "length(name) > 0".into() }];
        original.indexes =
            vec![StructIndex { name: "users_name".into(), columns: vec!["name".into()], ..Default::default() }];
        let dependent =
            |kind: &str, name: &str, sql: &str| Dependent { kind: kind.into(), name: name.into(), sql: sql.into() };
        original.dependents = vec![
            dependent("index", "users_name", "CREATE INDEX users_name ON users (name)"),
            dependent("trigger", "users_touch", "CREATE TRIGGER users_touch AFTER UPDATE ON users BEGIN SELECT 1; END"),
        ];
        original
    }

    #[test]
    fn sqlite_rebuilds_and_keeps_what_it_does_not_change() {
        let original = rebuilt();
        let mut draft = Draft::from_structure(&original);
        draft.columns[2].column.data_type = "INTEGER".into();
        draft.rename_column(1, "full_name".into());
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(plan.warnings, [Warning::Rebuild]);
        assert_eq!(
            sql(&plan),
            [
                "PRAGMA foreign_keys = OFF",
                "BEGIN",
                "CREATE TABLE \"barsql_new_users\" (\n    id INTEGER PRIMARY KEY AUTOINCREMENT,\n    name TEXT COLLATE NOCASE /* shown in lists */,\n    \"age\" INTEGER,\n    total REAL GENERATED ALWAYS AS (age * 2) STORED,\n    CHECK (length(name) > 0)\n) STRICT",
                r#"INSERT INTO "barsql_new_users" ("id", "name", "age") SELECT "id", "name", "age" FROM "users""#,
                "PRAGMA legacy_alter_table = ON",
                r#"DROP TABLE "users""#,
                r#"ALTER TABLE "barsql_new_users" RENAME TO "users""#,
                "PRAGMA legacy_alter_table = OFF",
                "CREATE INDEX users_name ON users (name)",
                "CREATE TRIGGER users_touch AFTER UPDATE ON users BEGIN SELECT 1; END",
                r#"ALTER TABLE "users" RENAME COLUMN "name" TO "full_name""#,
                r#"PRAGMA foreign_key_check("users")"#,
                "COMMIT",
                "PRAGMA foreign_keys = ON",
            ]
        );
        let checked: Vec<&str> = plan.steps.iter().filter(|s| s.expect_no_rows).map(|s| s.sql.as_str()).collect();
        assert_eq!(checked, [r#"PRAGMA foreign_key_check("users")"#]);
    }

    #[test]
    fn sqlite_drops_a_keyed_column_by_rebuild() {
        let original = table(
            "memberships",
            "CREATE TABLE memberships (user_id INTEGER, org_id INTEGER, role TEXT, PRIMARY KEY (user_id, org_id))",
            vec![column("user_id", "INTEGER"), column("org_id", "INTEGER"), column("role", "TEXT")],
            &["user_id", "org_id"],
        );
        let mut draft = Draft::from_structure(&original);
        draft.columns[1].dropped = true;
        draft.columns[1].primary = false;
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(plan.warnings, [Warning::Rebuild]);
        let steps = sql(&plan);
        assert_eq!(
            steps[2],
            "CREATE TABLE \"barsql_new_memberships\" (\n    \"user_id\" INTEGER PRIMARY KEY,\n    role TEXT\n)"
        );
        assert_eq!(
            steps[3],
            r#"INSERT INTO "barsql_new_memberships" ("user_id", "role") SELECT "user_id", "role" FROM "memberships""#
        );
    }

    #[test]
    fn sqlite_rebuild_keeps_inline_constraints_of_changed_columns() {
        let original = table(
            "pets",
            "CREATE TABLE pets (id INTEGER PRIMARY KEY, owner_id INTEGER CONSTRAINT owner_fk REFERENCES people(id) ON DELETE SET NULL, tag TEXT UNIQUE NOT NULL DEFAULT 'x')",
            vec![
                column("id", "INTEGER"),
                column("owner_id", "INTEGER"),
                StructColumn { nullable: false, default: "'x'".into(), ..column("tag", "TEXT") },
            ],
            &["id"],
        );
        let mut draft = Draft::from_structure(&original);
        draft.columns[1].column.data_type = "BIGINT".into();
        draft.columns[2].column.default = "'y'".into();
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(
            sql(&plan)[2],
            "CREATE TABLE \"barsql_new_pets\" (\n    id INTEGER PRIMARY KEY,\n    \"owner_id\" BIGINT CONSTRAINT owner_fk REFERENCES people(id) ON DELETE SET NULL,\n    \"tag\" TEXT NOT NULL DEFAULT 'y' UNIQUE\n)"
        );
    }

    #[test]
    fn sqlite_defaults_with_commas_survive() {
        let original = simple();
        let mut draft = Draft::from_structure(&original);
        draft.columns.push(added(StructColumn { default: "'a,b'".into(), ..column("tag", "TEXT") }));
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(sql(&plan), ["BEGIN", r#"ALTER TABLE "users" ADD COLUMN "tag" TEXT DEFAULT 'a,b'"#, "COMMIT"]);

        let mut draft = Draft::from_structure(&original);
        let created =
            StructColumn { nullable: false, default: "(datetime('now'))".into(), ..column("created", "TEXT") };
        draft.columns.push(added(created));
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(plan.warnings, [Warning::Rebuild]);
        assert!(sql(&plan)[2].contains(r#""created" TEXT NOT NULL DEFAULT (datetime('now'))"#), "{:?}", sql(&plan));
    }

    #[test]
    fn sqlite_quotes_awkward_names() {
        let mut original = simple();
        original.columns.push(column("we\"ird", "TEXT"));
        original.create_sql =
            r#"CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, legacy TEXT, "we""ird" TEXT)"#.into();
        let mut draft = Draft::from_structure(&original);
        draft.rename_column(3, "Mixed Case".into());
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(sql(&plan)[1], r#"ALTER TABLE "users" RENAME COLUMN "we""ird" TO "Mixed Case""#);
    }

    #[test]
    fn sqlite_creates_a_table() {
        let mut draft = Draft::new_table(&LITE, "main");
        draft.name = "people".into();
        let plan = plan_create(&LITE, &draft).expect("valid");
        assert_eq!(sql(&plan), ["CREATE TABLE \"people\" (\n    \"id\" INTEGER PRIMARY KEY NOT NULL\n)"]);
    }
}
