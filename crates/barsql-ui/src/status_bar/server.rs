use barsql_core::{ConnectionConfig, ConnectionStatus, DriverType};

// One line each of the connection item's tooltip. Never a password, token or key.
#[derive(Debug, Clone, PartialEq)]
pub enum Detail {
    Version(String),
    Address(String),
    Database { database: String, schema: Option<String> },
    Ssh(String),
    Tls(String),
    Latency(u64),
}

// "PostgreSQL 17.2" from what the server says it runs. None until it has said, and for Turso, which doesn't.
pub fn server_label(driver: &DriverType, version: &str) -> Option<String> {
    let version = version.trim();
    if version.is_empty() {
        return None;
    }
    match driver {
        DriverType::Postgres => Some(format!("PostgreSQL {}", version.split_whitespace().next()?)),
        DriverType::MySql => Some(format!("{} {}", mysql_family(version), version.split('-').next()?)),
        DriverType::SqlServer => {
            let product = version.split(" (").next()?.split(" - ").next()?;
            Some(product.strip_prefix("Microsoft ").unwrap_or(product).to_string())
        }
        DriverType::ClickHouse => {
            Some(format!("ClickHouse {}", version.split('.').take(2).collect::<Vec<_>>().join(".")))
        }
        DriverType::Sqlite => Some(format!("SQLite {version}")),
        _ => None,
    }
}

fn mysql_family(version: &str) -> &'static str {
    if version.contains("MariaDB") { "MariaDB" } else { "MySQL" }
}

// SQL Server's own line already names the product.
fn version_line(driver: &DriverType, version: &str) -> Option<String> {
    let version = version.trim();
    let family = match driver {
        _ if version.is_empty() => return None,
        DriverType::Postgres => "PostgreSQL",
        DriverType::MySql => mysql_family(version),
        DriverType::SqlServer => return Some(version.to_string()),
        DriverType::ClickHouse => "ClickHouse",
        DriverType::Sqlite => "SQLite",
        _ => return None,
    };
    Some(format!("{family} {version}"))
}

pub fn details(config: &ConnectionConfig, status: Option<&ConnectionStatus>) -> Vec<Detail> {
    let mut lines: Vec<Detail> =
        status.and_then(|s| version_line(&config.driver, &s.version)).map(Detail::Version).into_iter().collect();
    match config.driver {
        DriverType::Sqlite => {
            lines.extend((!config.file_path.is_empty()).then(|| Detail::Address(config.file_path.clone())));
            return lines;
        }
        DriverType::Turso => {
            let host = either(status.map(|s| s.host.as_str()), url_host(&config.url));
            lines.extend((!host.is_empty()).then_some(Detail::Address(host)));
            return lines;
        }
        _ => {}
    }
    let place = match (config.instance.is_empty(), config.port > 0) {
        (false, _) => format!(r"{}\{}", config.host, config.instance),
        (true, true) => format!("{}:{}", config.host, config.port),
        (true, false) => config.host.clone(),
    };
    // MySQL's CURRENT_USER() is the account with its host pattern, "app@%". A saved user name may hold an @ itself.
    let server_user = status.map(|s| match config.driver {
        DriverType::MySql => s.user.rsplit_once('@').map_or(s.user.as_str(), |(user, _)| user),
        _ => s.user.as_str(),
    });
    lines.push(Detail::Address(at(&either(server_user, &config.username), &place)));
    let database = either(status.map(|s| s.database.as_str()), &config.database);
    if !database.is_empty() {
        let schema = either(status.map(|s| s.schema.as_str()), &config.schema);
        let separate = !matches!(config.driver, DriverType::MySql | DriverType::ClickHouse);
        let schema = (separate && !schema.is_empty() && schema != database).then_some(schema);
        lines.push(Detail::Database { database, schema });
    }
    if config.ssh.enabled {
        let ssh = &config.ssh;
        let place = if ssh.port > 0 { format!("{}:{}", ssh.host, ssh.port) } else { ssh.host.clone() };
        lines.push(Detail::Ssh(at(&ssh.username, &place)));
    }
    if !config.ssl_mode.is_empty() {
        lines.push(Detail::Tls(config.ssl_mode.clone()));
    }
    lines
}

fn either(server: Option<&str>, setting: &str) -> String {
    server.filter(|value| !value.is_empty()).unwrap_or(setting).to_string()
}

fn at(user: &str, place: &str) -> String {
    if user.is_empty() { place.to_string() } else { format!("{user}@{place}") }
}

// "notes-acme.turso.io" from "libsql://notes-acme.turso.io/…", without any credentials the URL carries.
fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    authority.rsplit_once('@').map_or(authority, |(_, host)| host)
}

#[cfg(test)]
mod tests {
    use barsql_core::{ConnectionConfig, ConnectionStatus, DriverType, SshConfig};

    use super::{Detail, details, server_label};

    // MySQL's CURRENT_USER() names the account with its host pattern, like "app@%".
    #[test]
    fn a_mysql_account_drops_its_host_pattern() {
        let status = ConnectionStatus { user: "app@%".into(), ..Default::default() };
        let shown = details(&config(DriverType::MySql), Some(&status));
        assert!(shown.contains(&Detail::Address("app@db.example.com:5432".into())), "{shown:?}");
    }

    #[test]
    fn a_url_never_shows_its_credentials() {
        let turso = ConnectionConfig {
            driver: DriverType::Turso,
            url: "libsql://admin:hunter2@notes-acme.turso.io/db".into(),
            ..Default::default()
        };
        assert_eq!(details(&turso, None), vec![Detail::Address("notes-acme.turso.io".into())]);
    }

    // Without an edition in brackets, the product still ends where its build number begins.
    #[test]
    fn a_sql_server_line_without_its_edition_still_names_the_product() {
        let line = "Babelfish for Aurora PostgreSQL with SQL Server Compatibility - 12.0.2000.8";
        assert_eq!(
            server_label(&DriverType::SqlServer, line).as_deref(),
            Some("Babelfish for Aurora PostgreSQL with SQL Server Compatibility")
        );
    }

    fn config(driver: DriverType) -> ConnectionConfig {
        ConnectionConfig {
            driver,
            host: "db.example.com".into(),
            port: 5432,
            username: "app".into(),
            password: "hunter2".into(),
            auth_token: "tok-secret".into(),
            database: "shop".into(),
            schema: "public".into(),
            ssl_mode: "require".into(),
            ..Default::default()
        }
    }

    #[test]
    fn each_engine_names_its_server() {
        use DriverType::*;
        let label = |d, v| server_label(&d, v);
        assert_eq!(label(Postgres, "17.2 (Debian 17.2-1.pgdg120+1)").as_deref(), Some("PostgreSQL 17.2"));
        assert_eq!(label(MySql, "8.4.3").as_deref(), Some("MySQL 8.4.3"));
        assert_eq!(label(MySql, "11.4.2-MariaDB-ubu2404").as_deref(), Some("MariaDB 11.4.2"));
        assert_eq!(
            label(SqlServer, "Microsoft SQL Server 2022 (RTM-CU15) (KB5041321) - 16.0.4145.4 (X64)").as_deref(),
            Some("SQL Server 2022")
        );
        assert_eq!(
            label(SqlServer, "Microsoft Azure SQL Edge Developer (RTM) - 15.0.2000.1574 (ARM64)").as_deref(),
            Some("Azure SQL Edge Developer")
        );
        assert_eq!(label(SqlServer, "Microsoft SQL Azure (RTM) - 12.0.2000.8").as_deref(), Some("SQL Azure"));
        assert_eq!(label(ClickHouse, "24.8.4.13").as_deref(), Some("ClickHouse 24.8"));
        assert_eq!(label(Sqlite, "3.46.0").as_deref(), Some("SQLite 3.46.0"));
        assert_eq!(label(Turso, "3.45.1"), None);
        for driver in &DriverType::KNOWN {
            assert_eq!(server_label(driver, ""), None, "{driver:?}");
        }
    }

    #[test]
    fn the_server_speaks_before_the_settings() {
        let status = ConnectionStatus {
            database: "forum".into(),
            schema: "app".into(),
            user: "postgres".into(),
            version: "17.2 (Debian 17.2-1.pgdg120+1)".into(),
            ..Default::default()
        };
        let ssh = SshConfig {
            enabled: true,
            host: "bastion.example.com".into(),
            port: 22,
            username: "ops".into(),
            ..Default::default()
        };
        assert_eq!(
            details(&ConnectionConfig { ssh, ..config(DriverType::Postgres) }, Some(&status)),
            vec![
                Detail::Version("PostgreSQL 17.2 (Debian 17.2-1.pgdg120+1)".into()),
                Detail::Address("postgres@db.example.com:5432".into()),
                Detail::Database { database: "forum".into(), schema: Some("app".into()) },
                Detail::Ssh("ops@bastion.example.com:22".into()),
                Detail::Tls("require".into()),
            ]
        );
        assert_eq!(
            details(&config(DriverType::Postgres), None),
            vec![
                Detail::Address("app@db.example.com:5432".into()),
                Detail::Database { database: "shop".into(), schema: Some("public".into()) },
                Detail::Tls("require".into()),
            ]
        );
    }

    #[test]
    fn only_the_lines_that_apply() {
        for driver in [DriverType::MySql, DriverType::ClickHouse] {
            let shown = details(&config(driver.clone()), None);
            assert!(
                shown.contains(&Detail::Database { database: "shop".into(), schema: None }),
                "{driver:?}: {shown:?}"
            );
        }
        let named = ConnectionConfig { instance: "SQLEXPRESS".into(), ..config(DriverType::SqlServer) };
        assert!(details(&named, None).contains(&Detail::Address(r"app@db.example.com\SQLEXPRESS".into())));
        let sqlite =
            ConnectionConfig { driver: DriverType::Sqlite, file_path: "C:/data/notes.db".into(), ..Default::default() };
        assert_eq!(details(&sqlite, None), vec![Detail::Address("C:/data/notes.db".into())]);
        let turso = ConnectionConfig {
            driver: DriverType::Turso,
            url: "libsql://notes-acme.turso.io".into(),
            ..Default::default()
        };
        assert_eq!(details(&turso, None), vec![Detail::Address("notes-acme.turso.io".into())]);
    }

    #[test]
    fn no_secret_reaches_the_tooltip() {
        for driver in &DriverType::KNOWN {
            let ssh = SshConfig {
                enabled: true,
                host: "bastion".into(),
                password: "ssh-pass".into(),
                key_path: "C:/keys/id_ed25519".into(),
                passphrase: "phrase".into(),
                ..Default::default()
            };
            let config = ConnectionConfig {
                ssh,
                url: "libsql://x.turso.io".into(),
                file_path: "C:/data/x.db".into(),
                ..config(driver.clone())
            };
            let text = format!("{:?}", details(&config, None));
            for secret in ["hunter2", "tok-secret", "ssh-pass", "id_ed25519", "phrase"] {
                assert!(!text.contains(secret), "{driver:?}: {text}");
            }
        }
    }
}
