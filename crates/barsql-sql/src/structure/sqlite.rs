use std::collections::{HashMap, HashSet};

use barsql_core::{Check, Dependent, DriverType, ForeignKey, Identity, StructColumn, StructIndex, TableStructure};

use super::create_sql::{column_item, item_column, names_in, paren_body, split_create_table};
use super::{Changes, Draft, Step, Warning, check_clause, created_constraints, created_indexes, foreign_key_clause};
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
        let dropped = changes
            .dropped_indexes
            .iter()
            .map(|index| format!("DROP INDEX {}", table_ref(driver, schema, &index.name)));
        steps.extend(dropped.map(Step::sql));
        steps.extend(changes.renamed.iter().map(|(old, new)| Step::sql(rename_column(driver, schema, name, old, new))));
        steps.extend(changes.dropped.iter().map(|c| Step::sql(drop_column(driver, schema, name, &c.name, false))));
        let added =
            changes.added.iter().map(|c| format!("ALTER TABLE {table} ADD COLUMN {}", render(c, &c.name, false, &[])));
        steps.extend(added.map(Step::sql));
        steps.extend(changes.added_indexes.iter().map(|index| Step::sql(create_index(driver, schema, name, index))));
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
    items.extend(created_constraints(driver, draft));
    let table = table_ref(driver, &draft.schema, &draft.name);
    let create = format!("CREATE TABLE {table} (\n    {}\n)", items.join(",\n    "));
    let indexes = created_indexes(draft).map(|index| create_index(driver, &draft.schema, &draft.name, index));
    std::iter::once(create).chain(indexes).collect()
}

fn create_index(driver: &DriverType, schema: &str, table: &str, index: &StructIndex) -> String {
    let unique = if index.unique { "UNIQUE " } else { "" };
    let (name, on) = (table_ref(driver, schema, &index.name), quote_ident(driver, table));
    format!("CREATE {unique}INDEX {name} ON {on} ({})", quote_ident_list(driver, &index.columns))
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

// A table constraint item of this kind, like PRIMARY KEY (…) or CONSTRAINT name UNIQUE (…).
fn starts_constraint(item: &str, kind: &str) -> bool {
    let tokens = tokenize(item, Some(&LITE));
    let mut words = tokens.iter().filter(|t| t.kind != TokenKind::Comment);
    match words.next() {
        Some(first) if first.lower == kind => true,
        Some(first) if first.lower == "constraint" => words.nth(1).is_some_and(|next| next.lower == kind),
        _ => false,
    }
}

// The columns a UNIQUE (…) or FOREIGN KEY (…) item names, lowercased.
fn listed_columns(item: &str, kind: &str) -> Option<Vec<String>> {
    if !starts_constraint(item, kind) {
        return None;
    }
    let first_name = |part: &str| {
        let tokens = tokenize(part, Some(&LITE));
        let name = tokens.iter().find(|t| matches!(t.kind, TokenKind::Ident | TokenKind::Quoted))?;
        Some(name.ident_text().to_lowercase())
    };
    paren_body(item)?.split(',').map(first_name).collect()
}

// The table after REFERENCES, lowercased.
fn referenced(text: &str) -> Option<String> {
    let tokens = tokenize(text, Some(&LITE));
    let mut code = tokens.iter().filter(|t| t.kind != TokenKind::Comment);
    code.find(|t| t.lower == "references")?;
    let table = code.next().filter(|t| matches!(t.kind, TokenKind::Ident | TokenKind::Quoted))?;
    Some(table.ident_text().to_lowercase())
}

// Takes one entry equal to `wanted` off the list.
fn take<T: PartialEq<U>, U: ?Sized>(list: &mut Vec<T>, wanted: &U) -> bool {
    let found = list.iter().position(|listed| listed == wanted);
    found.map(|at| list.remove(at)).is_some()
}

// A rebuild writes the table with the old column names and renames them last, so a new check names them so too. A
// name followed by `(` is a function's.
fn under_old_names(expression: &str, back: &HashMap<&str, &str>) -> String {
    let tokens = tokenize(expression, Some(&LITE));
    let code: Vec<_> = tokens.iter().filter(|t| t.kind != TokenKind::Comment).collect();
    let (mut out, mut at) = (String::new(), 0);
    for (ix, token) in code.iter().enumerate() {
        let called = code.get(ix + 1).is_some_and(|next| next.is_punct("("));
        if !matches!(token.kind, TokenKind::Ident | TokenKind::Quoted) || called {
            continue;
        }
        let name = token.ident_text();
        let Some(old) = back.iter().find(|(new, _)| new.eq_ignore_ascii_case(&name)).map(|(_, old)| *old) else {
            continue;
        };
        out.push_str(&expression[at..token.start]);
        out.push_str(&quote_ident(&LITE, old));
        at = token.end;
    }
    out.push_str(&expression[at..]);
    out
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

    // Dropped unique constraints and checks are part of the definition, inline on a column or items of their own.
    let mut unconstrained: Vec<Vec<String>> = changes
        .dropped_indexes
        .iter()
        .filter(|index| index.constraint)
        .map(|index| index.columns.iter().map(|c| c.to_lowercase()).collect())
        .collect();
    let mut unchecked: Vec<String> =
        changes.dropped_checks.iter().map(|check| check.expression.trim().into()).collect();
    let lower = |names: &[String]| names.iter().map(|name| name.to_lowercase()).collect::<Vec<_>>();
    let mut unreferenced: Vec<(Vec<String>, String)> =
        changes.dropped_foreign_keys.iter().map(|key| (lower(&key.columns), key.ref_table.to_lowercase())).collect();
    let mut items: Vec<String> = Vec::new();
    let mut after_columns = 0;
    let mut key_placed = !key_changed || new_key.len() < 2;
    for item in &split.items {
        let Some(column) = item_column(item) else {
            if key_changed && starts_constraint(item, "primary") {
                if !key_placed {
                    items.push(format!("PRIMARY KEY ({})", quote_ident_list(driver, &new_key)));
                    key_placed = true;
                }
                continue;
            }
            let unique =
                listed_columns(item, "unique").is_some_and(|columns| take(&mut unconstrained, columns.as_slice()));
            let key = listed_columns(item, "foreign")
                .zip(referenced(item))
                .is_some_and(|wanted| take(&mut unreferenced, &wanted));
            let check =
                starts_constraint(item, "check") && paren_body(item).is_some_and(|body| take(&mut unchecked, body));
            if !unique && !check && !key {
                items.push(item.to_string());
            }
            continue;
        };
        let lower = column.to_lowercase();
        let parsed = column_item(item);
        let clauses: Vec<(&str, &str)> =
            parsed.iter().flat_map(|parsed| parsed.clauses.iter()).map(|(kind, text)| (kind.as_str(), *text)).collect();
        // The clauses of dropped constraints. A dropped column takes them along.
        let mut gone: Vec<&str> = Vec::new();
        if clauses.iter().any(|(kind, _)| *kind == "unique") && take(&mut unconstrained, std::slice::from_ref(&lower)) {
            gone.extend(clauses.iter().filter(|(kind, _)| *kind == "unique").map(|(_, text)| *text));
        }
        for (kind, text) in &clauses {
            let found = match *kind {
                "check" => paren_body(text).is_some_and(|body| take(&mut unchecked, body)),
                "references" => {
                    referenced(text).is_some_and(|table| take(&mut unreferenced, &(vec![lower.clone()], table)))
                }
                _ => false,
            };
            if found {
                gone.push(text);
            }
        }
        if dropped.contains(&lower) {
            continue;
        }
        if modified.contains(&lower) || key_members.contains(&lower) || !gone.is_empty() {
            let new = current.get(&lower).copied()?;
            let kept: Vec<&str> = clauses
                .iter()
                .filter(|(kind, text)| KEPT_CLAUSES.contains(kind) && !gone.contains(text))
                .map(|(_, text)| *text)
                .collect();
            items.push(render(new, &column, inline_key(&column), &kept));
        } else {
            items.push(item.to_string());
        }
        after_columns = items.len();
    }
    // A dropped constraint the definition doesn't show can't be taken out of it.
    if !unconstrained.is_empty() || !unchecked.is_empty() || !unreferenced.is_empty() {
        return None;
    }
    let added: Vec<String> = changes.added.iter().map(|c| render(c, &c.name, inline_key(&c.name), &[])).collect();
    items.splice(after_columns..after_columns, added);
    if !key_placed {
        items.push(format!("PRIMARY KEY ({})", quote_ident_list(driver, &new_key)));
    }
    items.extend(changes.added_checks.iter().map(|check| {
        let check = Check { expression: under_old_names(&check.expression, &back), ..check.clone() };
        check_clause(driver, &check)
    }));
    // A key names this table's columns by their old names. One that refers to this table names its own columns so.
    items.extend(changes.added_foreign_keys.iter().map(|key| {
        let old = |names: &[String]| names.iter().map(|name| old_name(name)).collect();
        let own = key.ref_table.eq_ignore_ascii_case(name);
        let ref_columns = if own { old(&key.ref_columns) } else { key.ref_columns.clone() };
        foreign_key_clause(driver, &ForeignKey { columns: old(&key.columns), ref_columns, ..key.clone() }, schema)
    }));

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
    // While the table is briefly gone, renaming the copy in trips over every view and trigger that names it. SQLite
    // skips that check under legacy_alter_table. Turso's server won't set it, so those objects go and come back.
    let turso = *driver == DriverType::Turso;
    let referring: Vec<&Dependent> = match turso {
        true => {
            original.dependents.iter().filter(|d| matches!(d.kind.as_str(), "view" | "referring trigger")).collect()
        }
        false => Vec::new(),
    };
    if !turso {
        steps.push(Step::sql("PRAGMA legacy_alter_table = ON"));
    }
    for dependent in referring.iter().rev() {
        let what = if dependent.kind == "view" { "VIEW" } else { "TRIGGER" };
        steps.push(Step::sql(format!("DROP {what} {}", quote_ident(driver, &dependent.name))));
    }
    steps.push(Step::sql(format!("DROP TABLE {table}")));
    steps.push(Step::sql(format!("ALTER TABLE {new_table} RENAME TO {}", quote_ident(driver, name))));
    if !turso {
        steps.push(Step::sql("PRAGMA legacy_alter_table = OFF"));
    }
    let own = original.dependents.iter().filter(|d| match d.kind.as_str() {
        "index" => !skipped.contains(d.name.as_str()),
        kind => kind == "trigger",
    });
    steps.extend(own.chain(referring.iter().copied()).map(|dependent| Step::sql(dependent.sql.clone())));
    steps.extend(changes.renamed.iter().map(|(old, new)| Step::sql(rename_column(driver, schema, name, old, new))));
    steps.extend(changes.added_indexes.iter().map(|index| Step::sql(create_index(driver, schema, name, index))));
    let check = format!("PRAGMA foreign_key_check({})", quote_ident(driver, name));
    steps.push(Step { sql: check, expect_no_rows: true });
    steps.push(Step::sql("COMMIT"));
    steps.push(Step::sql("PRAGMA foreign_keys = ON"));
    Some(steps)
}

#[cfg(test)]
mod tests {
    use barsql_core::{
        Check, Dependent, DriverType, ForeignKey, Identity, RefAction, StructColumn, StructIndex, TableStructure,
    };

    use crate::structure::fixtures::*;
    use crate::structure::{Draft, Row, Warning, plan_changes, plan_create};

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

    fn dependent(kind: &str, name: &str, sql: &str) -> Dependent {
        Dependent { kind: kind.into(), name: name.into(), sql: sql.into() }
    }

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

    fn with_view() -> (TableStructure, Draft) {
        let mut original = rebuilt();
        original.dependents.push(dependent("view", "users_v", "CREATE VIEW users_v AS SELECT name FROM users"));
        let other = "CREATE TRIGGER pets_log AFTER INSERT ON pets BEGIN UPDATE users SET age = age; END";
        original.dependents.push(dependent("referring trigger", "pets_log", other));
        let mut draft = Draft::from_structure(&original);
        draft.columns[2].column.data_type = "INTEGER".into();
        draft.rename_column(1, "full_name".into());
        (original, draft)
    }

    #[test]
    fn turso_rebuilds_by_dropping_and_recreating_what_refers_to_the_table() {
        let (original, draft) = with_view();
        let plan = plan_changes(&DriverType::Turso, &original, &draft).expect("valid");
        let steps = sql(&plan);
        assert!(!steps.iter().any(|step| step.contains("legacy_alter_table")), "{steps:?}");
        let at =
            |text: &str| steps.iter().position(|step| *step == text).unwrap_or_else(|| panic!("{text}: {steps:?}"));
        let renamed = at(r#"ALTER TABLE "barsql_new_users" RENAME TO "users""#);
        assert!(at(r#"DROP VIEW "users_v""#) < at(r#"DROP TABLE "users""#));
        assert!(at(r#"DROP TRIGGER "pets_log""#) < at(r#"DROP TABLE "users""#));
        let view = at("CREATE VIEW users_v AS SELECT name FROM users");
        let trigger = at("CREATE TRIGGER pets_log AFTER INSERT ON pets BEGIN UPDATE users SET age = age; END");
        let column = at(r#"ALTER TABLE "users" RENAME COLUMN "name" TO "full_name""#);
        assert!(renamed < view && view < trigger && trigger < column, "{steps:?}");
    }

    #[test]
    fn sqlite_leaves_what_refers_to_the_table_to_legacy_alter_table() {
        let (original, draft) = with_view();
        let steps: Vec<String> =
            sql(&plan_changes(&LITE, &original, &draft).expect("valid")).iter().map(|s| s.to_string()).collect();
        assert!(steps.contains(&"PRAGMA legacy_alter_table = ON".to_string()));
        assert!(!steps.iter().any(|step| step.contains("users_v") || step.contains("pets_log")), "{steps:?}");
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
    fn sqlite_adds_and_drops_indexes() {
        let mut original = simple();
        original.indexes = vec![index("idx_users_name", &["name"], false)];
        let mut draft = Draft::from_structure(&original);
        draft.indexes[0].dropped = true;
        draft.columns.push(added(column("email", "TEXT")));
        draft.indexes.push(Row::added(index("idx_users_email", &["email"], true)));
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "BEGIN",
                r#"DROP INDEX "idx_users_name""#,
                r#"ALTER TABLE "users" ADD COLUMN "email" TEXT"#,
                r#"CREATE UNIQUE INDEX "idx_users_email" ON "users" ("email")"#,
                "COMMIT",
            ]
        );
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn sqlite_rebuilds_to_drop_a_unique_constraint() {
        let mut original = table(
            "users",
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT UNIQUE CHECK (email <> ''), code TEXT, UNIQUE (code))",
            vec![column("id", "INTEGER"), column("email", "TEXT"), column("code", "TEXT")],
            &["id"],
        );
        original.indexes = vec![
            StructIndex { constraint: true, ..index("sqlite_autoindex_users_1", &["email"], true) },
            StructIndex { constraint: true, ..index("sqlite_autoindex_users_2", &["code"], true) },
        ];
        let mut draft = Draft::from_structure(&original);
        draft.indexes[0].dropped = true;
        draft.indexes[1].dropped = true;
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(plan.warnings, [Warning::Rebuild]);
        assert_eq!(
            sql(&plan)[2],
            "CREATE TABLE \"barsql_new_users\" (\n    id INTEGER PRIMARY KEY,\n    \"email\" TEXT CHECK (email <> ''),\n    code TEXT\n)"
        );
    }

    #[test]
    fn sqlite_creates_new_indexes_after_a_rebuild_renames_columns() {
        let original = simple();
        let mut draft = Draft::from_structure(&original);
        draft.columns[1].column.data_type = "VARCHAR(20)".into();
        draft.rename_column(1, "full_name".into());
        draft.indexes.push(Row::added(index("idx_users_full_name", &["full_name"], false)));
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        let steps = sql(&plan);
        let rename = steps.iter().position(|step| step.contains("RENAME COLUMN")).expect("a rename");
        assert_eq!(steps[rename + 1], r#"CREATE INDEX "idx_users_full_name" ON "users" ("full_name")"#);
    }

    #[test]
    fn sqlite_adds_and_drops_checks() {
        let mut original = table(
            "users",
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT CHECK (name <> ''), age INTEGER, CHECK (age >= 0))",
            vec![column("id", "INTEGER"), column("name", "TEXT"), column("age", "INTEGER")],
            &["id"],
        );
        original.checks = vec![check("", "name <> ''"), check("", "age >= 0")];
        let mut draft = Draft::from_structure(&original);
        draft.checks[0].dropped = true;
        draft.checks[1].dropped = true;
        draft.checks.push(Row::added(check("adult", "age >= 18")));
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(plan.warnings, [Warning::Rebuild]);
        assert_eq!(
            sql(&plan)[2],
            "CREATE TABLE \"barsql_new_users\" (\n    id INTEGER PRIMARY KEY,\n    \"name\" TEXT,\n    age INTEGER,\n    CONSTRAINT \"adult\" CHECK (age >= 18)\n)"
        );
    }

    #[test]
    fn sqlite_checks_name_renamed_columns_by_their_old_name() {
        let original = simple();
        let mut draft = Draft::from_structure(&original);
        draft.rename_column(1, "full_name".into());
        draft.checks.push(Row::added(check("", "length(full_name) > 0")));
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        let steps = sql(&plan);
        assert!(steps[2].ends_with("    CHECK (length(\"name\") > 0)\n)"), "{}", steps[2]);
        assert!(steps.contains(&r#"ALTER TABLE "users" RENAME COLUMN "name" TO "full_name""#), "{steps:?}");
    }

    #[test]
    fn sqlite_refuses_a_dropped_check_it_cannot_find() {
        let mut original = simple();
        original.checks = vec![check("", "legacy <> ''")];
        let mut draft = Draft::from_structure(&original);
        draft.checks[0].dropped = true;
        assert!(plan_changes(&LITE, &original, &draft).is_err());
    }

    #[test]
    fn sqlite_adds_and_drops_foreign_keys() {
        let mut original = table(
            "pets",
            "CREATE TABLE pets (id INTEGER PRIMARY KEY, owner_id INTEGER REFERENCES people(id), team_id INTEGER, FOREIGN KEY (team_id) REFERENCES teams (id))",
            vec![column("id", "INTEGER"), column("owner_id", "INTEGER"), column("team_id", "INTEGER")],
            &["id"],
        );
        original.foreign_keys =
            vec![foreign_key("", &["owner_id"], "people", &["id"]), foreign_key("", &["team_id"], "teams", &["id"])];
        let mut draft = Draft::from_structure(&original);
        draft.foreign_keys[0].dropped = true;
        draft.foreign_keys[1].dropped = true;
        let key = ForeignKey {
            on_delete: RefAction::Cascade,
            ..foreign_key("fk_pets_people", &["owner_id"], "people", &["id"])
        };
        draft.foreign_keys.push(Row::added(key));
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        assert_eq!(plan.warnings, [Warning::Rebuild]);
        assert_eq!(
            sql(&plan)[2],
            "CREATE TABLE \"barsql_new_pets\" (\n    id INTEGER PRIMARY KEY,\n    \"owner_id\" INTEGER,\n    team_id INTEGER,\n    CONSTRAINT \"fk_pets_people\" FOREIGN KEY (\"owner_id\") REFERENCES \"people\" (\"id\") ON DELETE CASCADE\n)"
        );
    }

    #[test]
    fn sqlite_foreign_keys_name_renamed_columns_by_their_old_name() {
        let original = simple();
        let mut draft = Draft::from_structure(&original);
        draft.rename_column(2, "team_id".into());
        draft.foreign_keys.push(Row::added(foreign_key("", &["team_id"], "teams", &["id"])));
        let plan = plan_changes(&LITE, &original, &draft).expect("valid");
        let steps = sql(&plan);
        assert!(steps[2].ends_with("    FOREIGN KEY (\"legacy\") REFERENCES \"teams\" (\"id\")\n)"), "{}", steps[2]);
    }

    #[test]
    fn sqlite_creates_a_table_with_its_indexes_checks_and_keys() {
        let mut draft = Draft::new_table(&LITE, "main");
        draft.name = "people".into();
        draft.columns.push(added(column("org_id", "INTEGER")));
        draft.checks.push(Row::added(check("people_id_check", "id > 0")));
        draft.foreign_keys.push(Row::added(foreign_key("fk_people_orgs", &["org_id"], "orgs", &["id"])));
        draft.indexes.push(Row::added(index("idx_people_org_id", &["org_id"], false)));
        let plan = plan_create(&LITE, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "BEGIN",
                "CREATE TABLE \"people\" (\n    \"id\" INTEGER PRIMARY KEY NOT NULL,\n    \"org_id\" INTEGER,\n    CONSTRAINT \"people_id_check\" CHECK (id > 0),\n    CONSTRAINT \"fk_people_orgs\" FOREIGN KEY (\"org_id\") REFERENCES \"orgs\" (\"id\")\n)",
                r#"CREATE INDEX "idx_people_org_id" ON "people" ("org_id")"#,
                "COMMIT",
            ]
        );
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
