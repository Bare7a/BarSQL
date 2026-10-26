use barsql_core::{Check, DriverType, ForeignKey, Identity, PrimaryKey, StructColumn, StructIndex, TableStructure};

use super::{ChangePlan, ColumnRow};

pub const PG: DriverType = DriverType::Postgres;
pub const MS: DriverType = DriverType::SqlServer;

pub fn column(name: &str, data_type: &str) -> StructColumn {
    StructColumn { name: name.into(), data_type: data_type.into(), nullable: true, ..Default::default() }
}

pub fn pk(name: &str, columns: &[&str]) -> PrimaryKey {
    PrimaryKey { name: name.into(), columns: columns.iter().map(|c| c.to_string()).collect() }
}

pub fn users() -> TableStructure {
    TableStructure {
        schema: "public".into(),
        name: "users".into(),
        columns: vec![
            StructColumn { nullable: false, identity: Identity::ByDefault, ..column("id", "bigint") },
            column("name", "text"),
            column("email", "text"),
            StructColumn { default: "'new'::text".into(), ..column("status", "text") },
            column("legacy", "text"),
        ],
        primary_key: Some(pk("users_pkey", &["id"])),
        ..Default::default()
    }
}

pub fn index(name: &str, columns: &[&str], unique: bool) -> StructIndex {
    StructIndex {
        name: name.into(),
        columns: columns.iter().map(|c| c.to_string()).collect(),
        unique,
        ..Default::default()
    }
}

pub fn check(name: &str, expression: &str) -> Check {
    Check { name: name.into(), expression: expression.into() }
}

pub fn foreign_key(name: &str, columns: &[&str], ref_table: &str, ref_columns: &[&str]) -> ForeignKey {
    let names = |list: &[&str]| list.iter().map(|c| c.to_string()).collect();
    ForeignKey {
        name: name.into(),
        columns: names(columns),
        ref_table: ref_table.into(),
        ref_columns: names(ref_columns),
        ..Default::default()
    }
}

pub fn added(column: StructColumn) -> ColumnRow {
    ColumnRow { origin: None, dropped: false, column, primary: false }
}

pub fn sql(plan: &ChangePlan) -> Vec<&str> {
    plan.steps.iter().map(|step| step.sql.as_str()).collect()
}
