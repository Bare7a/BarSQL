use barsql_core::{DriverType, SqlDialect};

use crate::dialect::{Dialect, IdentQuote, Placeholder};

pub fn quote_ident(driver: &DriverType, ident: &str) -> String {
    quote_ident_in(Dialect::for_driver(driver), ident)
}

pub fn quote_ident_in(dialect: &Dialect, ident: &str) -> String {
    match dialect.ident_quote {
        IdentQuote::Double => format!("\"{}\"", ident.replace('"', "\"\"")),
        IdentQuote::Backtick => format!("`{}`", ident.replace('`', "``")),
        IdentQuote::BacktickBackslash => format!("`{}`", ident.replace('\\', "\\\\").replace('`', "\\`")),
        IdentQuote::Bracket => format!("[{}]", ident.replace(']', "]]")),
    }
}

pub fn qualified_table(driver: &DriverType, schema: &str, table: &str) -> String {
    if schema.is_empty() {
        return quote_ident(driver, table);
    }
    format!("{}.{}", quote_ident(driver, schema), quote_ident(driver, table))
}

// SQLite tables are never schema-qualified.
pub fn table_ref(driver: &DriverType, schema: &str, table: &str) -> String {
    if !Dialect::for_driver(driver).qualify_tables {
        return quote_ident(driver, table);
    }
    qualified_table(driver, schema, table)
}

pub fn placeholder(driver: &DriverType, index: usize) -> String {
    match Dialect::for_driver(driver).placeholder {
        Placeholder::Dollar => format!("${index}"),
        Placeholder::Question => "?".into(),
        Placeholder::AtP => format!("@P{index}"),
    }
}

pub fn quote_ident_list(driver: &DriverType, idents: &[String]) -> String {
    idents.iter().map(|ident| quote_ident(driver, ident)).collect::<Vec<_>>().join(", ")
}

pub fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

// A string literal that reads the same whatever the server's settings, for text that may hold quotes and
// backslashes.
pub fn quote_literal_in(dialect: SqlDialect, text: &str) -> String {
    match dialect {
        // E'' escapes the same way whether standard_conforming_strings is on or off.
        SqlDialect::Postgres if text.contains('\\') => {
            format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
        }
        // A backslash escapes unless NO_BACKSLASH_ESCAPES is set, and a hex string means the same either way.
        SqlDialect::MySql if text.contains('\\') => {
            let hex: String = text.bytes().map(|b| format!("{b:02X}")).collect();
            format!("CONVERT(X'{hex}' USING utf8mb4)")
        }
        SqlDialect::Postgres | SqlDialect::MySql | SqlDialect::Sqlite => quote_literal(text),
        // Without the N, text outside the database's code page is lost.
        SqlDialect::TSql => format!("N{}", quote_literal(text)),
        SqlDialect::ClickHouse => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_double_their_quotes() {
        assert_eq!(quote_literal("plain"), "'plain'");
        assert_eq!(quote_literal("it's"), "'it''s'");
        assert_eq!(quote_literal(""), "''");
        assert_eq!(quote_literal("'; DROP TABLE users; --"), "'''; DROP TABLE users; --'");
    }

    #[test]
    fn dialect_literals_read_the_same_under_any_setting() {
        let cases = [
            (SqlDialect::Postgres, "it's", "'it''s'"),
            (SqlDialect::Postgres, r"a\b'", r"E'a\\b'''"),
            (SqlDialect::MySql, "it's", "'it''s'"),
            (SqlDialect::MySql, r"a\b", "CONVERT(X'615C62' USING utf8mb4)"),
            (SqlDialect::Sqlite, r"a\b'", r"'a\b'''"),
            (SqlDialect::TSql, r"a\b'ü", r"N'a\b''ü'"),
            (SqlDialect::ClickHouse, r"a\b'", r"'a\\b\''"),
        ];
        for (dialect, text, literal) in cases {
            assert_eq!(quote_literal_in(dialect, text), literal, "{dialect:?} {text}");
        }
    }

    #[test]
    fn quotes_per_driver() {
        let cases = [
            (DriverType::Postgres, "users", "\"users\""),
            (DriverType::Postgres, "a\"b", "\"a\"\"b\""),
            (DriverType::MySql, "users", "`users`"),
            (DriverType::MySql, "a`b", "`a``b`"),
            (DriverType::Sqlite, "users", "\"users\""),
            (DriverType::Turso, "users", "\"users\""),
            (DriverType::SqlServer, "a]b", "[a]]b]"),
            (DriverType::ClickHouse, "a`b\\c", "`a\\`b\\\\c`"),
        ];
        for (driver, ident, want) in cases {
            assert_eq!(quote_ident(&driver, ident), want);
        }
        assert_eq!(qualified_table(&DriverType::Postgres, "public", "users"), "\"public\".\"users\"");
        assert_eq!(qualified_table(&DriverType::Postgres, "", "users"), "\"users\"");
        assert_eq!(qualified_table(&DriverType::MySql, "shop", "orders"), "`shop`.`orders`");
        assert_eq!(quote_ident_list(&DriverType::Postgres, &["a".into(), "b".into()]), "\"a\", \"b\"");
        assert_eq!(quote_ident_list(&DriverType::Postgres, &[]), "");
    }

    #[test]
    fn placeholders() {
        assert_eq!(placeholder(&DriverType::Postgres, 1), "$1");
        assert_eq!(placeholder(&DriverType::Postgres, 42), "$42");
        assert_eq!(placeholder(&DriverType::MySql, 5), "?");
        assert_eq!(placeholder(&DriverType::Sqlite, 3), "?");
        assert_eq!(placeholder(&DriverType::SqlServer, 2), "@P2");
        assert_eq!(table_ref(&DriverType::Turso, "main", "t"), "\"t\"");
        assert_eq!(table_ref(&DriverType::SqlServer, "dbo", "t"), "[dbo].[t]");
    }
}
