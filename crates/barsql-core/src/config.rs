use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::capabilities::{self, Capabilities, DatabaseField, DefaultSchema, Location};
use crate::dialect::SqlDialect;

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum DriverType {
    Sqlite,
    Postgres,
    MySql,
    Turso,
    ClickHouse,
    SqlServer,
    Other(String),
    #[default]
    Unset,
}

impl DriverType {
    // In the order the connection dialog lists them.
    pub const KNOWN: [DriverType; 6] =
        [Self::Sqlite, Self::Postgres, Self::MySql, Self::SqlServer, Self::ClickHouse, Self::Turso];

    pub fn as_str(&self) -> &str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::Turso => "turso",
            Self::ClickHouse => "clickhouse",
            Self::SqlServer => "sqlserver",
            Self::Other(name) => name,
            Self::Unset => "",
        }
    }

    pub fn parse(name: &str) -> Self {
        match name {
            "sqlite" => Self::Sqlite,
            "postgres" => Self::Postgres,
            "mysql" => Self::MySql,
            "turso" => Self::Turso,
            "clickhouse" => Self::ClickHouse,
            "sqlserver" | "mssql" => Self::SqlServer,
            "" => Self::Unset,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn dialect(&self) -> Option<SqlDialect> {
        match self {
            Self::Sqlite | Self::Turso => Some(SqlDialect::Sqlite),
            Self::Postgres => Some(SqlDialect::Postgres),
            Self::MySql => Some(SqlDialect::MySql),
            Self::SqlServer => Some(SqlDialect::TSql),
            Self::ClickHouse => Some(SqlDialect::ClickHouse),
            Self::Other(_) | Self::Unset => None,
        }
    }

    pub fn capabilities(&self) -> &'static Capabilities {
        match self {
            Self::Sqlite => &capabilities::SQLITE,
            Self::Postgres => &capabilities::POSTGRES,
            Self::MySql => &capabilities::MYSQL,
            Self::Turso => &capabilities::TURSO,
            Self::ClickHouse => &capabilities::CLICKHOUSE,
            Self::SqlServer => &capabilities::SQL_SERVER,
            Self::Other(_) | Self::Unset => &capabilities::UNSUPPORTED,
        }
    }

    pub fn is_supported(&self) -> bool {
        self.capabilities().supported
    }
}

impl fmt::Display for DriverType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for DriverType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DriverType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(deserializer)?))
    }
}

pub const SSH_AUTH_PASSWORD: &str = "password";
pub const SSH_AUTH_KEY: &str = "key";
pub const SSH_AUTH_AGENT: &str = "agent";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub driver: DriverType,
    #[serde(default)]
    pub color: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub port: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub database: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    // A database reached by URL, like Turso's libsql://name-org.turso.io.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ssl_mode: String,
    // A SQL Server named instance, found through the SQL Browser service instead of a port.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
    // Asks before the editor runs a statement that changes data or schema, as for a production database.
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirm_changes: bool,
    #[serde(default)]
    pub ssh: SshConfig,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfig {
    #[serde(default, skip_serializing_if = "is_false")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub port: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub passphrase: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub known_hosts: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ignore_host_key: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ConnectionConfig {
    pub fn normalize(&mut self) {
        trim_in_place(&mut self.name);
        trim_in_place(&mut self.file_path);
        trim_in_place(&mut self.host);
        trim_in_place(&mut self.database);
        trim_in_place(&mut self.username);
        trim_in_place(&mut self.ssl_mode);
        trim_in_place(&mut self.schema);
        trim_in_place(&mut self.url);
        trim_in_place(&mut self.auth_token);
        trim_in_place(&mut self.instance);
        self.take_url_token();
        self.ssh.normalize();
    }

    // Turso's dashboard hands out URLs with `?authToken=…`. The token goes in its own field, so the URL can be
    // shown without it.
    fn take_url_token(&mut self) {
        let Some((base, query)) = self.url.split_once('?') else { return };
        let mut token = None;
        let rest: Vec<&str> = query
            .split('&')
            .filter(|pair| match pair.strip_prefix("authToken=") {
                Some(value) => {
                    token = Some(value.to_string());
                    false
                }
                None => !pair.is_empty(),
            })
            .collect();
        let Some(token) = token else { return };
        if self.auth_token.is_empty() {
            self.auth_token = token;
        }
        self.url = if rest.is_empty() { base.to_string() } else { format!("{base}?{}", rest.join("&")) };
    }

    pub fn validate(&self) -> Result<(), String> {
        let caps = self.driver.capabilities();
        if !caps.supported {
            return Err(match self.driver {
                DriverType::Other(_) | DriverType::Unset => format!("unsupported driver: {}", self.driver),
                DriverType::Sqlite
                | DriverType::Postgres
                | DriverType::MySql
                | DriverType::Turso
                | DriverType::ClickHouse
                | DriverType::SqlServer => format!("{} is not available in this build", self.driver),
            });
        }
        match caps.location {
            Location::LocalFile => require(&self.file_path, "SQLite database file path is required")?,
            Location::Url => {
                require(&self.url, "database URL is required")?;
                let lower = self.url.to_ascii_lowercase();
                if !URL_SCHEMES.iter().any(|scheme| lower.len() > scheme.len() && lower.starts_with(scheme)) {
                    return Err("database URL must start with libsql://, https://, http://, wss:// or ws://".into());
                }
            }
            Location::Network => {
                require(&self.host, "host is required")?;
                if caps.database == DatabaseField::Required {
                    require(&self.database, self.database_required_message())?;
                }
                require(&self.username, "username is required")?;
            }
        }
        if caps.ssh_tunnel {
            return self.ssh.validate();
        }
        Ok(())
    }

    fn database_required_message(&self) -> &'static str {
        match self.driver {
            DriverType::Postgres => "database name is required (e.g. blog - not the username \"postgres\")",
            DriverType::Sqlite
            | DriverType::MySql
            | DriverType::Turso
            | DriverType::ClickHouse
            | DriverType::SqlServer
            | DriverType::Other(_)
            | DriverType::Unset => "database name is required",
        }
    }

    pub fn dialect(&self) -> Result<SqlDialect, String> {
        self.driver.dialect().ok_or_else(|| format!("unsupported driver: {}", self.driver))
    }

    pub fn default_browse_schema(&self) -> String {
        if !self.schema.is_empty() {
            return self.schema.clone();
        }
        let caps = self.driver.capabilities();
        match caps.default_schema {
            DefaultSchema::Named(name) => name.into(),
            DefaultSchema::Database if self.database.is_empty() => caps.default_database.into(),
            DefaultSchema::Database => self.database.clone(),
        }
    }

    // Hashed so passwords never show up in plain text wherever the fingerprint is kept.
    pub fn fingerprint(&self) -> String {
        let ssh = &self.ssh;
        let raw = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.driver,
            self.host,
            self.port,
            self.database,
            self.username,
            self.password,
            self.ssl_mode,
            self.file_path,
            self.schema,
            self.read_only,
            ssh.enabled,
            ssh.host,
            ssh.port,
            ssh.username,
            ssh.auth,
            ssh.password,
            ssh.key_path,
            ssh.passphrase,
            ssh.known_hosts,
            ssh.ignore_host_key,
        );
        // Appended only when set, so connections from before these fields keep their fingerprints.
        let mut raw = raw;
        for (label, value) in [("url", &self.url), ("authToken", &self.auth_token), ("instance", &self.instance)] {
            if !value.is_empty() {
                raw.push_str(&format!("|{label}={value}"));
            }
        }
        hex::encode(Sha256::digest(raw.as_bytes()))
    }
}

impl SshConfig {
    fn normalize(&mut self) {
        trim_in_place(&mut self.host);
        trim_in_place(&mut self.username);
        trim_in_place(&mut self.key_path);
        trim_in_place(&mut self.known_hosts);
        // Defaults only once the tunnel is on, so direct connections keep an empty block.
        if !self.enabled {
            return;
        }
        if self.port == 0 {
            self.port = 22;
        }
        if self.auth.is_empty() {
            self.auth = SSH_AUTH_KEY.into();
        }
    }

    fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        require(&self.host, "SSH host is required")?;
        require(&self.username, "SSH username is required")?;
        if !(0..=65535).contains(&self.port) {
            return Err("SSH port must be between 1 and 65535".into());
        }
        match self.auth.as_str() {
            SSH_AUTH_KEY | "" => require(&self.key_path, "SSH private key file is required"),
            SSH_AUTH_PASSWORD | SSH_AUTH_AGENT => Ok(()),
            other => Err(format!("unsupported SSH authentication method: {other}")),
        }
    }
}

const URL_SCHEMES: [&str; 5] = ["libsql://", "https://", "http://", "wss://", "ws://"];

fn require(value: &str, message: &str) -> Result<(), String> {
    if value.is_empty() { Err(message.to_string()) } else { Ok(()) }
}

fn trim_in_place(value: &mut String) {
    let trimmed = value.trim();
    if trimmed.len() != value.len() {
        *value = trimmed.to_string();
    }
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg() -> ConnectionConfig {
        ConnectionConfig {
            driver: DriverType::Postgres,
            host: "db.internal".into(),
            database: "app".into(),
            username: "postgres".into(),
            ..Default::default()
        }
    }

    #[test]
    fn default_browse_schema_per_driver() {
        let cases = [
            (ConnectionConfig { driver: DriverType::Postgres, schema: "app".into(), ..Default::default() }, "app"),
            (ConnectionConfig { driver: DriverType::Postgres, ..Default::default() }, "public"),
            (ConnectionConfig { driver: DriverType::MySql, database: "shop".into(), ..Default::default() }, "shop"),
            (ConnectionConfig { driver: DriverType::Sqlite, ..Default::default() }, "main"),
        ];
        for (cfg, want) in cases {
            assert_eq!(cfg.default_browse_schema(), want, "{}", cfg.driver);
        }
    }

    #[test]
    fn normalize_trims_everything_but_the_password() {
        let mut cfg = ConnectionConfig {
            name: "  prod ".into(),
            file_path: " ./data.db ".into(),
            host: " localhost\n".into(),
            database: "\tshop ".into(),
            username: " admin ".into(),
            ssl_mode: " require ".into(),
            schema: " public ".into(),
            password: " secret ".into(),
            ..Default::default()
        };
        cfg.normalize();
        assert_eq!(
            [&cfg.name, &cfg.file_path, &cfg.host, &cfg.database, &cfg.username, &cfg.ssl_mode, &cfg.schema],
            ["prod", "./data.db", "localhost", "shop", "admin", "require", "public"]
        );
        assert_eq!(cfg.password, " secret ");
    }

    #[test]
    fn validate_connection_config() {
        let cfg = |driver: DriverType, file: &str, host: &str, db: &str, user: &str| ConnectionConfig {
            driver,
            file_path: file.into(),
            host: host.into(),
            database: db.into(),
            username: user.into(),
            ..Default::default()
        };
        let cases = [
            ("sqlite ok", cfg(DriverType::Sqlite, "x.db", "", "", ""), ""),
            ("sqlite missing file", cfg(DriverType::Sqlite, "", "", "", ""), "file path"),
            ("postgres ok", cfg(DriverType::Postgres, "", "h", "d", "u"), ""),
            ("postgres missing host", cfg(DriverType::Postgres, "", "", "d", "u"), "host"),
            ("postgres missing db", cfg(DriverType::Postgres, "", "h", "", "u"), "database"),
            ("postgres missing user", cfg(DriverType::Postgres, "", "h", "d", ""), "username"),
            ("mysql ok", cfg(DriverType::MySql, "", "h", "d", "u"), ""),
            ("mysql missing host", cfg(DriverType::MySql, "", "", "d", "u"), "host"),
            ("unknown driver", cfg(DriverType::parse("oracle"), "", "", "", ""), "unsupported"),
            ("unset driver", cfg(DriverType::Unset, "", "h", "d", "u"), "unsupported"),
        ];
        for (name, cfg, want) in cases {
            match (cfg.validate(), want) {
                (Ok(()), "") => {}
                (Err(err), want) if !want.is_empty() && err.contains(want) => {}
                (got, want) => panic!("{name}: got {got:?}, want {want:?}"),
            }
        }
    }

    #[test]
    fn fingerprint_changes_with_every_relevant_field() {
        let base = ConnectionConfig {
            id: "id-1".into(),
            driver: DriverType::Postgres,
            host: "h".into(),
            port: 5432,
            database: "d".into(),
            username: "u".into(),
            password: "p".into(),
            ssl_mode: "require".into(),
            schema: "s".into(),
            ..Default::default()
        };
        let base_fp = base.fingerprint();
        let cosmetic = ConnectionConfig {
            name: "different name".into(),
            color: "#ff0000".into(),
            folder_id: "f-1".into(),
            ..base.clone()
        };
        assert_eq!(cosmetic.fingerprint(), base_fp, "cosmetic fields must not force a reconnect");

        type Mutation = (&'static str, fn(&mut ConnectionConfig));
        let mutations: [Mutation; 12] = [
            ("url", |c| c.url = "libsql://db.turso.io".into()),
            ("authToken", |c| c.auth_token = "t".into()),
            ("driver", |c| c.driver = DriverType::MySql),
            ("host", |c| c.host = "h2".into()),
            ("port", |c| c.port = 5433),
            ("database", |c| c.database = "d2".into()),
            ("username", |c| c.username = "u2".into()),
            ("password", |c| c.password = "p2".into()),
            ("sslMode", |c| c.ssl_mode = "disable".into()),
            ("filePath", |c| c.file_path = "x.db".into()),
            ("schema", |c| c.schema = "s2".into()),
            ("readOnly", |c| c.read_only = true),
        ];
        for (name, mutate) in mutations {
            let mut cfg = base.clone();
            mutate(&mut cfg);
            assert_ne!(cfg.fingerprint(), base_fp, "{name}");
        }
    }

    #[test]
    fn normalize_ssh_trims_and_defaults() {
        let mut cfg = pg();
        cfg.ssh = SshConfig {
            enabled: true,
            host: "  bastion.example.com  ".into(),
            username: "  deploy ".into(),
            key_path: "  ~/.ssh/id_ed25519 ".into(),
            known_hosts: " ~/.ssh/known_hosts ".into(),
            ..Default::default()
        };
        cfg.normalize();
        assert_eq!(cfg.ssh.host, "bastion.example.com");
        assert_eq!(cfg.ssh.username, "deploy");
        assert_eq!(cfg.ssh.key_path, "~/.ssh/id_ed25519");
        assert_eq!(cfg.ssh.known_hosts, "~/.ssh/known_hosts");
        assert_eq!(cfg.ssh.port, 22);
        assert_eq!(cfg.ssh.auth, SSH_AUTH_KEY);

        let mut explicit = pg();
        explicit.ssh = SshConfig {
            enabled: true,
            host: "b".into(),
            username: "u".into(),
            port: 2222,
            auth: SSH_AUTH_AGENT.into(),
            ..Default::default()
        };
        explicit.normalize();
        assert_eq!((explicit.ssh.port, explicit.ssh.auth.as_str()), (2222, SSH_AUTH_AGENT));

        let mut disabled = pg();
        disabled.normalize();
        assert_eq!(disabled.ssh, SshConfig::default(), "a direct connection keeps an empty SSH block");
    }

    #[test]
    fn validate_ssh_config() {
        let ssh = |enabled: bool, host: &str, user: &str, auth: &str, key: &str, port: i64| SshConfig {
            enabled,
            host: host.into(),
            username: user.into(),
            auth: auth.into(),
            key_path: key.into(),
            port,
            ..Default::default()
        };
        let cases = [
            ("disabled ignores empty fields", SshConfig::default(), ""),
            ("disabled ignores invalid fields", ssh(false, "", "", "", "", 99999), ""),
            ("key auth is complete", ssh(true, "b", "u", SSH_AUTH_KEY, "/k", 0), ""),
            ("agent auth needs no key", ssh(true, "b", "u", SSH_AUTH_AGENT, "", 0), ""),
            ("password auth needs no key", ssh(true, "b", "u", SSH_AUTH_PASSWORD, "", 0), ""),
            ("missing host", ssh(true, "", "u", SSH_AUTH_AGENT, "", 0), "SSH host is required"),
            ("missing user", ssh(true, "b", "", SSH_AUTH_AGENT, "", 0), "SSH username is required"),
            ("missing key", ssh(true, "b", "u", SSH_AUTH_KEY, "", 0), "private key file is required"),
            ("empty auth defaults to key", ssh(true, "b", "u", "", "", 0), "private key file is required"),
            ("bad port", ssh(true, "b", "u", SSH_AUTH_AGENT, "", 70000), "between 1 and 65535"),
            ("bad method", ssh(true, "b", "u", "telepathy", "", 0), "unsupported SSH authentication"),
        ];
        for (name, ssh, want) in cases {
            let cfg = ConnectionConfig { ssh, ..pg() };
            match (cfg.validate(), want) {
                (Ok(()), "") => {}
                (Err(err), want) if !want.is_empty() && err.contains(want) => {}
                (got, want) => panic!("{name}: got {got:?}, want {want:?}"),
            }
        }
        let sqlite = ConnectionConfig {
            driver: DriverType::Sqlite,
            file_path: "/tmp/app.db".into(),
            ssh: SshConfig { enabled: true, ..Default::default() },
            ..Default::default()
        };
        assert_eq!(sqlite.validate(), Ok(()), "SQLite ignores leftover SSH settings");
    }

    #[test]
    fn fingerprint_covers_ssh_fields() {
        let mut base = pg();
        base.ssh = SshConfig {
            enabled: true,
            host: "bastion".into(),
            port: 22,
            username: "deploy".into(),
            auth: SSH_AUTH_KEY.into(),
            key_path: "/keys/id".into(),
            passphrase: "pp".into(),
            known_hosts: "/kh".into(),
            password: "pw".into(),
            ..Default::default()
        };
        let base_fp = base.fingerprint();
        assert_eq!(base.clone().fingerprint(), base_fp);
        type Mutation = (&'static str, fn(&mut SshConfig));
        let mutations: [Mutation; 10] = [
            ("enabled", |s| s.enabled = false),
            ("host", |s| s.host = "other".into()),
            ("port", |s| s.port = 2222),
            ("username", |s| s.username = "root".into()),
            ("auth", |s| s.auth = SSH_AUTH_AGENT.into()),
            ("password", |s| s.password = "other".into()),
            ("keyPath", |s| s.key_path = "/keys/other".into()),
            ("passphrase", |s| s.passphrase = "other".into()),
            ("knownHosts", |s| s.known_hosts = "/other".into()),
            ("ignoreHostKey", |s| s.ignore_host_key = true),
        ];
        for (name, mutate) in mutations {
            let mut cfg = base.clone();
            mutate(&mut cfg.ssh);
            assert_ne!(cfg.fingerprint(), base_fp, "SSH {name}");
        }
    }

    #[test]
    fn drivers_round_trip_their_ids() {
        for driver in DriverType::KNOWN {
            assert_eq!(DriverType::parse(driver.as_str()), driver);
            assert!(driver.dialect().is_some(), "{driver}");
        }
        assert_eq!(DriverType::parse("mssql"), DriverType::SqlServer);
        assert_eq!(DriverType::Turso.dialect(), Some(SqlDialect::Sqlite));
        assert_eq!(DriverType::Unset.dialect(), None);
        assert!(!DriverType::parse("oracle").is_supported());
    }

    #[test]
    fn drivers_this_build_lacks_are_rejected() {
        for driver in DriverType::KNOWN.into_iter().filter(|d| !d.is_supported()) {
            let cfg = ConnectionConfig { driver: driver.clone(), ..pg() };
            assert_eq!(cfg.validate(), Err(format!("{driver} is not available in this build")));
        }
    }

    #[test]
    fn driver_round_trips_unknown_names() {
        let cfg: ConnectionConfig = serde_json::from_str(r#"{"driver":"oracle","ssh":{}}"#).unwrap();
        assert_eq!(cfg.driver, DriverType::Other("oracle".into()));
        assert_eq!(serde_json::to_value(&cfg).unwrap()["driver"], "oracle");
        assert_eq!(cfg.validate(), Err("unsupported driver: oracle".into()));
    }

    #[test]
    fn a_url_token_moves_to_its_own_field() {
        let mut cfg = ConnectionConfig {
            driver: DriverType::Turso,
            url: " libsql://db-org.turso.io?authToken=abc.def&tls=1 ".into(),
            ..Default::default()
        };
        cfg.normalize();
        assert_eq!((cfg.url.as_str(), cfg.auth_token.as_str()), ("libsql://db-org.turso.io?tls=1", "abc.def"));
        let mut kept = ConnectionConfig { url: "https://x?authToken=new".into(), auth_token: "old".into(), ..cfg };
        kept.normalize();
        assert_eq!((kept.url.as_str(), kept.auth_token.as_str()), ("https://x", "old"), "a typed token wins");
    }

    #[test]
    fn fingerprints_without_urls_are_unchanged() {
        let cfg = ConnectionConfig { driver: DriverType::Postgres, host: "h".into(), ..Default::default() };
        // The fingerprint before url and authToken existed.
        assert_eq!(cfg.fingerprint(), hex::encode(Sha256::digest(b"postgres|h|0|||||||false|false||0|||||||false")));
    }
}
