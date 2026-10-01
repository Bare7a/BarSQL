use serde_json::Value;

// UI strings from `editor.sql.*` in the locale files. Templates use `{{name}}` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlLabels {
    pub pk: String,
    pub fk: String,
    pub not_null: String,
    pub table: String,
    pub view: String,
    pub schema: String,
    pub column: String,
    pub cte: String,
    pub subquery: String,
    pub cte_column: String,
    pub subquery_column: String,
    pub foreign_key: String,
    pub alias_arrow: String,
    pub schema_table: String,
    pub unknown_table: String,
    pub column_of: String,
    pub alias_for: String,
}

const KEYS: [&str; 17] = [
    "pk",
    "fk",
    "notNull",
    "table",
    "view",
    "schema",
    "column",
    "cte",
    "subquery",
    "cteColumn",
    "subqueryColumn",
    "foreignKey",
    "aliasArrow",
    "schemaTable",
    "unknownTable",
    "columnOf",
    "aliasFor",
];

const ENGLISH: [&str; 17] = [
    "PK",
    "FK",
    "not null",
    "table",
    "view",
    "schema",
    "column",
    "CTE",
    "subquery",
    "CTE column",
    "subquery column",
    "foreign key",
    "alias → {{table}}",
    "{{schema}} · {{type}}",
    "Unknown table \"{{table}}\"",
    "column of {{target}}",
    "alias for {{table}}",
];

impl Default for SqlLabels {
    fn default() -> Self {
        Self::from_values(|i| ENGLISH[i].to_string())
    }
}

impl SqlLabels {
    // `root` is the whole locale file. Missing keys fall back to English.
    pub fn from_locale(root: &Value) -> Self {
        let sql = &root["editor"]["sql"];
        Self::from_values(|i| sql[KEYS[i]].as_str().unwrap_or(ENGLISH[i]).to_string())
    }

    fn from_values(value: impl Fn(usize) -> String) -> Self {
        Self {
            pk: value(0),
            fk: value(1),
            not_null: value(2),
            table: value(3),
            view: value(4),
            schema: value(5),
            column: value(6),
            cte: value(7),
            subquery: value(8),
            cte_column: value(9),
            subquery_column: value(10),
            foreign_key: value(11),
            alias_arrow: value(12),
            schema_table: value(13),
            unknown_table: value(14),
            column_of: value(15),
            alias_for: value(16),
        }
    }
}

// Fills `{{name}}` placeholders, without escaping.
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    out
}
