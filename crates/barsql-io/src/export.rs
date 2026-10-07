use barsql_core::SqlDialect;
use barsql_db::Cell;
use barsql_sql::{Dialect, quote_ident_in, quote_literal_in};
use serde_json::Value as Json;

pub const EXPORT_CHUNK_ROWS: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    Text,
    Csv,
    Json,
    Markdown,
    Sql,
}

pub const EXPORT_FORMATS: [ExportFormat; 5] =
    [ExportFormat::Text, ExportFormat::Csv, ExportFormat::Json, ExportFormat::Markdown, ExportFormat::Sql];

impl ExportFormat {
    pub fn id(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::Sql => "sql",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Csv => "CSV",
            Self::Json => "JSON",
            Self::Markdown => "Markdown",
            Self::Sql => "SQL INSERT",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Text => "txt",
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Markdown => "md",
            Self::Sql => "sql",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        EXPORT_FORMATS.into_iter().find(|f| f.id() == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportChunk {
    pub text: String,
    // Rows in this chunk, not cumulative.
    pub rows: usize,
}

// Emits a chunk every `rows_per_chunk` rows. Joining every chunk's text gives the whole export.
pub struct Exporter {
    format: ExportFormat,
    columns: Vec<String>,
    json_columns: Vec<bool>,
    sql_prefix: String,
    // SQL INSERTs are written for this dialect, or as ANSI SQL without one.
    dialect: Option<SqlDialect>,
    pending: String,
    pending_rows: usize,
    written: usize,
    rows_per_chunk: usize,
}

impl Exporter {
    pub fn new(
        format: ExportFormat,
        columns: &[String],
        column_types: &[String],
        table_name: Option<&str>,
        dialect: Option<SqlDialect>,
        rows_per_chunk: usize,
    ) -> Self {
        let header = match format {
            ExportFormat::Csv => columns.iter().map(|c| escape_csv(c)).collect::<Vec<_>>().join(","),
            ExportFormat::Markdown => format!(
                "| {} |\n| {} |",
                columns.iter().map(|c| md_cell(c)).collect::<Vec<_>>().join(" | "),
                columns.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")
            ),
            _ => String::new(),
        };
        let quote = |id: &str| match dialect {
            Some(dialect) => quote_ident_in(Dialect::of(dialect), id),
            None => format!("\"{}\"", id.replace('"', "\"\"")),
        };
        let table = table_name.filter(|t| !t.is_empty()).unwrap_or("results");
        let sql_prefix = format!(
            "INSERT INTO {} ({}) VALUES (",
            quote(table),
            columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ")
        );
        // JSON columns nest instead of being string-wrapped, as in the row JSON viewer.
        let json_columns = (0..columns.len())
            .map(|i| column_types.get(i).is_some_and(|t| t.to_ascii_lowercase().contains("json")))
            .collect();
        Self {
            format,
            columns: columns.to_vec(),
            json_columns,
            sql_prefix,
            dialect,
            pending: header,
            pending_rows: 0,
            written: 0,
            rows_per_chunk: rows_per_chunk.max(1),
        }
    }

    pub fn push(&mut self, cells: &[Cell<'_>]) -> Option<ExportChunk> {
        let (open, sep) = match self.format {
            ExportFormat::Json => ("[\n", ",\n"),
            ExportFormat::Csv | ExportFormat::Markdown => ("\n", "\n"),
            ExportFormat::Sql | ExportFormat::Text => ("", "\n"),
        };
        self.pending.push_str(if self.written == 0 { open } else { sep });
        let row = self.format_row(cells);
        self.pending.push_str(&row);
        self.written += 1;
        self.pending_rows += 1;
        if self.written.is_multiple_of(self.rows_per_chunk) {
            return Some(self.take());
        }
        None
    }

    pub fn finish(mut self) -> Option<ExportChunk> {
        let (close, empty) = match self.format {
            ExportFormat::Json => ("\n]", "[]"),
            _ => ("", ""),
        };
        self.pending.push_str(if self.written == 0 { empty } else { close });
        (!self.pending.is_empty()).then(|| self.take())
    }

    fn take(&mut self) -> ExportChunk {
        let chunk = ExportChunk { text: std::mem::take(&mut self.pending), rows: self.pending_rows };
        self.pending_rows = 0;
        chunk
    }

    fn format_row(&self, cells: &[Cell<'_>]) -> String {
        match self.format {
            ExportFormat::Text => {
                cells.iter().map(|c| cell_text(*c).unwrap_or_default()).collect::<Vec<_>>().join("\t")
            }
            ExportFormat::Csv => cells
                .iter()
                .map(|c| cell_text(*c).map(|s| escape_csv(&defuse_csv_formula(&s))).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(","),
            ExportFormat::Markdown => {
                format!(
                    "| {} |",
                    cells
                        .iter()
                        .map(|c| cell_text(*c).map(|s| md_cell(&s)).unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join(" | ")
                )
            }
            ExportFormat::Sql => {
                let values = cells.iter().map(|c| sql_literal(*c, self.dialect)).collect::<Vec<_>>();
                format!("{}{});", self.sql_prefix, values.join(", "))
            }
            ExportFormat::Json => {
                let mut object = JsonObject::default();
                for (i, column) in self.columns.iter().enumerate() {
                    let cell = cells.get(i).copied().unwrap_or(Cell::Null);
                    let value = match cell {
                        Cell::Text(s) if self.json_columns[i] => serde_json::from_str::<Json>(s)
                            .map(JsonValue::from_json)
                            .unwrap_or_else(|_| JsonValue::String(s.to_string())),
                        other => JsonValue::from_cell(other),
                    };
                    object.insert(column.clone(), value);
                }
                let mut out = String::new();
                JsonValue::Object(object).write(&mut out, "");
                format!("  {}", out.replace('\n', "\n  "))
            }
        }
    }
}

pub fn export_to_string<'a>(
    format: ExportFormat,
    columns: &[String],
    column_types: &[String],
    table_name: Option<&str>,
    dialect: Option<SqlDialect>,
    rows: impl IntoIterator<Item = Vec<Cell<'a>>>,
) -> String {
    let mut exporter = Exporter::new(format, columns, column_types, table_name, dialect, EXPORT_CHUNK_ROWS);
    let mut out = String::new();
    for row in rows {
        if let Some(chunk) = exporter.push(&row) {
            out.push_str(&chunk.text);
        }
    }
    if let Some(chunk) = exporter.finish() {
        out.push_str(&chunk.text);
    }
    out
}

// A duplicated name resolves to its first column.
pub fn resolve_columns(all: &[String], wanted: &[String]) -> Vec<usize> {
    wanted.iter().filter_map(|w| all.iter().position(|c| c == w)).collect()
}

// Same text the grid shows. None for NULL.
pub fn cell_text(cell: Cell<'_>) -> Option<String> {
    cell.display().map(str::to_string)
}

pub fn escape_csv(cell: &str) -> String {
    // Quote '' too so re-import can tell it apart from NULL.
    let needs_quote = cell.is_empty()
        || cell == "\\."
        || cell.contains(['"', ',', '\n', '\r'])
        || cell.chars().next().is_some_and(is_space);
    if needs_quote { format!("\"{}\"", cell.replace('"', "\"\"")) } else { cell.to_string() }
}

// Defuses formula injection in cells starting with = + - @, but leaves plain numbers like -5 alone.
pub fn defuse_csv_formula(s: &str) -> String {
    if !s.starts_with(['=', '+', '-', '@']) || number_is_finite(s) {
        return s.to_string();
    }
    format!("'{s}")
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace("\r\n", " ").replace(['\r', '\n'], " ")
}

fn sql_literal(cell: Cell<'_>, dialect: Option<SqlDialect>) -> String {
    match (cell, dialect) {
        (Cell::Null, _) => "NULL".into(),
        // T-SQL has no TRUE or FALSE, and its bit columns take 1 and 0.
        (Cell::Bool(b), Some(SqlDialect::TSql)) => if b { "1" } else { "0" }.into(),
        (Cell::Bool(true), _) => "TRUE".into(),
        (Cell::Bool(false), _) => "FALSE".into(),
        (Cell::Number(s), _) => s.to_string(),
        (Cell::Text(s), Some(dialect)) => quote_literal_in(dialect, s),
        (Cell::Text(s), None) => format!("'{}'", s.replace('\'', "''")),
    }
}

// Matches \t \n \v \f \r, the Zs space separators, U+2028, U+2029 and U+FEFF.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

// True for blank text or a finite decimal number. No hex, no Infinity.
fn number_is_finite(s: &str) -> bool {
    let trimmed = s.trim_matches(is_space);
    if trimmed.is_empty() {
        return true;
    }
    let unsigned = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(ix) => (&unsigned[..ix], Some(&unsigned[ix + 1..])),
        None => (unsigned, None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = digits(int) && digits(frac) && !(int.is_empty() && frac.is_empty());
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    mantissa_ok && exponent_ok && trimmed.parse::<f64>().is_ok_and(f64::is_finite)
}

// Two-space indents when `pretty`, compact otherwise. None if the text isn't valid JSON.
pub fn restringify_json(text: &str, pretty: bool) -> Option<String> {
    let value = JsonValue::from_json(serde_json::from_str::<Json>(text).ok()?);
    let mut out = String::new();
    if pretty {
        value.write(&mut out, "");
    } else {
        value.write_compact(&mut out);
    }
    Some(out)
}

// Renders with two-space indents and writes non-finite numbers as null.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    // Already formatted, straight from the grid.
    NumberText(String),
    String(String),
    Array(Vec<JsonValue>),
    Object(JsonObject),
}

// Integer-index keys come first in ascending order, then the rest in insertion order. A repeated key
// keeps its first position but its last value.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct JsonObject {
    entries: Vec<(String, JsonValue)>,
}

impl JsonObject {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn insert(&mut self, key: String, value: JsonValue) {
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    pub(crate) fn ordered(&self) -> Vec<&(String, JsonValue)> {
        let mut indices: Vec<&(String, JsonValue)> =
            self.entries.iter().filter(|(k, _)| array_index(k).is_some()).collect();
        indices.sort_by_key(|(k, _)| array_index(k));
        indices.extend(self.entries.iter().filter(|(k, _)| array_index(k).is_none()));
        indices
    }
}

fn array_index(key: &str) -> Option<u32> {
    let n: u32 = key.parse().ok()?;
    (n != u32::MAX && n.to_string() == key).then_some(n)
}

impl JsonValue {
    pub(crate) fn from_cell(cell: Cell<'_>) -> Self {
        match cell {
            Cell::Null => Self::Null,
            Cell::Bool(b) => Self::Bool(b),
            Cell::Number(s) => Self::NumberText(s.to_string()),
            Cell::Text(s) => Self::String(s.to_string()),
        }
    }

    pub(crate) fn from_json(json: Json) -> Self {
        match json {
            Json::Null => Self::Null,
            Json::Bool(b) => Self::Bool(b),
            Json::Number(n) => Self::Number(n.as_f64().unwrap_or(f64::NAN)),
            Json::String(s) => Self::String(s),
            Json::Array(items) => Self::Array(items.into_iter().map(Self::from_json).collect()),
            Json::Object(map) => {
                let mut object = JsonObject::default();
                for (k, v) in map {
                    object.insert(k, Self::from_json(v));
                }
                Self::Object(object)
            }
        }
    }

    pub(crate) fn write(&self, out: &mut String, indent: &str) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Number(f) if f.is_finite() => out.push_str(ryu_js::Buffer::new().format(*f)),
            Self::Number(_) => out.push_str("null"),
            Self::NumberText(s) if matches!(s.as_str(), "NaN" | "Infinity" | "-Infinity") => out.push_str("null"),
            Self::NumberText(s) => out.push_str(s),
            Self::String(s) => write_json_string(out, s),
            Self::Array(items) if items.is_empty() => out.push_str("[]"),
            Self::Array(items) => {
                let inner = format!("{indent}  ");
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(",\n");
                    }
                    out.push_str(&inner);
                    item.write(out, &inner);
                }
                out.push('\n');
                out.push_str(indent);
                out.push(']');
            }
            Self::Object(object) if object.entries.is_empty() => out.push_str("{}"),
            Self::Object(object) => {
                let inner = format!("{indent}  ");
                out.push_str("{\n");
                for (i, (key, value)) in object.ordered().into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(",\n");
                    }
                    out.push_str(&inner);
                    write_json_string(out, key);
                    out.push_str(": ");
                    value.write(out, &inner);
                }
                out.push('\n');
                out.push_str(indent);
                out.push('}');
            }
        }
    }
}

impl JsonValue {
    pub(crate) fn write_compact(&self, out: &mut String) {
        match self {
            Self::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_compact(out);
                }
                out.push(']');
            }
            Self::Object(object) => {
                out.push('{');
                for (i, (key, value)) in object.ordered().into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(out, key);
                    out.push(':');
                    value.write_compact(out);
                }
                out.push('}');
            }
            scalar => scalar.write(out, ""),
        }
    }
}

fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_restringifies() {
        let text = r#" {"b": 1.50, "2": [true, null, -0, 1e21], "a": {"x": "q\"\u0001"}, "1": {}, "b": 2} "#;
        assert_eq!(
            restringify_json(text, true).unwrap(),
            "{\n  \"1\": {},\n  \"2\": [\n    true,\n    null,\n    0,\n    1e+21\n  ],\n  \"b\": 2,\n  \"a\": {\n    \"x\": \"q\\\"\\u0001\"\n  }\n}"
        );
        assert_eq!(
            restringify_json(text, false).unwrap(),
            r#"{"1":{},"2":[true,null,0,1e+21],"b":2,"a":{"x":"q\"\u0001"}}"#
        );
        assert_eq!(restringify_json("[]", true).unwrap(), "[]");
        assert_eq!(restringify_json("[1,]", true), None);
    }

    #[test]
    fn sql_export_doubles_quotes_in_identifiers() {
        let out = export_to_string(
            ExportFormat::Sql,
            &["wei\"rd".into()],
            &[String::new()],
            Some("tab\"le"),
            None,
            [vec![Cell::Number("1")]],
        );
        assert!(out.contains(r#"INSERT INTO "tab""le" ("wei""rd") VALUES (1)"#), "{out}");
    }

    #[test]
    fn sql_export_writes_the_connections_dialect() {
        let row = || [vec![Cell::Text(r"it's a\b"), Cell::Bool(true), Cell::Number("2"), Cell::Null]];
        let columns: Vec<String> = ["note", "flag", "n", "gone"].map(String::from).to_vec();
        let insert = |dialect| export_to_string(ExportFormat::Sql, &columns, &[], Some("t"), Some(dialect), row());
        let cases = [
            (
                SqlDialect::Postgres,
                r#"INSERT INTO "t" ("note", "flag", "n", "gone") VALUES (E'it''s a\\b', TRUE, 2, NULL);"#,
            ),
            (
                SqlDialect::MySql,
                "INSERT INTO `t` (`note`, `flag`, `n`, `gone`) VALUES (CONVERT(X'6974277320615C62' USING utf8mb4), TRUE, 2, NULL);",
            ),
            (
                SqlDialect::Sqlite,
                r#"INSERT INTO "t" ("note", "flag", "n", "gone") VALUES ('it''s a\b', TRUE, 2, NULL);"#,
            ),
            (SqlDialect::TSql, r"INSERT INTO [t] ([note], [flag], [n], [gone]) VALUES (N'it''s a\b', 1, 2, NULL);"),
            (
                SqlDialect::ClickHouse,
                r"INSERT INTO `t` (`note`, `flag`, `n`, `gone`) VALUES ('it\'s a\\b', TRUE, 2, NULL);",
            ),
        ];
        for (dialect, expected) in cases {
            assert_eq!(insert(dialect), expected, "{dialect:?}");
        }
    }

    #[test]
    fn markdown_export_flattens_carriage_returns() {
        let out = export_to_string(
            ExportFormat::Markdown,
            &["v".into()],
            &[String::new()],
            None,
            None,
            [vec![Cell::Text("a\r\nb")], vec![Cell::Text("c\rd")]],
        );
        assert!(out.contains("| a b |") && out.contains("| c d |") && !out.contains('\r'), "{out:?}");
    }

    #[test]
    fn formula_defusing_leaves_numbers_alone() {
        let cases = [
            ("=SUM(A1)", "'=SUM(A1)"),
            ("+1", "+1"),
            ("-5", "-5"),
            ("-x", "'-x"),
            ("@cmd", "'@cmd"),
            ("-1e3", "-1e3"),
            ("-.5", "-.5"),
            ("-5.", "-5."),
            ("-0x10", "'-0x10"),
            ("-Infinity", "'-Infinity"),
            ("-1e400", "'-1e400"),
            ("- 5", "'- 5"),
            ("-5 ", "-5 "),
            ("-", "'-"),
        ];
        for (input, want) in cases {
            assert_eq!(defuse_csv_formula(input), want, "{input:?}");
        }
    }

    #[test]
    fn object_key_order() {
        let mut object = JsonObject::default();
        for key in ["b", "10", "a", "2", "01", "b"] {
            object.insert(key.to_string(), JsonValue::Null);
        }
        let keys: Vec<&str> = object.ordered().iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["2", "10", "b", "a", "01"]);
    }
}
