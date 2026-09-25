use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum DriverType {
    Sqlite,
    Postgres,
    MySql,
    Other(String),
    #[default]
    Unset,
}

impl DriverType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::Other(name) => name,
            Self::Unset => "",
        }
    }

    pub fn parse(name: &str) -> Self {
        match name {
            "sqlite" => Self::Sqlite,
            "postgres" => Self::Postgres,
            "mysql" => Self::MySql,
            "" => Self::Unset,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Sqlite | Self::Postgres | Self::MySql)
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ssl_mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
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
        self.ssh.normalize();
    }

    pub fn validate(&self) -> Result<(), String> {
        match self.driver {
            DriverType::Sqlite => {
                if self.file_path.is_empty() {
                    return Err("SQLite database file path is required".into());
                }
            }
            DriverType::Postgres => {
                require(&self.host, "host is required")?;
                require(&self.database, "database name is required (e.g. blog - not the username \"postgres\")")?;
                require(&self.username, "username is required")?;
            }
            DriverType::MySql => {
                require(&self.host, "host is required")?;
                require(&self.database, "database name is required")?;
                require(&self.username, "username is required")?;
            }
            _ => return Err(format!("unsupported driver: {}", self.driver)),
        }
        if self.driver != DriverType::Sqlite {
            return self.ssh.validate();
        }
        Ok(())
    }

    pub fn default_browse_schema(&self) -> String {
        if !self.schema.is_empty() {
            return self.schema.clone();
        }
        match self.driver {
            DriverType::Sqlite => "main".into(),
            DriverType::MySql => self.database.clone(),
            _ => "public".into(),
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
        let mutations: [Mutation; 10] = [
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
    fn driver_round_trips_unknown_names() {
        let cfg: ConnectionConfig = serde_json::from_str(r#"{"driver":"oracle","ssh":{}}"#).unwrap();
        assert_eq!(cfg.driver, DriverType::Other("oracle".into()));
        assert_eq!(serde_json::to_value(&cfg).unwrap()["driver"], "oracle");
        assert_eq!(cfg.validate(), Err("unsupported driver: oracle".into()));
    }
}
