mod clickhouse;
pub mod create_sql;
mod mysql;
mod postgres;
mod sqlite;
mod tsql;

use std::collections::{HashMap, HashSet};

use barsql_core::capabilities::{IdentityStyle, StructureEditing};
use barsql_core::{
    Check, DriverType, ForeignKey, Identity, PrimaryKey, SqlDialect, StructColumn, StructIndex, TableStructure,
};

use crate::ddl::{DdlColumn, terminate_statement};
use crate::lang::tokens::{TokenKind, tokenize};

// The structure editor's model of a change: the edited draft, what differs from the table as loaded, and the
// statements that make the difference.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row<T> {
    // Index into the original's list. None for a row the draft added.
    pub origin: Option<usize>,
    pub dropped: bool,
    pub value: T,
}

impl<T> Row<T> {
    pub fn added(value: T) -> Self {
        Self { origin: None, dropped: false, value }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnRow {
    pub origin: Option<usize>,
    pub dropped: bool,
    pub column: StructColumn,
    pub primary: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub schema: String,
    pub name: String,
    pub columns: Vec<ColumnRow>,
    pub indexes: Vec<Row<StructIndex>>,
    pub foreign_keys: Vec<Row<ForeignKey>>,
    pub checks: Vec<Row<Check>>,
}

fn kept<T: Clone>(items: &[T]) -> Vec<Row<T>> {
    items.iter().enumerate().map(|(ix, value)| Row { origin: Some(ix), dropped: false, value: value.clone() }).collect()
}

impl Draft {
    pub fn from_structure(original: &TableStructure) -> Self {
        let keys = original.primary_key.as_ref().map_or(&[][..], |key| key.columns.as_slice());
        let columns = original
            .columns
            .iter()
            .enumerate()
            .map(|(ix, column)| ColumnRow {
                origin: Some(ix),
                dropped: false,
                primary: keys.contains(&column.name),
                column: column.clone(),
            })
            .collect();
        Self {
            schema: original.schema.clone(),
            name: original.name.clone(),
            columns,
            indexes: kept(&original.indexes),
            foreign_keys: kept(&original.foreign_keys),
            checks: kept(&original.checks),
        }
    }

    pub fn new_table(driver: &DriverType, schema: &str) -> Self {
        let (data_type, identity) = match driver.dialect() {
            Some(SqlDialect::MySql) => ("BIGINT", Identity::AutoIncrement),
            Some(SqlDialect::Sqlite) => ("INTEGER", Identity::None),
            Some(SqlDialect::TSql) => ("bigint", Identity::Seeded { seed: 1, increment: 1 }),
            Some(SqlDialect::ClickHouse) => ("UInt64", Identity::None),
            Some(SqlDialect::Postgres) | None => ("bigint", Identity::ByDefault),
        };
        let id = StructColumn { name: "id".into(), data_type: data_type.into(), identity, ..Default::default() };
        let row = ColumnRow { origin: None, dropped: false, column: id, primary: true };
        Self { schema: schema.into(), columns: vec![row], ..Default::default() }
    }

    pub fn primary_columns(&self) -> Vec<String> {
        self.columns.iter().filter(|row| row.primary && !row.dropped).map(|row| row.column.name.clone()).collect()
    }

    // Indexes and foreign keys name columns as the draft does, so they follow a rename.
    pub fn rename_column(&mut self, row: usize, name: String) {
        let Some(old) = self.columns.get(row).map(|row| row.column.name.clone()) else { return };
        let follow = |columns: &mut Vec<String>| {
            columns.iter_mut().filter(|column| **column == old).for_each(|column| *column = name.clone())
        };
        self.indexes.iter_mut().for_each(|index| follow(&mut index.value.columns));
        self.foreign_keys.iter_mut().for_each(|key| follow(&mut key.value.columns));
        self.columns[row].column.name = name;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub sql: String,
    // SQLite's foreign_key_check: any row it returns fails the plan.
    pub expect_no_rows: bool,
}

impl Step {
    pub fn sql(sql: impl Into<String>) -> Self {
        Self { sql: sql.into(), expect_no_rows: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warning {
    Rebuild,
    NotAtomic,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangePlan {
    pub steps: Vec<Step>,
    pub warnings: Vec<Warning>,
}

impl ChangePlan {
    // What the preview and the history show. SQL Server runs each step as a batch of its own.
    pub fn script(&self, driver: &DriverType) -> String {
        let statements: Vec<String> = match driver.dialect() {
            Some(SqlDialect::TSql) => self.steps.iter().map(|step| format!("{};\nGO", step.sql)).collect(),
            _ => self.steps.iter().map(|step| terminate_statement(&step.sql)).collect(),
        };
        statements.join("\n\n")
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Table,
    Columns,
    Indexes,
    ForeignKeys,
    Checks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemKind {
    EmptyTableName,
    EmptyName,
    DuplicateName,
    EmptyType,
    NoColumns,
    NoIndexColumns,
    UsesDroppedColumn(String),
    ForeignKeyColumnCount,
    NoReferencedTable,
    EmptyExpression,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub section: Section,
    pub row: usize,
    pub kind: ProblemKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    // (old, new) names.
    pub renamed: Vec<(String, String)>,
    // (old, new) columns whose fields besides the name differ. The new one carries the final name.
    pub modified: Vec<(StructColumn, StructColumn)>,
    pub dropped: Vec<StructColumn>,
    pub added: Vec<StructColumn>,
    // (old, new), set only when the key's columns differ.
    pub primary_key: Option<(Option<PrimaryKey>, Option<PrimaryKey>)>,
    pub dropped_indexes: Vec<StructIndex>,
    pub added_indexes: Vec<StructIndex>,
    pub dropped_foreign_keys: Vec<ForeignKey>,
    pub added_foreign_keys: Vec<ForeignKey>,
    pub dropped_checks: Vec<Check>,
    pub added_checks: Vec<Check>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.renamed.is_empty()
            && self.modified.is_empty()
            && self.dropped.is_empty()
            && self.added.is_empty()
            && self.primary_key.is_none()
            && self.dropped_indexes.is_empty()
            && self.added_indexes.is_empty()
            && self.dropped_foreign_keys.is_empty()
            && self.added_foreign_keys.is_empty()
            && self.dropped_checks.is_empty()
            && self.added_checks.is_empty()
    }
}

fn same_column(a: &StructColumn, b: &StructColumn) -> bool {
    let unnamed = |c: &StructColumn| StructColumn { name: String::new(), ..c.clone() };
    unnamed(a) == unnamed(b)
}

// Rows that differ from the original: added, dropped, or changed once renames are followed.
fn row_changes<'a, T: PartialEq>(
    original: &'a [T],
    rows: &[Row<T>],
    follow: impl Fn(&T) -> T,
) -> Vec<(usize, Option<&'a T>)> {
    rows.iter()
        .enumerate()
        .filter_map(|(ix, row)| match row.origin.and_then(|origin| original.get(origin)) {
            None => (!row.dropped).then_some((ix, None)),
            Some(old) if row.dropped || follow(old) != row.value => Some((ix, Some(old))),
            Some(_) => None,
        })
        .collect()
}

fn diff_rows<T: Clone + PartialEq>(
    original: &[T],
    rows: &[Row<T>],
    follow: impl Fn(&T) -> T,
    dropped: &mut Vec<T>,
    added: &mut Vec<T>,
) {
    for (ix, old) in row_changes(original, rows, follow) {
        dropped.extend(old.cloned());
        if !rows[ix].dropped {
            added.push(rows[ix].value.clone());
        }
    }
}

fn renames(original: &TableStructure, draft: &Draft) -> HashMap<String, String> {
    draft
        .columns
        .iter()
        .filter_map(|row| row.origin.and_then(|origin| original.columns.get(origin)).map(|old| (old, row)))
        .filter(|(old, row)| old.name != row.column.name)
        .map(|(old, row)| (old.name.clone(), row.column.name.clone()))
        .collect()
}

fn follow_names(renamed: &HashMap<String, String>, names: &[String]) -> Vec<String> {
    names.iter().map(|name| renamed.get(name).cloned().unwrap_or_else(|| name.clone())).collect()
}

pub fn changes(original: &TableStructure, draft: &Draft) -> Changes {
    let mut out = Changes::default();
    for row in &draft.columns {
        match row.origin.and_then(|origin| original.columns.get(origin)) {
            None if !row.dropped => out.added.push(row.column.clone()),
            None => {}
            Some(old) if row.dropped => out.dropped.push(old.clone()),
            Some(old) => {
                if old.name != row.column.name {
                    out.renamed.push((old.name.clone(), row.column.name.clone()));
                }
                if !same_column(old, &row.column) {
                    out.modified.push((old.clone(), row.column.clone()));
                }
            }
        }
    }
    let renamed = renames(original, draft);
    let old_key = original.primary_key.clone().filter(|key| !key.columns.is_empty());
    let new_columns = draft.primary_columns();
    if old_key.as_ref().map(|key| follow_names(&renamed, &key.columns)).unwrap_or_default() != new_columns {
        let name = old_key.as_ref().map(|key| key.name.clone()).unwrap_or_default();
        let new_key = (!new_columns.is_empty()).then_some(PrimaryKey { name, columns: new_columns });
        out.primary_key = Some((old_key, new_key));
    }
    let index = |old: &StructIndex| StructIndex { columns: follow_names(&renamed, &old.columns), ..old.clone() };
    diff_rows(&original.indexes, &draft.indexes, index, &mut out.dropped_indexes, &mut out.added_indexes);
    let key = |old: &ForeignKey| ForeignKey { columns: follow_names(&renamed, &old.columns), ..old.clone() };
    diff_rows(
        &original.foreign_keys,
        &draft.foreign_keys,
        key,
        &mut out.dropped_foreign_keys,
        &mut out.added_foreign_keys,
    );
    diff_rows(&original.checks, &draft.checks, Check::clone, &mut out.dropped_checks, &mut out.added_checks);
    out
}

fn identity_allowed(editing: &StructureEditing, identity: Identity) -> bool {
    matches!(
        (editing.identity, identity),
        (_, Identity::None)
            | (IdentityStyle::Generated, Identity::ByDefault | Identity::Always)
            | (IdentityStyle::AutoIncrement, Identity::AutoIncrement)
            | (IdentityStyle::Seeded, Identity::Seeded { .. })
    )
}

// Names compared case-blind. Empty names are left to `required`.
fn name_problems<'a>(
    section: Section,
    names: impl Iterator<Item = (usize, &'a str)>,
    required: bool,
    out: &mut Vec<Problem>,
) {
    let mut seen = HashSet::new();
    for (row, name) in names {
        let name = name.trim();
        if name.is_empty() {
            if required {
                out.push(Problem { section, row, kind: ProblemKind::EmptyName });
            }
        } else if !seen.insert(name.to_lowercase()) {
            out.push(Problem { section, row, kind: ProblemKind::DuplicateName });
        }
    }
}

pub fn validate(driver: &DriverType, original: Option<&TableStructure>, draft: &Draft) -> Vec<Problem> {
    let editing = &driver.capabilities().structure;
    let empty = TableStructure::default();
    let base = original.unwrap_or(&empty);
    let mut out = Vec::new();
    let problem = |section, row, kind| Problem { section, row, kind };
    if original.is_none() && draft.name.trim().is_empty() {
        out.push(problem(Section::Table, 0, ProblemKind::EmptyTableName));
    }

    let columns: Vec<(usize, &ColumnRow)> = draft.columns.iter().enumerate().filter(|(_, row)| !row.dropped).collect();
    if columns.is_empty() {
        out.push(problem(Section::Columns, 0, ProblemKind::NoColumns));
    }
    let mut column_problems = Vec::new();
    name_problems(
        Section::Columns,
        columns.iter().map(|(ix, row)| (*ix, row.column.name.as_str())),
        true,
        &mut column_problems,
    );
    for (ix, row) in &columns {
        if row.column.data_type.trim().is_empty() && row.column.generated.is_empty() {
            column_problems.push(problem(Section::Columns, *ix, ProblemKind::EmptyType));
        }
        let old = row.origin.and_then(|origin| base.columns.get(origin));
        let identity_ok = match old {
            None => identity_allowed(editing, row.column.identity),
            Some(old) if old.identity == row.column.identity => true,
            Some(_) => editing.identity_on_existing && identity_allowed(editing, row.column.identity),
        };
        let comment_ok =
            editing.comments || old.map_or(row.column.comment.is_empty(), |c| c.comment == row.column.comment);
        if !identity_ok || !comment_ok {
            column_problems.push(problem(Section::Columns, *ix, ProblemKind::Unsupported));
        }
    }
    let found = changes(base, draft);
    if !editing.primary_key && original.is_some() && found.primary_key.is_some() {
        let first = draft.columns.iter().position(|row| row.primary).unwrap_or(0);
        column_problems.push(problem(Section::Columns, first, ProblemKind::Unsupported));
    }
    if driver.dialect() == Some(SqlDialect::ClickHouse) && !base.sorting_key.is_empty() {
        let keyed = clickhouse::sorting_columns(&base.sorting_key);
        for (ix, row) in draft.columns.iter().enumerate() {
            let Some(old) = row.origin.and_then(|origin| base.columns.get(origin)) else { continue };
            let reshaped = StructColumn { name: old.name.clone(), comment: old.comment.clone(), ..row.column.clone() };
            if keyed.contains(&old.name) && (row.dropped || reshaped != *old) {
                column_problems.push(problem(Section::Columns, ix, ProblemKind::Unsupported));
            }
        }
    }
    column_problems.sort_by_key(|p| p.row);
    out.extend(column_problems);

    let dropped: HashSet<&str> =
        draft.columns.iter().filter(|row| row.dropped).map(|row| row.column.name.as_str()).collect();
    let uses_dropped = |columns: &[String]| columns.iter().find(|column| dropped.contains(column.as_str())).cloned();
    let renamed = renames(base, draft);

    let index = |old: &StructIndex| StructIndex { columns: follow_names(&renamed, &old.columns), ..old.clone() };
    let changed_indexes: HashSet<usize> =
        row_changes(&base.indexes, &draft.indexes, index).into_iter().map(|(ix, _)| ix).collect();
    let live_indexes = draft.indexes.iter().enumerate().filter(|(_, row)| !row.dropped);
    name_problems(
        Section::Indexes,
        live_indexes.clone().map(|(ix, row)| (ix, row.value.name.as_str())),
        true,
        &mut out,
    );
    for (ix, row) in draft.indexes.iter().enumerate() {
        let kind = if !editing.indexes && changed_indexes.contains(&ix) {
            Some(ProblemKind::Unsupported)
        } else if row.dropped || !row.value.definition.is_empty() {
            None
        } else if row.value.columns.is_empty() {
            Some(ProblemKind::NoIndexColumns)
        } else {
            uses_dropped(&row.value.columns).map(ProblemKind::UsesDroppedColumn)
        };
        out.extend(kind.map(|kind| problem(Section::Indexes, ix, kind)));
    }

    let key = |old: &ForeignKey| ForeignKey { columns: follow_names(&renamed, &old.columns), ..old.clone() };
    let changed_keys: HashSet<usize> =
        row_changes(&base.foreign_keys, &draft.foreign_keys, key).into_iter().map(|(ix, _)| ix).collect();
    let live_keys = draft.foreign_keys.iter().enumerate().filter(|(_, row)| !row.dropped);
    name_problems(Section::ForeignKeys, live_keys.map(|(ix, row)| (ix, row.value.name.as_str())), false, &mut out);
    for (ix, row) in draft.foreign_keys.iter().enumerate() {
        let fk = &row.value;
        let kind = if !editing.foreign_keys && changed_keys.contains(&ix) {
            Some(ProblemKind::Unsupported)
        } else if row.dropped {
            None
        } else if fk.ref_table.trim().is_empty() {
            Some(ProblemKind::NoReferencedTable)
        } else if fk.columns.is_empty() || (!fk.ref_columns.is_empty() && fk.ref_columns.len() != fk.columns.len()) {
            Some(ProblemKind::ForeignKeyColumnCount)
        } else {
            uses_dropped(&fk.columns).map(ProblemKind::UsesDroppedColumn)
        };
        out.extend(kind.map(|kind| problem(Section::ForeignKeys, ix, kind)));
    }

    let changed_checks: HashSet<usize> =
        row_changes(&base.checks, &draft.checks, Check::clone).into_iter().map(|(ix, _)| ix).collect();
    let live_checks = draft.checks.iter().enumerate().filter(|(_, row)| !row.dropped);
    name_problems(Section::Checks, live_checks.map(|(ix, row)| (ix, row.value.name.as_str())), false, &mut out);
    for (ix, row) in draft.checks.iter().enumerate() {
        if !editing.checks && changed_checks.contains(&ix) {
            out.push(problem(Section::Checks, ix, ProblemKind::Unsupported));
        } else if !row.dropped && row.value.expression.trim().is_empty() {
            out.push(problem(Section::Checks, ix, ProblemKind::EmptyExpression));
        }
    }
    out
}

pub fn plan_changes(driver: &DriverType, original: &TableStructure, draft: &Draft) -> Result<ChangePlan, Vec<Problem>> {
    let problems = validate(driver, Some(original), draft);
    if !problems.is_empty() {
        return Err(problems);
    }
    let found = changes(original, draft);
    if found.is_empty() {
        return Ok(ChangePlan::default());
    }
    Ok(match driver.dialect() {
        Some(SqlDialect::Postgres) => transaction(postgres::alter(driver, original, &found)),
        Some(SqlDialect::TSql) => tsql_transaction(tsql::alter(driver, original, &found)),
        Some(SqlDialect::MySql) => statements(mysql::alter(driver, original, &found)),
        Some(SqlDialect::ClickHouse) => statements(clickhouse::alter(driver, original, &found)),
        Some(SqlDialect::Sqlite) => match sqlite::alter(driver, original, &found, draft) {
            Some((steps, warnings)) => ChangePlan { steps, warnings },
            None => return Err(vec![Problem { section: Section::Table, row: 0, kind: ProblemKind::Unsupported }]),
        },
        _ => ChangePlan::default(),
    })
}

pub fn plan_create(driver: &DriverType, draft: &Draft) -> Result<ChangePlan, Vec<Problem>> {
    let problems = validate(driver, None, draft);
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(match driver.dialect() {
        Some(SqlDialect::Postgres) => transaction(postgres::create(driver, draft)),
        Some(SqlDialect::TSql) => tsql_transaction(tsql::create(driver, draft)),
        Some(SqlDialect::MySql) => statements(mysql::create(driver, draft)),
        Some(SqlDialect::ClickHouse) => statements(clickhouse::create(driver, draft)),
        Some(SqlDialect::Sqlite) => ChangePlan {
            steps: sqlite::create(driver, draft).into_iter().map(Step::sql).collect(),
            warnings: Vec::new(),
        },
        _ => ChangePlan::default(),
    })
}

fn transaction(statements: Vec<String>) -> ChangePlan {
    let steps = std::iter::once("BEGIN".to_string()).chain(statements).chain(std::iter::once("COMMIT".to_string()));
    ChangePlan { steps: steps.map(Step::sql).collect(), warnings: Vec::new() }
}

// For the engines that can't roll DDL back. More than one statement can stop halfway.
fn statements(statements: Vec<String>) -> ChangePlan {
    let warnings = if statements.len() > 1 { vec![Warning::NotAtomic] } else { Vec::new() };
    ChangePlan { steps: statements.into_iter().map(Step::sql).collect(), warnings }
}

// XACT_ABORT rolls the whole transaction back on the first error, which a batch-by-batch run needs.
fn tsql_transaction(statements: Vec<String>) -> ChangePlan {
    let open = ["SET XACT_ABORT ON", "BEGIN TRANSACTION"].map(String::from);
    let steps = open.into_iter().chain(statements).chain(std::iter::once("COMMIT TRANSACTION".to_string()));
    ChangePlan { steps: steps.map(Step::sql).collect(), warnings: Vec::new() }
}

// The ddl.rs form of a column, for the dialects that render through it.
fn ddl_column(column: &StructColumn) -> DdlColumn {
    let identity = match column.identity {
        Identity::ByDefault => "BY DEFAULT".to_string(),
        Identity::Always => "ALWAYS".to_string(),
        Identity::Seeded { seed, increment } => format!("{seed}, {increment}"),
        Identity::None | Identity::AutoIncrement => String::new(),
    };
    DdlColumn {
        name: column.name.clone(),
        data_type: column.data_type.clone(),
        not_null: !column.nullable,
        default: column.default.clone(),
        collation: column.collation.clone(),
        identity,
        generated: column.generated.clone(),
    }
}

// "CHECK ((age > 0))" -> "(age > 0)": the keyword and one pair of parentheses around the whole expression.
pub fn check_body(definition: &str) -> String {
    let mut text = definition.trim();
    let keyword = text.get(..5).is_some_and(|head| head.eq_ignore_ascii_case("check"));
    if keyword && !text[5..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        text = text[5..].trim_start();
    }
    let tokens = tokenize(text, None);
    let wrapped =
        tokens.first().is_some_and(|t| t.is_punct("(")) && tokens.last().is_some_and(|t| t.is_punct(")")) && {
            let mut depth = 0i32;
            let parens = tokens.iter().filter(|t| t.kind == TokenKind::Punct && (t.text == "(" || t.text == ")"));
            let closes: Vec<usize> = parens
                .filter_map(|t| {
                    depth += if t.text == "(" { 1 } else { -1 };
                    (depth == 0).then_some(t.start)
                })
                .collect();
            closes == [tokens.last().map_or(0, |t| t.start)]
        };
    if wrapped { text[1..text.len() - 1].trim().to_string() } else { text.to_string() }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    Index,
    ForeignKey,
    Default,
    PrimaryKey,
}

// What a new object is called until the user names it. Empty where the engine names it itself.
pub fn suggested_name(driver: &DriverType, kind: NameKind, table: &str, columns: &[String], target: &str) -> String {
    let cols = columns.join("_");
    let dialect = driver.dialect();
    let (pg, tsql) = (dialect == Some(SqlDialect::Postgres), dialect == Some(SqlDialect::TSql));
    match kind {
        NameKind::Index if pg => format!("{table}_{cols}_idx"),
        NameKind::Index if tsql => format!("IX_{table}_{cols}"),
        NameKind::Index => format!("idx_{table}_{cols}"),
        NameKind::ForeignKey if pg => format!("{table}_{cols}_fkey"),
        NameKind::ForeignKey if tsql => format!("FK_{table}_{target}"),
        NameKind::ForeignKey => format!("fk_{table}_{target}"),
        NameKind::Default if tsql => format!("DF_{table}_{cols}"),
        NameKind::PrimaryKey if tsql => format!("PK_{table}"),
        NameKind::Default | NameKind::PrimaryKey => String::new(),
    }
}

pub fn type_suggestions(driver: &DriverType) -> &'static [&'static str] {
    match driver.dialect() {
        Some(SqlDialect::Postgres) => &[
            "bigint",
            "integer",
            "smallint",
            "numeric(12,2)",
            "real",
            "double precision",
            "boolean",
            "text",
            "varchar(255)",
            "uuid",
            "date",
            "timestamp",
            "timestamptz",
            "jsonb",
            "bytea",
        ],
        Some(SqlDialect::MySql) => &[
            "INT",
            "BIGINT",
            "TINYINT(1)",
            "DECIMAL(12,2)",
            "DOUBLE",
            "VARCHAR(255)",
            "TEXT",
            "DATE",
            "DATETIME",
            "TIMESTAMP",
            "JSON",
            "BLOB",
        ],
        Some(SqlDialect::Sqlite) => &["INTEGER", "REAL", "TEXT", "BLOB", "NUMERIC"],
        Some(SqlDialect::TSql) => &[
            "int",
            "bigint",
            "bit",
            "decimal(18,2)",
            "float",
            "nvarchar(255)",
            "nvarchar(max)",
            "date",
            "datetime2",
            "uniqueidentifier",
            "varbinary(max)",
        ],
        Some(SqlDialect::ClickHouse) => &[
            "UInt64",
            "Int64",
            "Int32",
            "Float64",
            "Decimal(18,2)",
            "String",
            "LowCardinality(String)",
            "Date",
            "DateTime",
            "DateTime64(3)",
            "UUID",
            "Bool",
        ],
        None => &[],
    }
}

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use barsql_core::{Check, ForeignKey, Identity, StructColumn, StructIndex, TableStructure};

    use super::fixtures::*;
    use super::*;

    #[test]
    fn an_unchanged_draft_plans_nothing() {
        let plan = plan_changes(&PG, &users(), &Draft::from_structure(&users())).expect("valid");
        assert!(plan.is_empty());
    }

    #[test]
    fn renames_are_told_from_drop_and_add() {
        let mut draft = Draft::from_structure(&users());
        draft.rename_column(1, "full_name".into());
        let renamed = changes(&users(), &draft);
        assert_eq!(renamed.renamed, [("name".to_string(), "full_name".to_string())]);
        assert!(renamed.dropped.is_empty() && renamed.added.is_empty() && renamed.modified.is_empty());

        let mut draft = Draft::from_structure(&users());
        draft.columns[1].dropped = true;
        draft.columns.push(added(column("name", "text")));
        let replaced = changes(&users(), &draft);
        assert_eq!((replaced.dropped.len(), replaced.added.len()), (1, 1));
        assert!(replaced.renamed.is_empty());
    }

    #[test]
    fn a_renamed_column_keeps_its_index() {
        let mut original = users();
        original.indexes =
            vec![StructIndex { name: "users_name_idx".into(), columns: vec!["name".into()], ..Default::default() }];
        let mut draft = Draft::from_structure(&original);
        draft.rename_column(1, "full_name".into());
        assert_eq!(draft.indexes[0].value.columns, ["full_name"]);
        let found = changes(&original, &draft);
        assert!(found.dropped_indexes.is_empty() && found.added_indexes.is_empty());
    }

    #[test]
    fn primary_key_changes_keep_the_old_name() {
        let mut original = users();
        original.columns.push(StructColumn { nullable: false, ..column("num", "integer") });
        let mut draft = Draft::from_structure(&original);
        draft.columns[5].primary = true;
        assert_eq!(draft.primary_columns(), ["id", "num"]);
        assert_eq!(
            changes(&original, &draft).primary_key,
            Some((Some(pk("users_pkey", &["id"])), Some(pk("users_pkey", &["id", "num"]))))
        );
    }

    #[test]
    fn validation_reports_each_problem() {
        let problem = |section, row, kind| Problem { section, row, kind };
        let found =
            |driver: &DriverType, original: Option<&TableStructure>, draft: &Draft| validate(driver, original, draft);
        let original = users();
        let base = || Draft::from_structure(&original);

        let mut draft = Draft::new_table(&PG, "public");
        assert_eq!(found(&PG, None, &draft), [problem(Section::Table, 0, ProblemKind::EmptyTableName)]);
        draft.name = "people".into();
        assert!(found(&PG, None, &draft).is_empty());

        let mut draft = base();
        draft.columns[2].column.name = String::new();
        assert_eq!(found(&PG, Some(&original), &draft), [problem(Section::Columns, 2, ProblemKind::EmptyName)]);

        let mut draft = base();
        draft.columns.push(added(column("EMAIL", "text")));
        assert_eq!(found(&PG, Some(&original), &draft), [problem(Section::Columns, 5, ProblemKind::DuplicateName)]);

        let mut draft = base();
        draft.columns[3].column.data_type = " ".into();
        assert_eq!(found(&PG, Some(&original), &draft), [problem(Section::Columns, 3, ProblemKind::EmptyType)]);

        let mut draft = base();
        draft.columns.iter_mut().for_each(|row| row.dropped = true);
        assert_eq!(found(&PG, Some(&original), &draft), [problem(Section::Columns, 0, ProblemKind::NoColumns)]);

        let mut draft = base();
        draft.indexes.push(Row::added(StructIndex { name: "empty_idx".into(), ..Default::default() }));
        assert_eq!(found(&PG, Some(&original), &draft), [problem(Section::Indexes, 0, ProblemKind::NoIndexColumns)]);

        let mut draft = base();
        draft.columns[4].dropped = true;
        let index = StructIndex { name: "legacy_idx".into(), columns: vec!["legacy".into()], ..Default::default() };
        draft.indexes.push(Row::added(index));
        let uses = ProblemKind::UsesDroppedColumn("legacy".into());
        assert_eq!(found(&PG, Some(&original), &draft), [problem(Section::Indexes, 0, uses)]);

        let fk = |columns: &[&str], ref_table: &str, ref_columns: &[&str]| ForeignKey {
            name: "users_org_fkey".into(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            ref_table: ref_table.into(),
            ref_columns: ref_columns.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        };
        let mut draft = base();
        draft.foreign_keys.push(Row::added(fk(&["email"], "orgs", &["id", "code"])));
        let count = ProblemKind::ForeignKeyColumnCount;
        assert_eq!(found(&PG, Some(&original), &draft), [problem(Section::ForeignKeys, 0, count)]);

        let mut draft = base();
        draft.foreign_keys.push(Row::added(fk(&["email"], "", &["id"])));
        let missing = ProblemKind::NoReferencedTable;
        assert_eq!(found(&PG, Some(&original), &draft), [problem(Section::ForeignKeys, 0, missing)]);

        let mut draft = base();
        draft.checks.push(Row::added(Check { name: "ck".into(), expression: "  ".into() }));
        assert_eq!(found(&PG, Some(&original), &draft), [problem(Section::Checks, 0, ProblemKind::EmptyExpression)]);

        let mut draft = base();
        draft.foreign_keys.push(Row::added(fk(&["email"], "orgs", &["id"])));
        let unsupported = ProblemKind::Unsupported;
        assert_eq!(
            found(&DriverType::ClickHouse, Some(&original), &draft),
            [problem(Section::ForeignKeys, 0, unsupported.clone())]
        );

        let mut draft = base();
        draft.columns[0].column.identity = Identity::None;
        assert_eq!(found(&MS, Some(&original), &draft), [problem(Section::Columns, 0, unsupported)]);
    }

    #[test]
    fn script_terminates_statements_and_separates_tsql_batches() {
        let plan = ChangePlan { steps: vec![Step::sql("A"), Step::sql("B")], warnings: Vec::new() };
        assert_eq!(plan.script(&PG), "A;\n\nB;");
        assert_eq!(plan.script(&MS), "A;\nGO\n\nB;\nGO");
    }

    #[test]
    fn check_bodies_lose_their_wrapper() {
        assert_eq!(check_body("CHECK ((age > 0))"), "(age > 0)");
        assert_eq!(check_body("([a]>(0))"), "[a]>(0)");
        assert_eq!(check_body("age > 0"), "age > 0");
        assert_eq!(check_body("(a > 0) AND (b > 0)"), "(a > 0) AND (b > 0)");
    }

    #[test]
    fn new_tables_start_with_an_id_key() {
        let id = |driver: &DriverType| {
            let draft = Draft::new_table(driver, "s");
            assert_eq!((draft.name.as_str(), draft.columns.len()), ("", 1));
            let row = &draft.columns[0];
            assert!(row.primary && !row.column.nullable && row.origin.is_none());
            (row.column.name.clone(), row.column.data_type.clone(), row.column.identity)
        };
        assert_eq!(id(&PG), ("id".into(), "bigint".into(), Identity::ByDefault));
        assert_eq!(id(&DriverType::MySql), ("id".into(), "BIGINT".into(), Identity::AutoIncrement));
        assert_eq!(id(&DriverType::Sqlite), ("id".into(), "INTEGER".into(), Identity::None));
        assert_eq!(id(&DriverType::Turso), ("id".into(), "INTEGER".into(), Identity::None));
        assert_eq!(id(&MS), ("id".into(), "bigint".into(), Identity::Seeded { seed: 1, increment: 1 }));
        assert_eq!(id(&DriverType::ClickHouse), ("id".into(), "UInt64".into(), Identity::None));
    }

    #[test]
    fn names_follow_engine_conventions() {
        let email = ["email".to_string()];
        assert_eq!(suggested_name(&MS, NameKind::Index, "users", &email, ""), "IX_users_email");
        assert_eq!(suggested_name(&PG, NameKind::Index, "users", &email, ""), "users_email_idx");
        assert_eq!(suggested_name(&DriverType::MySql, NameKind::Index, "users", &email, ""), "idx_users_email");
        let org = ["org_id".to_string()];
        assert_eq!(suggested_name(&PG, NameKind::ForeignKey, "users", &org, "orgs"), "users_org_id_fkey");
        assert_eq!(suggested_name(&MS, NameKind::ForeignKey, "users", &org, "orgs"), "FK_users_orgs");
        assert_eq!(suggested_name(&MS, NameKind::Default, "users", &["status".into()], ""), "DF_users_status");
        assert_eq!(suggested_name(&MS, NameKind::PrimaryKey, "users", &[], ""), "PK_users");
        assert_eq!(suggested_name(&PG, NameKind::PrimaryKey, "users", &[], ""), "");
        assert!(type_suggestions(&DriverType::Turso).contains(&"BLOB"));
    }
}
