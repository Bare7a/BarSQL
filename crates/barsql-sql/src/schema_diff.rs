use std::collections::BTreeMap;

use barsql_core::{ColumnInfo, DriverType, SqlDialect};

use crate::alter::drop_column;
use crate::ddl::{DdlColumn, compose_create_table, render_column};
use crate::quote::{quote_ident_list, table_ref};

// One schema's tables with their columns.
pub type Tables = Vec<(String, Vec<ColumnInfo>)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnDiff {
    // In the source only, so the target lacks it.
    Missing(ColumnInfo),
    // In the target only.
    Extra(ColumnInfo),
    Changed { source: ColumnInfo, target: ColumnInfo },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableDiff {
    Missing { name: String, columns: Vec<ColumnInfo> },
    Extra { name: String },
    Changed { name: String, columns: Vec<ColumnDiff> },
}

impl TableDiff {
    pub fn name(&self) -> &str {
        match self {
            Self::Missing { name, .. } | Self::Extra { name } | Self::Changed { name, .. } => name,
        }
    }
}

fn same_column(a: &ColumnInfo, b: &ColumnInfo) -> bool {
    a.data_type.trim().eq_ignore_ascii_case(b.data_type.trim())
        && a.is_nullable == b.is_nullable
        && a.is_primary == b.is_primary
        && a.default_val.trim() == b.default_val.trim()
}

// What the target lacks or has beyond the source, by table and column name, ignoring case. Tables sort by name, and
// columns keep the source's order with the target's extras after.
pub fn diff(source: &Tables, target: &Tables) -> Vec<TableDiff> {
    let index = |tables: &Tables| -> BTreeMap<String, usize> {
        tables.iter().enumerate().map(|(ix, (name, _))| (name.to_lowercase(), ix)).collect()
    };
    let (from, to) = (index(source), index(target));
    let mut out = Vec::new();
    let names: std::collections::BTreeSet<&String> = from.keys().chain(to.keys()).collect();
    for name in names {
        match (from.get(name), to.get(name)) {
            (Some(&s), None) => {
                out.push(TableDiff::Missing { name: source[s].0.clone(), columns: source[s].1.clone() })
            }
            (None, Some(&t)) => out.push(TableDiff::Extra { name: target[t].0.clone() }),
            (Some(&s), Some(&t)) => {
                let columns = diff_columns(&source[s].1, &target[t].1);
                if !columns.is_empty() {
                    out.push(TableDiff::Changed { name: target[t].0.clone(), columns });
                }
            }
            (None, None) => {}
        }
    }
    out
}

fn diff_columns(source: &[ColumnInfo], target: &[ColumnInfo]) -> Vec<ColumnDiff> {
    let find = |columns: &[ColumnInfo], name: &str| columns.iter().find(|c| c.name.eq_ignore_ascii_case(name)).cloned();
    let mut out: Vec<ColumnDiff> = source
        .iter()
        .filter_map(|column| match find(target, &column.name) {
            None => Some(ColumnDiff::Missing(column.clone())),
            Some(other) if !same_column(column, &other) => {
                Some(ColumnDiff::Changed { source: column.clone(), target: other })
            }
            Some(_) => None,
        })
        .collect();
    out.extend(target.iter().filter(|c| find(source, &c.name).is_none()).map(|c| ColumnDiff::Extra(c.clone())));
    out
}

fn ddl_column(column: &ColumnInfo) -> DdlColumn {
    DdlColumn {
        name: column.name.clone(),
        data_type: column.data_type.clone(),
        not_null: !column.is_nullable,
        default: column.default_val.clone(),
        ..Default::default()
    }
}

fn describe(column: &ColumnInfo) -> String {
    let mut text = column.data_type.to_lowercase();
    if !column.is_nullable {
        text.push_str(" not null");
    }
    if column.is_primary {
        text.push_str(" primary key");
    }
    if !column.default_val.is_empty() {
        text.push_str(&format!(" default {}", column.default_val));
    }
    text
}

// SQL that brings the target schema up to the source: missing tables and columns are created, extra columns
// dropped. What can't be changed safely in one statement, an extra table or a changed column, is left as a
// comment to do by hand.
pub fn sync_script(driver: &DriverType, schema: &str, diffs: &[TableDiff]) -> String {
    let mut out: Vec<String> = Vec::new();
    let add = if driver.dialect() == Some(SqlDialect::TSql) { "ADD" } else { "ADD COLUMN" };
    for diff in diffs {
        match diff {
            TableDiff::Missing { name, columns } => {
                let keys: Vec<String> = columns.iter().filter(|c| c.is_primary).map(|c| c.name.clone()).collect();
                let constraints = match keys.is_empty() {
                    true => Vec::new(),
                    false => vec![format!("PRIMARY KEY ({})", quote_ident_list(driver, &keys))],
                };
                let columns: Vec<DdlColumn> = columns.iter().map(ddl_column).collect();
                out.push(compose_create_table(driver, schema, name, &columns, &constraints));
            }
            TableDiff::Extra { name } => {
                out.push(format!("-- Only in the target: DROP TABLE {};", table_ref(driver, schema, name)));
            }
            TableDiff::Changed { name, columns } => {
                let table = table_ref(driver, schema, name);
                for column in columns {
                    out.push(match column {
                        ColumnDiff::Missing(c) => {
                            format!("ALTER TABLE {table} {add} {};", render_column(driver, &ddl_column(c)))
                        }
                        ColumnDiff::Extra(c) => format!("{};", drop_column(driver, schema, name, &c.name, false)),
                        ColumnDiff::Changed { source, target } => {
                            format!("-- {name}.{}: {} → {}", source.name, describe(target), describe(source))
                        }
                    });
                }
            }
        }
    }
    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, data_type: &str) -> ColumnInfo {
        ColumnInfo { name: name.into(), data_type: data_type.into(), is_nullable: true, ..Default::default() }
    }

    fn key(name: &str) -> ColumnInfo {
        ColumnInfo { is_primary: true, is_nullable: false, ..column(name, "integer") }
    }

    #[test]
    fn finds_missing_extra_and_changed_tables_and_columns() {
        let source: Tables = vec![
            ("users".into(), vec![key("id"), column("email", "varchar(255)"), column("name", "text")]),
            ("orders".into(), vec![key("id")]),
        ];
        let target: Tables = vec![
            ("Users".into(), vec![key("ID"), column("email", "varchar(100)"), column("legacy", "text")]),
            ("logs".into(), vec![column("line", "text")]),
        ];
        let diffs = diff(&source, &target);
        let names: Vec<&str> = diffs.iter().map(TableDiff::name).collect();
        assert_eq!(names, ["logs", "orders", "Users"], "by name, ignoring case");
        let TableDiff::Changed { columns, .. } = &diffs[2] else { panic!("users changed") };
        let kinds: Vec<String> = columns
            .iter()
            .map(|c| match c {
                ColumnDiff::Missing(c) => format!("+{}", c.name),
                ColumnDiff::Extra(c) => format!("-{}", c.name),
                ColumnDiff::Changed { source, .. } => format!("~{}", source.name),
            })
            .collect();
        assert_eq!(kinds, ["~email", "+name", "-legacy"]);
    }

    #[test]
    fn the_script_creates_adds_and_drops_and_leaves_the_rest_as_comments() {
        let source: Tables = vec![
            ("users".into(), vec![key("id"), column("name", "text")]),
            ("orders".into(), vec![key("id"), column("total", "numeric")]),
        ];
        let target: Tables =
            vec![("users".into(), vec![key("id"), column("old", "text")]), ("logs".into(), vec![key("id")])];
        let script = sync_script(&DriverType::Postgres, "public", &diff(&source, &target));
        let expected = r#"-- Only in the target: DROP TABLE "public"."logs";

CREATE TABLE "public"."orders" (
    "id" integer NOT NULL,
    "total" numeric,
    PRIMARY KEY ("id")
);

ALTER TABLE "public"."users" ADD COLUMN "name" text;

ALTER TABLE "public"."users" DROP COLUMN "old";"#;
        assert_eq!(script, expected);
    }
}
