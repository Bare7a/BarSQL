// A database on a saved connection, as a connection of its own: id `{base}/{database}`, the saved config with the
// database swapped. The connection's own database keeps the base id, so its tabs, history and tree state stay put.

use crate::capabilities::DefaultSchema;
use crate::config::ConnectionConfig;

pub const SEPARATOR: char = '/';

pub fn scoped_id(base: &str, database: &str) -> String {
    format!("{base}{SEPARATOR}{}", encode(database))
}

pub fn split_id(id: &str) -> (&str, Option<String>) {
    match id.split_once(SEPARATOR) {
        Some((base, database)) => (base, Some(decode(database))),
        None => (id, None),
    }
}

pub fn base_id(id: &str) -> &str {
    id.split_once(SEPARATOR).map_or(id, |(base, _)| base)
}

// The same saved connection, on any of its databases.
pub fn same_connection(a: &str, b: &str) -> bool {
    base_id(a) == base_id(b)
}

// A saved connection, or one of its databases. An exact match wins, for saved ids that happen to hold a `/`.
pub fn resolve(connections: &[ConnectionConfig], id: &str) -> Option<ConnectionConfig> {
    resolve_with(id, |id| connections.iter().find(|cfg| cfg.id == id).cloned())
}

pub fn resolve_with(id: &str, get: impl Fn(&str) -> Option<ConnectionConfig>) -> Option<ConnectionConfig> {
    if let Some(cfg) = get(id) {
        return Some(cfg);
    }
    let (base, Some(database)) = split_id(id) else { return None };
    get(base).map(|cfg| cfg.in_database(&database))
}

impl ConnectionConfig {
    pub fn shows_all_databases(&self) -> bool {
        self.show_all_databases && self.driver.capabilities().all_databases
    }

    // The database the connection opens. MySQL and ClickHouse open their schema.
    pub fn default_database(&self) -> String {
        match self.driver.capabilities().default_schema {
            DefaultSchema::Database => self.default_browse_schema(),
            DefaultSchema::Named(_) => self.database.clone(),
        }
    }

    // Called on the saved connection. Its own database gives the connection back unchanged.
    pub fn in_database(&self, database: &str) -> ConnectionConfig {
        if database == self.default_database() {
            return self.clone();
        }
        let mut cfg = self.clone();
        cfg.id = scoped_id(base_id(&self.id), database);
        cfg.database = database.to_string();
        // The saved schema belongs to the saved database. MySQL and ClickHouse would connect to it.
        cfg.schema.clear();
        cfg
    }

    pub fn scoped_database(&self) -> Option<String> {
        split_id(&self.id).1
    }

    // "Forum › sales" for another database, so prompts say where a statement runs.
    pub fn label(&self) -> String {
        match self.scoped_database() {
            Some(database) => format!("{} › {database}", self.name),
            None => self.name.clone(),
        }
    }
}

fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '%' => out.push_str("%25"),
            '/' => out.push_str("%2F"),
            ':' => out.push_str("%3A"),
            c => out.push(c),
        }
    }
    out
}

fn decode(encoded: &str) -> String {
    let mut out = String::with_capacity(encoded.len());
    let mut rest = encoded;
    while let Some(ix) = rest.find('%') {
        out.push_str(&rest[..ix]);
        let decoded = match rest.get(ix + 1..ix + 3) {
            Some("25") => Some('%'),
            Some("2F") => Some('/'),
            Some("3A") => Some(':'),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[ix + 3..];
            }
            None => {
                out.push('%');
                rest = &rest[ix + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DriverType;

    fn pg() -> ConnectionConfig {
        ConnectionConfig {
            id: "c1".into(),
            name: "Forum".into(),
            driver: DriverType::Postgres,
            host: "h".into(),
            database: "forum".into(),
            username: "u".into(),
            schema: "app".into(),
            read_only: true,
            confirm_changes: true,
            show_all_databases: true,
            ..Default::default()
        }
    }

    #[test]
    fn database_names_survive_the_round_trip() {
        for name in ["sales", "a/b", "a:b", "100%", "%2F", "x%3Ay", "日本/語:%", "", "a//b"] {
            let id = scoped_id("c1", name);
            assert_eq!(id.matches(SEPARATOR).count(), 1, "{id}");
            assert!(!id.contains(':'), "{id}");
            assert_eq!(split_id(&id), ("c1", Some(name.to_string())), "{name}");
            assert_eq!(base_id(&id), "c1");
        }
        assert_eq!(split_id("c1"), ("c1", None));
        assert_eq!(decode("50%"), "50%");
        assert_eq!(decode("%zz%2"), "%zz%2");
    }

    #[test]
    fn the_saved_database_keeps_the_saved_id() {
        let cfg = pg();
        assert_eq!(cfg.in_database("forum"), cfg);
        let sales = cfg.in_database("sales");
        assert_eq!(sales.id, "c1/sales");
        assert_eq!(sales.database, "sales");
        assert_eq!(sales.schema, "");
        assert_eq!(sales.name, "Forum");
        assert!(sales.read_only && sales.confirm_changes);
        assert_eq!(sales.scoped_database().as_deref(), Some("sales"));
        assert_eq!(sales.label(), "Forum › sales");
        assert_eq!(cfg.label(), "Forum");
        assert_ne!(sales.fingerprint(), cfg.fingerprint());
    }

    #[test]
    fn mysql_and_clickhouse_open_their_schema() {
        let mysql = ConnectionConfig { driver: DriverType::MySql, database: "shop".into(), ..pg() };
        assert_eq!(mysql.default_database(), "app");
        assert_eq!(mysql.in_database("app"), mysql);
        let shop = mysql.in_database("shop");
        assert_eq!((shop.id.as_str(), shop.default_browse_schema().as_str()), ("c1/shop", "shop"));
        let clickhouse = ConnectionConfig { driver: DriverType::ClickHouse, ..ConnectionConfig::default() };
        assert_eq!(clickhouse.default_database(), "default");
        let mssql = ConnectionConfig { driver: DriverType::SqlServer, database: String::new(), ..pg() };
        assert_eq!(mssql.default_database(), "");
    }

    #[test]
    fn resolve_finds_saved_connections_and_their_databases() {
        let saved = vec![pg(), ConnectionConfig { id: "odd/id".into(), ..pg() }];
        assert_eq!(resolve(&saved, "c1"), Some(pg()));
        assert_eq!(resolve(&saved, "c1/forum"), Some(pg()));
        assert_eq!(resolve(&saved, "c1/sales").map(|c| c.database), Some("sales".into()));
        assert_eq!(resolve(&saved, "odd/id").map(|c| c.id), Some("odd/id".into()));
        assert_eq!(resolve(&saved, "gone/sales"), None);
        assert_eq!(resolve(&saved, "gone"), None);
    }

    #[test]
    fn the_option_needs_an_engine_with_databases() {
        let sqlite = ConnectionConfig { driver: DriverType::Sqlite, show_all_databases: true, ..Default::default() };
        assert!(!sqlite.shows_all_databases());
        assert!(pg().shows_all_databases());
        assert!(!ConnectionConfig { show_all_databases: false, ..pg() }.shows_all_databases());
    }

    #[test]
    fn the_option_is_saved_but_does_not_change_how_it_connects() {
        let cfg = pg();
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["showAllDatabases"], true);
        let off = ConnectionConfig { show_all_databases: false, ..pg() };
        assert!(serde_json::to_value(&off).unwrap().get("showAllDatabases").is_none());
        assert!(cfg.connects_like(&off));
    }

    #[test]
    fn sql_server_needs_a_database_to_list_the_others() {
        let mssql = ConnectionConfig { driver: DriverType::SqlServer, database: String::new(), ..pg() };
        assert!(mssql.validate().is_err());
        assert!(ConnectionConfig { show_all_databases: false, ..mssql.clone() }.validate().is_ok());
        assert!(ConnectionConfig { database: "master".into(), ..mssql }.validate().is_ok());
    }
}
