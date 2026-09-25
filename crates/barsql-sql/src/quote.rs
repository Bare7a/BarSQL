use barsql_core::DriverType;

pub fn quote_ident(driver: &DriverType, ident: &str) -> String {
    match driver {
        DriverType::MySql => format!("`{}`", ident.replace('`', "``")),
        _ => format!("\"{}\"", ident.replace('"', "\"\"")),
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
    if *driver == DriverType::Sqlite {
        return quote_ident(driver, table);
    }
    qualified_table(driver, schema, table)
}

pub fn placeholder(driver: &DriverType, index: usize) -> String {
    if *driver == DriverType::Postgres { format!("${index}") } else { "?".into() }
}

pub fn quote_ident_list(driver: &DriverType, idents: &[String]) -> String {
    idents.iter().map(|ident| quote_ident(driver, ident)).collect::<Vec<_>>().join(", ")
}

pub fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
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
    fn quotes_per_driver() {
        let cases = [
            (DriverType::Postgres, "users", "\"users\""),
            (DriverType::Postgres, "a\"b", "\"a\"\"b\""),
            (DriverType::MySql, "users", "`users`"),
            (DriverType::MySql, "a`b", "`a``b`"),
            (DriverType::Sqlite, "users", "\"users\""),
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
    }
}
