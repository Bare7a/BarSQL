use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub is_primary: bool,
    pub is_foreign: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub foreign_table: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub foreign_column: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_val: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    #[default]
    Table,
    View,
    MaterializedView,
    Index,
    Constraint,
    Trigger,
    Function,
    Procedure,
    Other(String),
}

impl ObjectKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Table => "table",
            Self::View => "view",
            Self::MaterializedView => "materialized view",
            Self::Index => "index",
            Self::Constraint => "constraint",
            Self::Trigger => "trigger",
            Self::Function => "function",
            Self::Procedure => "procedure",
            Self::Other(kind) => kind,
        }
    }

    pub fn parse(kind: &str) -> Self {
        match kind {
            "table" => Self::Table,
            "view" => Self::View,
            "materialized view" => Self::MaterializedView,
            "index" => Self::Index,
            "constraint" => Self::Constraint,
            "trigger" => Self::Trigger,
            "function" => Self::Function,
            "procedure" => Self::Procedure,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn is_relation(&self) -> bool {
        matches!(self, Self::Table | Self::View | Self::MaterializedView)
    }

    pub fn for_relation(table_type: &str) -> Self {
        match table_type.to_lowercase().as_str() {
            "view" => Self::View,
            "materialized view" => Self::MaterializedView,
            _ => Self::Table,
        }
    }
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ObjectKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ObjectKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(deserializer)?))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexInfo {
    pub name: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub is_primary: bool,
    pub is_unique: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub method: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintInfo {
    pub name: String,
    pub schema: String,
    pub table: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ref_table: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ref_columns: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub definition: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerInfo {
    pub name: String,
    pub schema: String,
    pub table: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub timing: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub events: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineInfo {
    pub name: String,
    pub schema: String,
    pub kind: ObjectKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub return_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
}

// `table` is only set for indexes, constraints and triggers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectRef {
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub table: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaInfo {
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaTables {
    pub schema: String,
    pub tables: Vec<TableInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaBundle {
    pub status: ConnectionStatus,
    pub schemas: Vec<SchemaInfo>,
    pub loaded_tables: Vec<SchemaTables>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub database: String,
    pub schema: String,
    pub user: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
}

pub fn primary_keys(columns: &[ColumnInfo]) -> Vec<String> {
    columns.iter().filter(|c| c.is_primary).map(|c| c.name.clone()).collect()
}

pub fn column_exists(columns: &[ColumnInfo], name: &str) -> bool {
    columns.iter().any(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, primary: bool) -> ColumnInfo {
        ColumnInfo { name: name.into(), is_primary: primary, ..Default::default() }
    }

    #[test]
    fn primary_keys_keep_column_order() {
        assert_eq!(
            primary_keys(&[column("id", true), column("name", false), column("role_id", true)]),
            ["id", "role_id"]
        );
        assert!(primary_keys(&[column("x", false)]).is_empty());
        assert!(primary_keys(&[]).is_empty());
    }
}
