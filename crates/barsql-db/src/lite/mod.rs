// What SQLite and Turso share above their transports: the catalog and how values display. SQLite reaches its
// file through rusqlite and Turso a server over HTTP, and both answer the catalog's queries as a LiteSource.

pub(crate) mod catalog;
pub mod values;

use barsql_core::{QueryError, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum LiteValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl LiteValue {
    // SQLite has no booleans, so they bind as 0 and 1.
    pub fn from_value(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Integer(i64::from(*b)),
            Value::Int(i) => Self::Integer(*i),
            Value::Float(f) => Self::Real(*f),
            Value::Text(s) => Self::Text(s.clone()),
        }
    }

    pub fn as_ref(&self) -> LiteRef<'_> {
        match self {
            Self::Null => LiteRef::Null,
            Self::Integer(i) => LiteRef::Integer(*i),
            Self::Real(f) => LiteRef::Real(*f),
            Self::Text(s) => LiteRef::Text(s.as_bytes()),
            Self::Blob(b) => LiteRef::Blob(b),
        }
    }
}

// A value as it's displayed, borrowed from a row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LiteRef<'a> {
    Null,
    Integer(i64),
    Real(f64),
    Text(&'a [u8]),
    Blob(&'a [u8]),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiteRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<LiteValue>>,
}

// One statement in, all its rows out. Catalog queries return a handful of rows, so nothing streams.
pub trait LiteSource {
    fn query(&mut self, sql: &str, params: &[LiteValue]) -> Result<LiteRows, QueryError>;
}

// Reads of a catalog row's cells. A missing cell or NULL reads as empty.
pub(crate) trait LiteRow {
    fn text(&self, i: usize) -> String;
    fn opt_text(&self, i: usize) -> Option<String>;
    fn int(&self, i: usize) -> i64;
}

impl LiteRow for [LiteValue] {
    fn text(&self, i: usize) -> String {
        match self.get(i) {
            Some(LiteValue::Text(s)) => s.clone(),
            Some(LiteValue::Integer(n)) => n.to_string(),
            Some(LiteValue::Real(f)) => f.to_string(),
            Some(LiteValue::Blob(b)) => String::from_utf8_lossy(b).into_owned(),
            Some(LiteValue::Null) | None => String::new(),
        }
    }

    fn opt_text(&self, i: usize) -> Option<String> {
        match self.get(i) {
            Some(LiteValue::Null) | None => None,
            Some(_) => Some(self.text(i)),
        }
    }

    fn int(&self, i: usize) -> i64 {
        match self.get(i) {
            Some(LiteValue::Integer(n)) => *n,
            Some(LiteValue::Real(f)) => *f as i64,
            Some(LiteValue::Text(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }
}

// A value inlined into SQL, for looking a row up by what was just written.
pub(crate) fn literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Bool(b) => i64::from(*b).to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{f:?}"),
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
    }
}
