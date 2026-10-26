use std::collections::HashSet;

use barsql_core::{DriverType, SqlDialect, StructColumn, TableStructure};

use super::{Changes, Draft};
use crate::alter::{drop_column, rename_column};
use crate::lang::tokens::{TokenKind, tokenize};
use crate::quote::{qualified_table, quote_ident, quote_ident_list, quote_literal_in, table_ref};

// The model keeps nullability apart from the type. ClickHouse writes it as Nullable(T), inside LowCardinality(T).
pub(super) fn full_type(column: &StructColumn) -> String {
    let data_type = column.data_type.trim();
    if !column.nullable {
        return data_type.to_string();
    }
    match data_type.strip_prefix("LowCardinality(").and_then(|inner| inner.strip_suffix(')')) {
        Some(inner) => format!("LowCardinality(Nullable({inner}))"),
        None => format!("Nullable({data_type})"),
    }
}

// The column names a sorting key expression uses, function names left out.
pub(super) fn sorting_columns(sorting_key: &str) -> HashSet<String> {
    let tokens = tokenize(sorting_key, Some(&DriverType::ClickHouse));
    let named = tokens.iter().enumerate().filter(|(ix, t)| {
        matches!(t.kind, TokenKind::Ident | TokenKind::Quoted)
            && !tokens.get(ix + 1).is_some_and(|next| next.is_punct("("))
    });
    named.map(|(_, t)| t.ident_text()).collect()
}

fn definition(column: &StructColumn) -> String {
    match column.default.as_str() {
        "" => full_type(column),
        default => format!("{} DEFAULT {default}", full_type(column)),
    }
}

pub(super) fn alter(driver: &DriverType, original: &TableStructure, changes: &Changes) -> Vec<String> {
    let (schema, name) = (original.schema.as_str(), original.name.as_str());
    let table = table_ref(driver, schema, name);
    let column_action = |what: &str, column: &str, rest: &str| {
        format!("ALTER TABLE {table} {what} COLUMN {} {rest}", quote_ident(driver, column)).trim_end().to_string()
    };
    let mut out: Vec<String> =
        changes.renamed.iter().map(|(old, new)| rename_column(driver, schema, name, old, new)).collect();
    for (old, new) in &changes.modified {
        let retyped = old.data_type != new.data_type || old.nullable != new.nullable;
        if new.default.is_empty() && !old.default.is_empty() {
            out.push(column_action("MODIFY", &new.name, "REMOVE DEFAULT"));
        }
        if retyped || (old.default != new.default && !new.default.is_empty()) {
            out.push(column_action("MODIFY", &new.name, &definition(new)));
        }
    }
    out.extend(changes.dropped.iter().map(|column| drop_column(driver, schema, name, &column.name, false)));
    out.extend(changes.added.iter().map(|column| column_action("ADD", &column.name, &definition(column))));
    let commented = changes.modified.iter().filter(|(old, new)| old.comment != new.comment).map(|(_, new)| new);
    let added = changes.added.iter().filter(|column| !column.comment.is_empty());
    for column in commented.chain(added) {
        out.push(column_action("COMMENT", &column.name, &quote_literal_in(SqlDialect::ClickHouse, &column.comment)));
    }
    out
}

pub(super) fn create(driver: &DriverType, draft: &Draft) -> Vec<String> {
    let live = draft.columns.iter().filter(|row| !row.dropped).map(|row| &row.column);
    let items: Vec<String> = live
        .map(|column| {
            let mut item = format!("{} {}", quote_ident(driver, &column.name), definition(column));
            if !column.comment.is_empty() {
                item.push_str(&format!(" COMMENT {}", quote_literal_in(SqlDialect::ClickHouse, &column.comment)));
            }
            item
        })
        .collect();
    let keys = draft.primary_columns();
    let order = match keys.is_empty() {
        true => "tuple()".to_string(),
        false => format!("({})", quote_ident_list(driver, &keys)),
    };
    let table = qualified_table(driver, &draft.schema, &draft.name);
    vec![format!("CREATE TABLE {table} (\n    {}\n) ENGINE = MergeTree ORDER BY {order}", items.join(",\n    "))]
}

#[cfg(test)]
mod tests {
    use barsql_core::{DriverType, StructColumn, TableStructure};

    use crate::structure::fixtures::*;
    use crate::structure::{Draft, Problem, ProblemKind, Section, Warning, plan_changes, plan_create};

    const CH: DriverType = DriverType::ClickHouse;

    fn events() -> TableStructure {
        TableStructure {
            schema: "db".into(),
            name: "events".into(),
            columns: vec![
                StructColumn { nullable: false, ..column("id", "UInt64") },
                column("name", "String"),
                StructColumn { nullable: false, ..column("tag", "LowCardinality(String)") },
                StructColumn { nullable: false, ..column("legacy", "String") },
            ],
            sorting_key: "id".into(),
            ..Default::default()
        }
    }

    #[test]
    fn clickhouse_alters_one_action_each() {
        let original = events();
        let mut draft = Draft::from_structure(&original);
        draft.columns[1].column.nullable = false;
        draft.columns[1].column.comment = "Display".into();
        draft.columns[2].column.nullable = true;
        draft.rename_column(3, "old".into());
        let score = StructColumn { nullable: false, default: "0".into(), ..column("score", "Float64") };
        draft.columns.push(added(score));
        let plan = plan_changes(&CH, &original, &draft).expect("valid");
        assert_eq!(plan.warnings, [Warning::NotAtomic]);
        assert_eq!(
            sql(&plan),
            [
                "ALTER TABLE `db`.`events` RENAME COLUMN `legacy` TO `old`",
                "ALTER TABLE `db`.`events` MODIFY COLUMN `name` String",
                "ALTER TABLE `db`.`events` MODIFY COLUMN `tag` LowCardinality(Nullable(String))",
                "ALTER TABLE `db`.`events` ADD COLUMN `score` Float64 DEFAULT 0",
                "ALTER TABLE `db`.`events` COMMENT COLUMN `name` 'Display'",
            ]
        );
    }

    #[test]
    fn clickhouse_protects_the_sorting_key() {
        let original = events();
        let mut draft = Draft::from_structure(&original);
        draft.columns[0].column.data_type = "UInt32".into();
        let unsupported = Problem { section: Section::Columns, row: 0, kind: ProblemKind::Unsupported };
        assert_eq!(plan_changes(&CH, &original, &draft), Err(vec![unsupported]));

        let mut draft = Draft::from_structure(&original);
        draft.rename_column(0, "event_id".into());
        assert!(plan_changes(&CH, &original, &draft).is_ok(), "a rename is fine");
    }

    #[test]
    fn clickhouse_quotes_backticks() {
        let mut original = events();
        original.columns.push(column("a`b", "String"));
        let mut draft = Draft::from_structure(&original);
        draft.rename_column(4, "ab".into());
        let plan = plan_changes(&CH, &original, &draft).expect("valid");
        assert_eq!(sql(&plan), ["ALTER TABLE `db`.`events` RENAME COLUMN `a\\`b` TO `ab`"]);
    }

    #[test]
    fn clickhouse_creates_a_table() {
        let mut draft = Draft::new_table(&CH, "db");
        draft.name = "events".into();
        let plan = plan_create(&CH, &draft).expect("valid");
        assert_eq!(sql(&plan), ["CREATE TABLE `db`.`events` (\n    `id` UInt64\n) ENGINE = MergeTree ORDER BY (`id`)"]);
    }
}
