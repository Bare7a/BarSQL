use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryError {
    pub message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub position: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub severity: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cancelled: bool,
}

impl QueryError {
    pub fn message(message: impl Into<String>) -> Self {
        Self { message: message.into(), ..Default::default() }
    }

    pub fn cancelled() -> Self {
        Self { message: "query cancelled".into(), cancelled: true, ..Default::default() }
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QueryError {}

impl From<String> for QueryError {
    fn from(message: String) -> Self {
        Self::message(message)
    }
}

impl From<&str> for QueryError {
    fn from(message: &str) -> Self {
        Self::message(message)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Self::Text(text.to_string())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

pub type Row = BTreeMap<String, Value>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageLevel {
    #[default]
    Info,
    Notice,
    Warning,
}

// What a server says about a statement besides its result: Postgres notices, MySQL warnings and notes, and
// BarSQL's own notes about how it ran the statement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerMessage {
    pub level: MessageLevel,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
}

impl ServerMessage {
    pub fn new(level: MessageLevel, text: impl Into<String>) -> Self {
        Self { level, text: text.into(), ..Default::default() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSummary {
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    pub row_count: i64,
    pub affected_rows: i64,
    pub duration_ms: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primary_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub table_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub schema_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDataRequest {
    pub schema: String,
    pub table: String,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub limit: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub order_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub order_dir: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub filter: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowUpdate {
    pub schema: String,
    pub table: String,
    pub primary_key: Row,
    pub changes: Row,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowDelete {
    pub schema: String,
    pub table: String,
    pub primary_keys: Vec<Row>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub connection_id: String,
    #[serde(default)]
    pub sql: String,
    #[serde(default)]
    pub executed_at: String,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub success: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedQuery {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub connection_id: String,
    #[serde(default)]
    pub sql: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}
