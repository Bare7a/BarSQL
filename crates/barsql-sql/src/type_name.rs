use barsql_core::SqlDialect;

// How a column's type is shown: in the tree, grid headers, the ER diagram and completions. Hover cards keep the
// engine's own spelling. ClickHouse types are case-sensitive, so they stay as they are.
pub fn display_type(dialect: Option<SqlDialect>, raw: &str) -> String {
    let raw = raw.trim();
    match dialect {
        Some(SqlDialect::ClickHouse) => raw.to_string(),
        Some(SqlDialect::Postgres) => postgres(&lowercase_unquoted(raw)),
        _ => lowercase_unquoted(raw),
    }
}

// A quoted name keeps its case: `"Mood"` is not `mood`.
fn lowercase_unquoted(raw: &str) -> String {
    let mut quoted = false;
    raw.chars()
        .map(|c| {
            quoted ^= c == '"';
            if quoted { c } else { c.to_ascii_lowercase() }
        })
        .collect()
}

// A result's types arrive by their internal names (`int4`, `_text` for an array), the catalog's in full
// (`character varying(50)[]`). Both end up as you'd write them in DDL, the long ones shortened.
fn postgres(name: &str) -> String {
    if let Some(element) = name.strip_prefix('_') {
        return format!("{}[]", postgres(element));
    }
    let base = name.trim_end_matches("[]");
    let dims = &name[base.len()..];
    let short = match base {
        "int2" => "smallint".to_string(),
        "int4" => "integer".to_string(),
        "int8" => "bigint".to_string(),
        "bool" => "boolean".to_string(),
        "float4" => "real".to_string(),
        "float8" => "double precision".to_string(),
        "bpchar" => "char".to_string(),
        _ => shorten(base),
    };
    format!("{short}{dims}")
}

fn shorten(base: &str) -> String {
    for (long, short) in [("character varying", "varchar"), ("bit varying", "varbit"), ("character", "char")] {
        if let Some(rest) = base.strip_prefix(long).filter(|rest| rest.is_empty() || rest.starts_with('(')) {
            return format!("{short}{rest}");
        }
    }
    // `timestamp(3) with time zone` is `timestamptz(3)`. Timestamp first, since it also starts with time.
    for (stem, zoned) in [("timestamp", "timestamptz"), ("time", "timetz")] {
        let Some(rest) = base.strip_prefix(stem) else { continue };
        let precision = rest.find(')').filter(|_| rest.starts_with('(')).map_or("", |end| &rest[..=end]);
        match &rest[precision.len()..] {
            " without time zone" => return format!("{stem}{precision}"),
            " with time zone" => return format!("{zoned}{precision}"),
            _ => {}
        }
    }
    base.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_catalog_spellings_are_shortened() {
        let cases = [
            ("character varying(255)", "varchar(255)"),
            ("character varying", "varchar"),
            ("character(10)", "char(10)"),
            ("bit varying(8)", "varbit(8)"),
            ("timestamp without time zone", "timestamp"),
            ("timestamp(3) with time zone", "timestamptz(3)"),
            ("time without time zone", "time"),
            ("time with time zone", "timetz"),
            ("character varying(50)[]", "varchar(50)[]"),
            ("integer", "integer"),
            ("double precision", "double precision"),
            ("numeric(10,2)", "numeric(10,2)"),
            ("\"Mood\"", "\"Mood\""),
        ];
        for (raw, want) in cases {
            assert_eq!(display_type(Some(SqlDialect::Postgres), raw), want, "{raw:?}");
        }
    }

    #[test]
    fn postgres_result_types_read_like_the_catalog() {
        let cases = [
            ("INT4", "integer"),
            ("INT8", "bigint"),
            ("INT2", "smallint"),
            ("BOOL", "boolean"),
            ("FLOAT4", "real"),
            ("FLOAT8", "double precision"),
            ("BPCHAR", "char"),
            ("VARCHAR", "varchar"),
            ("TIMESTAMPTZ", "timestamptz"),
            ("_TEXT", "text[]"),
            ("_INT4", "integer[]"),
            ("1266", "1266"),
        ];
        for (raw, want) in cases {
            assert_eq!(display_type(Some(SqlDialect::Postgres), raw), want, "{raw:?}");
        }
    }

    #[test]
    fn other_engines_are_lowercased_but_clickhouse_keeps_its_case() {
        let cases = [
            (Some(SqlDialect::Sqlite), "VARCHAR(255)", "varchar(255)"),
            (Some(SqlDialect::MySql), "INT UNSIGNED", "int unsigned"),
            (Some(SqlDialect::TSql), "NVARCHAR(50)", "nvarchar(50)"),
            (Some(SqlDialect::ClickHouse), "Nullable(String)", "Nullable(String)"),
            (Some(SqlDialect::ClickHouse), "DateTime64(3)", "DateTime64(3)"),
            (None, "TEXT", "text"),
        ];
        for (dialect, raw, want) in cases {
            assert_eq!(display_type(dialect, raw), want, "{dialect:?} {raw:?}");
        }
    }
}
