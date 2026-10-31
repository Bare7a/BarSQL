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

// What a column's values are, as far as its type says, for the tree's column icons. Keys and the values a column
// takes from a fixed list are the column's to say, not its type's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeFamily {
    Number,
    Text,
    Boolean,
    Date,
    Time,
    DateTime,
    Json,
    Uuid,
    Binary,
    Array,
    Enum,
    Other,
}

// By the name the tree shows, inside ClickHouse's Nullable(…) and LowCardinality(…), and without MySQL's
// unsigned or zerofill.
pub fn type_family(dialect: Option<SqlDialect>, raw: &str) -> TypeFamily {
    let mut name = display_type(dialect, raw).to_lowercase();
    while let Some(inner) = ["nullable(", "lowcardinality("]
        .iter()
        .find_map(|wrapper| name.strip_prefix(wrapper))
        .and_then(|rest| rest.strip_suffix(')'))
    {
        name = inner.to_string();
    }
    if name.ends_with("[]") || name.starts_with("array(") {
        return TypeFamily::Array;
    }
    let mut base = name.split('(').next().unwrap_or_default().trim();
    while let Some(rest) = [" unsigned", " signed", " zerofill"].iter().find_map(|word| base.strip_suffix(word)) {
        base = rest.trim_end();
    }
    match (dialect, base) {
        // MySQL's BOOLEAN is tinyint(1), and bit(1) often stands in for one. SQL Server's bit is its boolean.
        (Some(SqlDialect::MySql), "tinyint" | "bit") if name.starts_with(&format!("{base}(1)")) => TypeFamily::Boolean,
        (Some(SqlDialect::TSql), "bit") => TypeFamily::Boolean,
        (Some(SqlDialect::Sqlite), _) => named(base).or_else(|| affinity(base)).unwrap_or(TypeFamily::Other),
        _ => named(base).unwrap_or(TypeFamily::Other),
    }
}

fn named(base: &str) -> Option<TypeFamily> {
    use TypeFamily::*;
    // ClickHouse's Int32, UInt64, Float64 and Decimal64, Postgres's int4 and float8.
    let sized =
        |stem: &str| base.strip_prefix(stem).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    Some(match base {
        "smallint" | "integer" | "int" | "bigint" | "tinyint" | "mediumint" | "serial" | "smallserial"
        | "bigserial" | "numeric" | "decimal" | "real" | "float" | "double" | "double precision" | "money"
        | "smallmoney" | "number" | "oid" => Number,
        _ if ["int", "uint", "float", "bfloat", "decimal"].into_iter().any(sized) => Number,
        "text" | "varchar" | "char" | "nvarchar" | "nchar" | "ntext" | "tinytext" | "mediumtext" | "longtext"
        | "string" | "fixedstring" | "citext" | "name" | "clob" | "nclob" | "xml" => Text,
        "boolean" | "bool" => Boolean,
        "date" | "date32" | "year" => Date,
        "time" | "timetz" | "interval" => Time,
        "timestamp" | "timestamptz" | "datetime" | "datetime2" | "datetime64" | "smalldatetime" | "datetimeoffset" => {
            DateTime
        }
        "json" | "jsonb" | "object" => Json,
        "uuid" | "uniqueidentifier" => Uuid,
        "bytea" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "binary" | "varbinary" | "image" | "bit"
        | "varbit" => Binary,
        "enum" | "enum8" | "enum16" | "set" => Enum,
        _ => return None,
    })
}

// SQLite's affinity rules, for a name it doesn't know: `UNSIGNED BIG INT` is an integer, `LONG CHARACTERS` text.
fn affinity(base: &str) -> Option<TypeFamily> {
    let has = |parts: &[&str]| parts.iter().any(|part| base.contains(part));
    if has(&["int"]) {
        Some(TypeFamily::Number)
    } else if has(&["char", "clob", "text"]) {
        Some(TypeFamily::Text)
    } else if has(&["blob"]) {
        Some(TypeFamily::Binary)
    } else if has(&["real", "floa", "doub"]) {
        Some(TypeFamily::Number)
    } else {
        None
    }
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
            ("TIMETZ", "timetz"),
            ("mood", "mood"),
            ("mood[]", "mood[]"),
            ("\"TransactionType\"", "\"TransactionType\""),
            ("\"TransactionType\"[]", "\"TransactionType\"[]"),
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

    #[test]
    fn each_engines_spellings_fall_into_their_family() {
        use TypeFamily::*;
        let (pg, my, ms) = (Some(SqlDialect::Postgres), Some(SqlDialect::MySql), Some(SqlDialect::TSql));
        let (ch, lite) = (Some(SqlDialect::ClickHouse), Some(SqlDialect::Sqlite));
        let cases = [
            (pg, "bigint", Number),
            (pg, "numeric(10,2)", Number),
            (pg, "double precision", Number),
            (pg, "character varying(255)", Text),
            (pg, "boolean", Boolean),
            (pg, "date", Date),
            (pg, "time without time zone", Time),
            (pg, "interval", Time),
            (pg, "timestamp(3) with time zone", DateTime),
            (pg, "jsonb", Json),
            (pg, "uuid", Uuid),
            (pg, "bytea", Binary),
            (pg, "bit varying(8)", Binary),
            (pg, "character varying(50)[]", Array),
            (pg, "_INT4", Array),
            (pg, "\"Mood\"", Other),
            (pg, "point", Other),
            (my, "int unsigned", Number),
            (my, "bigint(20) unsigned zerofill", Number),
            (my, "tinyint(1)", Boolean),
            (my, "tinyint(4)", Number),
            (my, "bit(1)", Boolean),
            (my, "bit(8)", Binary),
            (my, "year", Date),
            (my, "datetime(6)", DateTime),
            (my, "enum('draft','live')", Enum),
            (my, "set('a','b')", Enum),
            (my, "longblob", Binary),
            (ms, "bit", Boolean),
            (ms, "decimal(18, 2)", Number),
            (ms, "nvarchar(max)", Text),
            (ms, "datetime2(7)", DateTime),
            (ms, "datetimeoffset(7)", DateTime),
            (ms, "time(7)", Time),
            (ms, "uniqueidentifier", Uuid),
            (ms, "varbinary(max)", Binary),
            (ms, "geography", Other),
            (ch, "UInt64", Number),
            (ch, "Decimal64(4)", Number),
            (ch, "LowCardinality(Nullable(String))", Text),
            (ch, "FixedString(16)", Text),
            (ch, "Bool", Boolean),
            (ch, "Date32", Date),
            (ch, "Nullable(DateTime64(3, 'UTC'))", DateTime),
            (ch, "UUID", Uuid),
            (ch, "Object('json')", Json),
            (ch, "Array(Nullable(Int32))", Array),
            (ch, "Enum8('a' = 1, 'b' = 2)", Enum),
            (ch, "Map(String, UInt64)", Other),
            (lite, "INTEGER", Number),
            (lite, "UNSIGNED BIG INT", Number),
            (lite, "NVARCHAR(100)", Text),
            (lite, "BOOLEAN", Boolean),
            (lite, "DATETIME", DateTime),
            (lite, "JSON", Json),
            (lite, "BLOB", Binary),
            (lite, "", Other),
            (None, "TEXT", Text),
        ];
        for (dialect, raw, want) in cases {
            assert_eq!(type_family(dialect, raw), want, "{dialect:?} {raw:?}");
        }
    }

    // SQLite takes any declared name and goes by its affinity, but other engines' names only mean what they say:
    // a Postgres `point` or `interval` has nothing to do with integers.
    #[test]
    fn only_sqlite_reads_a_name_by_its_affinity() {
        assert_eq!(type_family(Some(SqlDialect::Sqlite), "MEDIUMINTEGER"), TypeFamily::Number);
        assert_eq!(type_family(Some(SqlDialect::Sqlite), "LONG CHARACTERS"), TypeFamily::Text);
        assert_eq!(type_family(Some(SqlDialect::Postgres), "point"), TypeFamily::Other);
        assert_eq!(type_family(Some(SqlDialect::MySql), "multipoint"), TypeFamily::Other);
    }
}
