// URLs as other tools write them, JDBC's too.

use crate::config::{ConnectionConfig, DriverType, named_instance};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UrlParts {
    pub driver: DriverType,
    pub host: Option<String>,
    pub port: Option<i64>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub ssl_mode: Option<String>,
    pub instance: Option<String>,
    pub file_path: Option<String>,
    pub url: Option<String>,
    pub auth_token: Option<String>,
}

pub fn parse_connection_url(text: &str) -> Option<UrlParts> {
    let text = text.trim();
    let text = text.strip_prefix("jdbc:").unwrap_or(text);
    let (scheme, rest) = text.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let driver = match scheme.as_str() {
        "postgres" | "postgresql" => DriverType::Postgres,
        "mysql" | "mariadb" => DriverType::MySql,
        "sqlserver" | "mssql" => DriverType::SqlServer,
        "clickhouse" | "ch" => DriverType::ClickHouse,
        "libsql" => DriverType::Turso,
        "sqlite" | "file" => {
            let path = local_path(rest.split(['?', '#']).next().unwrap_or_default())?;
            return (!path.is_empty()).then(|| UrlParts {
                driver: DriverType::Sqlite,
                file_path: Some(path),
                ..Default::default()
            });
        }
        _ => return None,
    };
    if driver == DriverType::Turso {
        let (url, auth_token) = split_token(&format!("libsql://{rest}"));
        return Some(UrlParts { driver, url: Some(url), auth_token, ..Default::default() });
    }
    let sql_server = driver == DriverType::SqlServer;
    // JDBC's SQL Server form: host:port;databaseName=db;user=sa;password=…
    let (rest, jdbc) = match rest.split_once(';') {
        Some((rest, tail)) if sql_server => (rest, tail),
        _ => (rest, ""),
    };
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (login, address) = authority.rsplit_once('@').map_or((None, authority), |(login, at)| (Some(login), at));
    let mut parts = UrlParts { driver, ..Default::default() };
    if let Some(login) = login {
        let (user, password) = login.split_once(':').map_or((login, None), |(user, pass)| (user, Some(pass)));
        parts.username = (!user.is_empty()).then(|| decode(user));
        parts.password = password.map(decode);
    }
    // [::1]:5432 keeps its brackets out of the host, and [::1]%5CNAME its instance in.
    let (host, port) = match address.strip_prefix('[').and_then(|v6| v6.split_once(']')) {
        Some((host, after)) => {
            let (instance, port) =
                after.rsplit_once(':').map_or((after, None), |(instance, port)| (instance, Some(port)));
            (format!("{host}{instance}"), port)
        }
        None => address.rsplit_once(':').map_or((address.to_string(), None), |(host, port)| (host.into(), Some(port))),
    };
    let mut host = decode(&host);
    if sql_server && let Some(at) = host.find('\\') {
        parts.instance = Some(host[at + 1..].to_string()).filter(|instance| !instance.is_empty());
        host.truncate(at);
    }
    parts.host = (!host.is_empty()).then_some(host);
    parts.port = port.and_then(|port| port.parse().ok());
    let path = decode(path.trim_end_matches('/'));
    parts.database = (!path.is_empty()).then(|| path.clone());
    let params: Vec<_> = params(query).into_iter().chain(jdbc_params(jdbc)).collect();
    parts.ssl_mode = tls(&params, sql_server, scheme == "mariadb").map(Into::into);
    for (key, value) in params {
        match key.to_ascii_lowercase().as_str() {
            "user" | "username" => parts.username = Some(value),
            "password" => parts.password = Some(value),
            "database" | "databasename" | "dbname" => {
                // go-mssqldb puts a named instance where other URLs put the database.
                if sql_server && !path.is_empty() {
                    parts.instance = Some(path.clone());
                }
                parts.database = Some(value);
            }
            "instancename" => parts.instance = Some(value),
            _ => {}
        }
    }
    // JDBC and go-mssqldb connect to a given port and ignore the instance.
    if parts.port.is_some() {
        parts.instance = None;
    }
    Some(parts)
}

// A named mode wins. An on/off switch means a checked certificate, as its drivers default, unless the URL says
// otherwise. MySQL's Connector/J, unlike MariaDB's, checks a useSSL certificate only with verifyServerCertificate.
fn tls(params: &[(String, String)], sql_server: bool, mariadb: bool) -> Option<&'static str> {
    let (mut named, mut switch, mut verify, mut checks) = (None, None, None, true);
    for (key, value) in params {
        let setting = || value.to_ascii_lowercase().replace('_', "-");
        match key.to_ascii_lowercase().as_str() {
            "sslmode" | "ssl-mode" | "ssl_mode" => match setting().as_str() {
                "strict" if !sql_server => verify = Some(true),
                "none" => verify = Some(false),
                mode => named = tls_mode(mode).or(named),
            },
            key @ ("ssl" | "encrypt" | "secure" | "usessl" | "requiressl") => {
                checks &= mariadb || !matches!(key, "usessl" | "requiressl");
                match setting().as_str() {
                    "strict" => named = Some(if sql_server { "strict" } else { "verify-full" }),
                    // postgres.js takes libpq's mode names here.
                    value => match tls_mode(value) {
                        Some(mode) => named = Some(mode),
                        None => switch = on(value).or(switch),
                    },
                }
            }
            "trustservercertificate" | "skip_verify" => verify = on(&setting()).map(|trust| !trust),
            "verifyservercertificate" => verify = on(&setting()),
            _ => {}
        }
    }
    let switched = switch.map(|on| match (on, verify.unwrap_or(checks)) {
        (false, _) => "disable",
        (true, true) => "verify-full",
        (true, false) => "require",
    });
    named.or(switched)
}

// Turso's dashboard hands out URLs with `?authToken=…`.
pub(crate) fn split_token(url: &str) -> (String, Option<String>) {
    let Some((base, query)) = url.split_once('?') else { return (url.to_string(), None) };
    let mut token = None;
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| match pair.split_once('=') {
            Some((key, value)) if key.eq_ignore_ascii_case("authToken") => {
                token = Some(decode(value));
                false
            }
            _ => !pair.is_empty(),
        })
        .collect();
    let url = if kept.is_empty() { base.to_string() } else { format!("{base}?{}", kept.join("&")) };
    (url, token)
}

impl UrlParts {
    pub fn apply(self, config: &mut ConnectionConfig) {
        config.driver = self.driver;
        let fill = |field: &mut String, value: Option<String>| {
            if let Some(value) = value {
                *field = value;
            }
        };
        fill(&mut config.host, self.host);
        fill(&mut config.database, self.database);
        fill(&mut config.username, self.username);
        fill(&mut config.password, self.password);
        fill(&mut config.ssl_mode, self.ssl_mode);
        fill(&mut config.instance, self.instance);
        fill(&mut config.file_path, self.file_path);
        fill(&mut config.url, self.url);
        fill(&mut config.auth_token, self.auth_token);
        if let Some(port) = self.port {
            config.port = port;
        }
    }

    // What an unnamed connection is called after a paste, as Browse names a file.
    pub fn name(&self) -> Option<String> {
        let name = match self.driver {
            DriverType::Sqlite => self.file_path.as_deref().and_then(|path| path.rsplit(['/', '\\']).next()),
            DriverType::Turso => {
                self.url.as_deref().and_then(|url| url.split_once("://")?.1.split(['/', '?', ':']).next())
            }
            _ => self.database.as_deref().filter(|database| !database.is_empty()).or(self.host.as_deref()),
        };
        name.filter(|name| !name.is_empty()).map(Into::into)
    }

    // Another server or login, for which a form keeps nothing the URL leaves out. Host names ignore case, and a
    // named instance is a server of its own.
    pub fn other_server(&self, config: &ConnectionConfig) -> bool {
        let server = |host: &str, instance: &str| {
            let (host, instance) = named_instance(host.trim(), instance);
            (host.to_lowercase(), instance.to_lowercase())
        };
        let moved = self.host.as_deref().is_some_and(|host| {
            server(host, self.instance.as_deref().unwrap_or_default()) != server(&config.host, &config.instance)
        });
        let changes = |given: &Option<String>, had: &str| given.as_deref().is_some_and(|given| given != had);
        self.driver != config.driver
            || moved
            || changes(&self.username, &config.username)
            || changes(&self.url, &config.url)
    }
}

// Never carries the password or token.
pub fn connection_url(config: &ConnectionConfig) -> Option<String> {
    let scheme = match config.driver {
        DriverType::Postgres => "postgres",
        DriverType::MySql => "mysql",
        DriverType::SqlServer => "sqlserver",
        DriverType::ClickHouse => "clickhouse",
        DriverType::Sqlite => {
            return (!config.file_path.is_empty()).then(|| format!("sqlite://{}", file_url_path(&config.file_path)));
        }
        DriverType::Turso | DriverType::Other(_) | DriverType::Unset => return None,
    };
    if config.host.is_empty() {
        return None;
    }
    let sql_server = config.driver == DriverType::SqlServer;
    let user = if config.username.is_empty() { String::new() } else { format!("{}@", encode(&config.username)) };
    let (host, instance) =
        if sql_server { named_instance(&config.host, &config.instance) } else { (config.host.as_str(), "") };
    // A Postgres socket directory, as libpq writes one. A % starts an IPv6 zone.
    let host = if host.starts_with('/') { encode(host) } else { host.replace('%', "%25") };
    let host = if host.contains(':') { format!("[{host}]") } else { host };
    // SQL Server finds a named instance's port itself.
    let address = if config.port > 0 && instance.is_empty() { format!("{host}:{}", config.port) } else { host };
    let mut query = Vec::new();
    let path = if sql_server {
        // go-mssqldb's form, whose path names the instance, so a URL with one names its database too.
        if !config.database.is_empty() || !instance.is_empty() {
            query.push(format!("database={}", encode(&config.database)));
        }
        if instance.is_empty() { String::new() } else { format!("/{}", encode(instance)) }
    } else if config.database.is_empty() {
        String::new()
    } else {
        format!("/{}", encode(&config.database))
    };
    // Each engine's own spelling, as its tools read it. MySQL's and ClickHouse's default, disable, goes unsaid.
    let tls = match (&config.driver, config.ssl_mode.as_str()) {
        (_, "") => "",
        (DriverType::Postgres, "disable") => "sslmode=disable",
        (DriverType::Postgres, "require") => "sslmode=require",
        (DriverType::Postgres, _) => "sslmode=verify-full",
        (DriverType::SqlServer, "disable") => "encrypt=false",
        (DriverType::SqlServer, "require") => "encrypt=true&trustServerCertificate=true",
        (DriverType::SqlServer, "strict") => "encrypt=strict",
        (DriverType::SqlServer, _) => "encrypt=true",
        (DriverType::MySql, "require") => "ssl-mode=REQUIRED",
        (DriverType::MySql, "verify-full") => "ssl-mode=VERIFY_IDENTITY",
        (DriverType::ClickHouse, "require") => "secure=true&skip_verify=true",
        (DriverType::ClickHouse, "verify-full") => "secure=true",
        _ => "",
    };
    query.extend((!tls.is_empty()).then(|| tls.to_string()));
    let query = if query.is_empty() { String::new() } else { format!("?{}", query.join("&")) };
    Some(format!("{scheme}://{user}{address}{path}{query}"))
}

// macOS hands Finder opens over as file:// URLs.
pub fn path_from_file_url(url: &str) -> Option<String> {
    local_path(url.strip_prefix("file://")?)
}

// After file:// or sqlite://, `localhost` is this machine and /C:/x.db a Windows drive.
fn local_path(rest: &str) -> Option<String> {
    let rest = match rest.strip_prefix("localhost") {
        Some(after) if after.is_empty() || after.starts_with('/') => after,
        _ => rest,
    };
    let path = String::from_utf8(unescape(rest)).ok()?;
    let drive = path.len() > 2 && path.as_bytes()[2] == b':' && path.starts_with('/');
    Some(if drive { path[1..].to_string() } else { path })
}

fn file_url_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let drive = bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':');
    let path = if drive { format!("/{}", path.replace('\\', "/")) } else { path.to_string() };
    path.bytes().map(|b| if matches!(b, b'/' | b':') { (b as char).to_string() } else { escape(b) }).collect()
}

// BarSQL has no try-TLS mode, so prefer and allow read as disable.
fn tls_mode(mode: &str) -> Option<&'static str> {
    match mode {
        "disable" | "disabled" | "allow" | "prefer" | "preferred" => Some("disable"),
        "require" | "required" | "no-verify" | "trust" => Some("require"),
        "verify-ca" | "verify-full" | "verify-identity" => Some("verify-full"),
        "strict" => Some("strict"),
        _ => None,
    }
}

fn on(value: &str) -> Option<bool> {
    match value {
        "true" | "yes" | "1" | "mandatory" => Some(true),
        "false" | "no" | "0" | "optional" => Some(false),
        _ => None,
    }
}

fn params(query: &str) -> Vec<(String, String)> {
    query.split('&').filter_map(|pair| pair.split_once('=')).map(|(key, value)| (decode(key), decode(value))).collect()
}

// `key={a;b}` quotes a value holding semicolons, with `}}` for a brace.
fn jdbc_params(tail: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut rest = tail;
    while let Some((key, after)) = rest.split_once('=') {
        let key = key.rsplit(';').next().unwrap_or(key).trim().to_string();
        let after = after.trim_start();
        let Some(quoted) = after.strip_prefix('{') else {
            let (value, next) = after.split_once(';').unwrap_or((after, ""));
            pairs.push((key, value.trim().to_string()));
            rest = next;
            continue;
        };
        let mut value = String::new();
        let mut chars = quoted.char_indices().peekable();
        let mut end = quoted.len();
        while let Some((at, c)) = chars.next() {
            if c == '}' && chars.next_if(|&(_, next)| next == '}').is_none() {
                end = at + 1;
                break;
            }
            value.push(c);
        }
        pairs.push((key, value));
        rest = quoted[end..].split_once(';').map_or("", |(_, next)| next);
    }
    pairs
}

fn decode(text: &str) -> String {
    String::from_utf8_lossy(&unescape(text)).into_owned()
}

fn unescape(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|b| (*b as char).to_digit(16));
        match (bytes[ix], hex(ix + 1), hex(ix + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                ix += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                ix += 1;
            }
        }
    }
    out
}

fn encode(text: &str) -> String {
    text.bytes().map(escape).collect()
}

fn escape(byte: u8) -> String {
    match byte {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (byte as char).to_string(),
        _ => format!("%{byte:02X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_urls_fill_every_part() {
        let parts =
            parse_connection_url("postgresql://app%40corp:p%3Ass@db.internal:6543/shop?sslmode=verify-ca").unwrap();
        assert_eq!(parts.driver, DriverType::Postgres);
        assert_eq!(parts.username.as_deref(), Some("app@corp"));
        assert_eq!(parts.password.as_deref(), Some("p:ss"));
        assert_eq!((parts.host.as_deref(), parts.port), (Some("db.internal"), Some(6543)));
        assert_eq!(parts.database.as_deref(), Some("shop"));
        assert_eq!(parts.ssl_mode.as_deref(), Some("verify-full"));
        let bare = parse_connection_url("postgres://localhost").unwrap();
        assert_eq!((bare.host.as_deref(), bare.port, bare.database), (Some("localhost"), None, None));
    }

    #[test]
    fn other_engines_and_their_own_forms() {
        let my = parse_connection_url(" mariadb://root@127.0.0.1:3307/app ").unwrap();
        assert_eq!((my.driver, my.port, my.database.as_deref()), (DriverType::MySql, Some(3307), Some("app")));
        let jdbc = parse_connection_url(
            "jdbc:sqlserver://sql.local:1433;databaseName=billing;user=sa;password=x;encrypt=true",
        )
        .unwrap();
        assert_eq!(jdbc.driver, DriverType::SqlServer);
        assert_eq!((jdbc.database.as_deref(), jdbc.username.as_deref()), (Some("billing"), Some("sa")));
        assert_eq!(jdbc.ssl_mode.as_deref(), Some("verify-full"));
        let braced =
            parse_connection_url("sqlserver://h:1433;database=db;user={sa};password={p@ss;w0}}rd};encrypt=true")
                .unwrap();
        assert_eq!((braced.username.as_deref(), braced.password.as_deref()), (Some("sa"), Some("p@ss;w0}rd")));
        assert_eq!(braced.ssl_mode.as_deref(), Some("verify-full"));
        let v6 = parse_connection_url("clickhouse://[::1]:8123/default").unwrap();
        assert_eq!((v6.host.as_deref(), v6.port), (Some("::1"), Some(8123)));
        let turso = parse_connection_url("libsql://shop-acme.turso.io?authToken=abc").unwrap();
        assert_eq!(turso.url.as_deref(), Some("libsql://shop-acme.turso.io"));
        assert_eq!(turso.auth_token.as_deref(), Some("abc"));
        let local = parse_connection_url("libsql://127.0.0.1:8080?tls=0&authToken=abc").unwrap();
        assert_eq!(local.url.as_deref(), Some("libsql://127.0.0.1:8080?tls=0"));
        let file = parse_connection_url("sqlite:///Users/me/app%20data.db").unwrap();
        assert_eq!(file.file_path.as_deref(), Some("/Users/me/app data.db"));
        assert_eq!(
            parse_connection_url("sqlite:///C:/data/app.db").unwrap().file_path.as_deref(),
            Some("C:/data/app.db")
        );
        assert_eq!(parse_connection_url("https://example.com"), None);
        assert_eq!(parse_connection_url("not a url"), None);
    }

    #[test]
    fn tls_settings_map_onto_the_modes_barsql_offers() {
        let mode = |url: &str| parse_connection_url(url).unwrap().ssl_mode;
        let cases = [
            ("mysql://h/app?ssl-mode=DISABLED", Some("disable")),
            ("mysql://h/app?ssl-mode=PREFERRED", Some("disable")),
            ("mysql://h/app?ssl-mode=REQUIRED", Some("require")),
            ("jdbc:mysql://h/app?sslMode=VERIFY_CA", Some("verify-full")),
            ("jdbc:mariadb://h/app?sslMode=trust", Some("require")),
            ("postgres://h/db?sslmode=no-verify", Some("require")),
            ("postgres://h/db?sslmode=sometimes", None),
            ("jdbc:postgresql://h/db?ssl=true", Some("verify-full")),
            ("postgres://h/db?ssl=verify-full", Some("verify-full")),
            ("postgres://h/db?ssl=require", Some("require")),
            ("postgres://h/db?ssl=prefer", Some("disable")),
            ("jdbc:sqlserver://h:1433;encrypt=strict", Some("strict")),
            ("jdbc:sqlserver://h:1433;encrypt=mandatory", Some("verify-full")),
            ("jdbc:sqlserver://h:1433;encrypt=true;trustServerCertificate=false", Some("verify-full")),
            ("jdbc:sqlserver://h:1433;encrypt=true;trustServerCertificate=true", Some("require")),
            ("jdbc:sqlserver://h:1433;encrypt=false", Some("disable")),
            ("jdbc:mysql://h/app?useSSL=true&requireSSL=true", Some("require")),
            ("jdbc:mysql://h/app?useSSL=true&verifyServerCertificate=true", Some("verify-full")),
            ("jdbc:mariadb://h/app?useSsl=true", Some("verify-full")),
            ("jdbc:mariadb://h/app?useSsl=true&trustServerCertificate=true", Some("require")),
            ("clickhouse://h/db?secure=true", Some("verify-full")),
            ("clickhouse://h/db?secure=true&skip_verify=true", Some("require")),
            ("jdbc:clickhouse://h/db?ssl=true&sslmode=strict", Some("verify-full")),
            ("jdbc:clickhouse://h/db?ssl=true&sslmode=none", Some("require")),
        ];
        for (url, expected) in cases {
            assert_eq!(mode(url).as_deref(), expected, "{url}");
        }
    }

    #[test]
    fn named_instances_in_each_form() {
        let instance = |url: &str| {
            let parts = parse_connection_url(url).unwrap();
            (parts.host.unwrap(), parts.instance, parts.database)
        };
        let named =
            |database: Option<&str>| ("db01".to_string(), Some("SQLEXPRESS".to_string()), database.map(Into::into));
        assert_eq!(instance(r"jdbc:sqlserver://db01\SQLEXPRESS;databaseName=shop"), named(Some("shop")));
        assert_eq!(instance("sqlserver://sa@db01/SQLEXPRESS?database=shop"), named(Some("shop")));
        assert_eq!(instance("jdbc:sqlserver://db01;instanceName=SQLEXPRESS"), named(None));
        let v6 = parse_connection_url("sqlserver://[::1]%5CSQLEXPRESS/master").unwrap();
        assert_eq!((v6.host.as_deref(), v6.instance.as_deref()), (Some("::1"), Some("SQLEXPRESS")));
        for url in
            [r"jdbc:sqlserver://db01\SQLEXPRESS:1500;databaseName=shop", "sqlserver://[::1]%5CSQLEXPRESS:1500/shop"]
        {
            let parts = parse_connection_url(url).unwrap();
            assert_eq!((parts.port, parts.instance), (Some(1500), None), "a port wins over the instance: {url}");
        }
    }

    #[test]
    fn another_server_or_login_is_told_apart() {
        let current = ConnectionConfig {
            driver: DriverType::SqlServer,
            host: r"SQLPROD01\INST1".into(),
            username: "sa".into(),
            ..Default::default()
        };
        let other = |url: &str| parse_connection_url(url).unwrap().other_server(&current);
        assert!(!other(r"jdbc:sqlserver://sqlprod01\inst1;databaseName=x"), "names ignore case");
        assert!(!other("sqlserver://sa@SQLPROD01/INST1?database=x"));
        assert!(other("sqlserver://sa@SQLPROD01/INST2?database=x"), "another instance");
        assert!(other("sqlserver://bob@SQLPROD01/INST1?database=x"), "another login");
        assert!(other("postgres://SQLPROD01/x"), "another engine");
    }

    #[test]
    fn a_paste_names_the_connection_after_what_it_points_at() {
        let name = |url: &str| parse_connection_url(url).unwrap().name();
        assert_eq!(name("postgres://db.internal/shop").as_deref(), Some("shop"));
        assert_eq!(name("postgres://db.internal").as_deref(), Some("db.internal"));
        assert_eq!(name("sqlite:///Users/me/app.db").as_deref(), Some("app.db"));
        assert_eq!(name("libsql://shop-acme.turso.io?authToken=x").as_deref(), Some("shop-acme.turso.io"));
    }

    #[test]
    fn a_config_turns_back_into_a_url_without_secrets() {
        let mut config = ConnectionConfig { password: "secret".into(), ..Default::default() };
        parse_connection_url("postgres://me:secret@db:5432/shop?sslmode=require").unwrap().apply(&mut config);
        assert_eq!(connection_url(&config).as_deref(), Some("postgres://me@db:5432/shop?sslmode=require"));
        assert_eq!(connection_url(&ConnectionConfig { driver: DriverType::Postgres, ..Default::default() }), None);
        let sales = ConnectionConfig {
            driver: DriverType::SqlServer,
            host: "db01".into(),
            port: 1433,
            username: "sa".into(),
            database: "shop".into(),
            ssl_mode: "verify-full".into(),
            ..Default::default()
        };
        // go-mssqldb's own form, so its tools read it the same way.
        assert_eq!(connection_url(&sales).as_deref(), Some("sqlserver://sa@db01:1433?database=shop&encrypt=true"));
        let named = ConnectionConfig { instance: "SQLEXPRESS".into(), database: String::new(), ..sales };
        assert_eq!(connection_url(&named).as_deref(), Some("sqlserver://sa@db01/SQLEXPRESS?database=&encrypt=true"));
    }

    #[test]
    fn a_copy_pastes_back_as_the_same_connection() {
        let round_trip = |config: &ConnectionConfig| {
            let mut back = ConnectionConfig::default();
            parse_connection_url(&connection_url(config).unwrap()).unwrap().apply(&mut back);
            back
        };
        let strict = ConnectionConfig {
            driver: DriverType::SqlServer,
            host: "db01".into(),
            port: 1433,
            instance: "SQLEXPRESS".into(),
            database: "master".into(),
            ssl_mode: "strict".into(),
            ..Default::default()
        };
        let back = round_trip(&strict);
        assert_eq!((&back.host, &back.instance, &back.database), (&strict.host, &strict.instance, &strict.database));
        assert_eq!(back.ssl_mode, "strict");
        let both = ConnectionConfig { host: r"db01\A".into(), instance: "B".into(), database: String::new(), ..strict };
        let back = round_trip(&both);
        assert_eq!((back.host.as_str(), back.instance.as_str()), ("db01", "B"), "the Instance field wins");
        let zone = ConnectionConfig {
            driver: DriverType::Postgres,
            host: "fe80::1%12".into(),
            port: 5432,
            ..Default::default()
        };
        assert_eq!(round_trip(&zone).host, zone.host);
        let file = ConnectionConfig {
            driver: DriverType::Sqlite,
            file_path: "/Users/me/Q3 #2.db".into(),
            ..Default::default()
        };
        assert_eq!(connection_url(&file).as_deref(), Some("sqlite:///Users/me/Q3%20%232.db"));
        assert_eq!(round_trip(&file).file_path, file.file_path);
        let windows =
            ConnectionConfig { driver: DriverType::Sqlite, file_path: r"C:\data\app.db".into(), ..Default::default() };
        assert_eq!(connection_url(&windows).as_deref(), Some("sqlite:///C:/data/app.db"));
        assert_eq!(round_trip(&windows).file_path, "C:/data/app.db");
        let relative = ConnectionConfig {
            driver: DriverType::Sqlite,
            file_path: "localhost_backup.db".into(),
            ..Default::default()
        };
        assert_eq!(round_trip(&relative).file_path, relative.file_path);
        let socket = ConnectionConfig {
            driver: DriverType::Postgres,
            host: "/var/run/postgresql".into(),
            port: 5432,
            database: "db".into(),
            ..Default::default()
        };
        let back = round_trip(&socket);
        assert_eq!((back.host, back.database), (socket.host, socket.database));
        let mysql = |ssl_mode: &str| ConnectionConfig {
            driver: DriverType::MySql,
            host: "db".into(),
            ssl_mode: ssl_mode.into(),
            ..Default::default()
        };
        assert_eq!(connection_url(&mysql("disable")).as_deref(), Some("mysql://db"), "the default goes unsaid");
        assert_eq!(connection_url(&mysql("require")).as_deref(), Some("mysql://db?ssl-mode=REQUIRED"));
        for mode in ["require", "verify-full"] {
            for driver in [DriverType::SqlServer, DriverType::ClickHouse, DriverType::Postgres, DriverType::MySql] {
                let config = ConnectionConfig { driver: driver.clone(), ssl_mode: mode.into(), ..mysql("") };
                assert_eq!(round_trip(&config).ssl_mode, mode, "{driver:?}");
            }
        }
    }

    #[test]
    fn finder_urls_become_paths() {
        assert_eq!(
            path_from_file_url("file:///Users/me/My%20Notes.sqlite").as_deref(),
            Some("/Users/me/My Notes.sqlite")
        );
        assert_eq!(path_from_file_url("file://localhost/tmp/a.db").as_deref(), Some("/tmp/a.db"));
        assert_eq!(path_from_file_url("file:///C:/data/b%C3%A4r.db").as_deref(), Some("C:/data/bär.db"));
        assert_eq!(path_from_file_url("file:///tmp/100%.db").as_deref(), Some("/tmp/100%.db"));
        assert_eq!(path_from_file_url("https://example.com/a.db"), None);
    }
}
