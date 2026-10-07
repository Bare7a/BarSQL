#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use barsql_core::{ConnectionConfig, DriverType, ObjectKind, ObjectRef};
use barsql_db::{Cancel, Cell, Engine, ScriptEvent, Session};
use barsql_sql::split_statement_texts;
use serde_json::{Map, Value as Json, json};

// Fixtures share one schema per engine, so their tests take turns.
pub async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

pub fn fixture(rel: &str) -> Json {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden").join(rel);
    serde_json::from_str(&fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .expect("fixture json")
}

pub fn strings(v: &Json) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

pub fn postgres_config() -> ConnectionConfig {
    ConnectionConfig {
        id: "golden-postgres".into(),
        driver: DriverType::Postgres,
        host: env_or("BARSQL_E2E_PG_HOST", "127.0.0.1"),
        port: env_or("BARSQL_E2E_PG_PORT", "55432").parse().unwrap(),
        username: env_or("BARSQL_E2E_PG_USER", "postgres"),
        password: env_or("BARSQL_E2E_PG_PASSWORD", "postgres"),
        database: env_or("BARSQL_E2E_PG_DB", "barsql_test"),
        ..Default::default()
    }
}

pub fn mysql_config(prefix: &str, port: &str) -> ConnectionConfig {
    ConnectionConfig {
        id: format!("golden-{prefix}"),
        driver: DriverType::MySql,
        host: env_or(&format!("BARSQL_E2E_{prefix}_HOST"), "127.0.0.1"),
        port: env_or(&format!("BARSQL_E2E_{prefix}_PORT"), port).parse().unwrap(),
        username: env_or(&format!("BARSQL_E2E_{prefix}_USER"), "root"),
        password: env_or(&format!("BARSQL_E2E_{prefix}_PASSWORD"), "root"),
        database: env_or(&format!("BARSQL_E2E_{prefix}_DB"), "barsql_test"),
        ..Default::default()
    }
}

// sqld from docker-compose.yml, or a Turso database given by URL and token.
pub fn turso_config() -> ConnectionConfig {
    ConnectionConfig {
        id: "golden-turso".into(),
        driver: DriverType::Turso,
        url: env_or("BARSQL_E2E_TURSO_URL", "http://127.0.0.1:38080"),
        auth_token: env_or("BARSQL_E2E_TURSO_TOKEN", ""),
        ..Default::default()
    }
}

// ClickHouse from docker-compose.yml over HTTP.
pub fn clickhouse_config() -> ConnectionConfig {
    ConnectionConfig {
        id: "golden-clickhouse".into(),
        driver: DriverType::ClickHouse,
        host: env_or("BARSQL_E2E_CH_HOST", "127.0.0.1"),
        port: env_or("BARSQL_E2E_CH_PORT", "38123").parse().unwrap(),
        username: env_or("BARSQL_E2E_CH_USER", "default"),
        password: env_or("BARSQL_E2E_CH_PASSWORD", "clickhouse"),
        database: env_or("BARSQL_E2E_CH_DB", "barsql_test"),
        ssl_mode: "disable".into(),
        ..Default::default()
    }
}

// SQL Server from docker-compose.yml's mssql profile, Azure SQL Edge on arm64. Its suites run only with
// BARSQL_E2E_MSSQL=1, since the profile is off by default.
pub fn mssql_config() -> Option<ConnectionConfig> {
    if env_or("BARSQL_E2E_MSSQL", "") != "1" {
        eprintln!("skipped: set BARSQL_E2E_MSSQL=1 and start the mssql profile");
        return None;
    }
    Some(ConnectionConfig {
        id: "golden-mssql".into(),
        driver: DriverType::SqlServer,
        host: env_or("BARSQL_E2E_MSSQL_HOST", "127.0.0.1"),
        port: env_or("BARSQL_E2E_MSSQL_PORT", "31433").parse().unwrap(),
        username: env_or("BARSQL_E2E_MSSQL_USER", "sa"),
        password: env_or("BARSQL_E2E_MSSQL_PASSWORD", "BarSQL-e2e-Passw0rd"),
        database: env_or("BARSQL_E2E_MSSQL_DB", "barsql_test"),
        ssl_mode: env_or("BARSQL_E2E_MSSQL_SSL", "require"),
        ..Default::default()
    })
}

// Creates the database, with readers that see the last committed rows instead of waiting on a writer's locks, as
// Postgres and MySQL readers do. Azure SQL works this way too; a plain SQL Server doesn't.
pub fn mssql_setup(database: &str) -> [String; 2] {
    [
        format!("IF DB_ID(N'{database}') IS NULL CREATE DATABASE [{database}]"),
        format!(
            "IF EXISTS (SELECT 1 FROM sys.databases WHERE name = N'{database}' AND is_read_committed_snapshot_on = 0)
            ALTER DATABASE [{database}] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE"
        ),
    ]
}

pub async fn mssql_engine(cfg: &ConnectionConfig) -> barsql_db::Engine {
    // Once, with the other tests waiting: switching the snapshot on restarts the database, failing their logins.
    static PREPARED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    PREPARED.get_or_init(|| prepare_mssql(cfg)).await;
    barsql_db::Engine::connect(cfg).await.expect("SQL Server is reachable")
}

// The stack's SQL Server has no barsql_test until the first run makes it. Logins fail for a few seconds after the
// container reports healthy, so this retries.
async fn prepare_mssql(cfg: &ConnectionConfig) {
    let master = ConnectionConfig { database: "master".into(), ..cfg.clone() };
    let mut last = None;
    for _ in 0..30 {
        match barsql_db::Engine::connect(&master).await {
            Ok(engine) => {
                let mut session = engine.session().await.unwrap();
                for sql in mssql_setup(&cfg.database) {
                    session.buffered(&sql, &Cancel::new()).await.unwrap();
                }
                engine.close().await;
                return;
            }
            Err(error) => last = Some(error),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    panic!("SQL Server is not reachable; start it with BARSQL_E2E_MSSQL=1 cargo xtask e2e up: {last:?}");
}

// BARSQL_GOLDEN_WRITE=1 rewrites fixtures from what the server returns, for a new engine or a deliberate change.
// Review the diff before keeping it.
pub fn golden_write() -> bool {
    std::env::var("BARSQL_GOLDEN_WRITE").is_ok_and(|v| v == "1")
}

pub fn write_fixture(rel: &str, value: &Json) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden").join(rel);
    fs::write(&path, serde_json::to_string_pretty(value).unwrap() + "\n")
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

// Each probe of `template` with the results this session gives, in fixture form.
pub async fn snapshot_probes(session: &mut Session, driver: &DriverType, template: &Json) -> Json {
    let mut out = Vec::new();
    for p in template.as_array().expect("probes") {
        let actual = probe(session, driver, p["sql"].as_str().unwrap()).await;
        let mut entry = json!({ "name": p["name"], "sql": p["sql"] });
        for (key, value) in fixture_probe(&actual).as_object().unwrap() {
            entry[key] = value.clone();
        }
        out.push(entry);
    }
    Json::Array(out)
}

pub fn sqlite_config(path: &std::path::Path) -> ConnectionConfig {
    ConnectionConfig {
        id: "golden-sqlite".into(),
        driver: DriverType::Sqlite,
        file_path: path.display().to_string(),
        ..Default::default()
    }
}

#[derive(Default)]
pub struct Report {
    pub differences: Vec<String>,
    pub deviations: Vec<String>,
}

impl Report {
    pub fn assert_clean(&self, engine: &str) {
        eprintln!("{engine} expected deviations: {:?}", self.deviations);
        assert!(
            self.differences.is_empty(),
            "{} {engine} case(s) differ from the fixtures:\n{}",
            self.differences.len(),
            self.differences.join("\n")
        );
    }

    pub fn check(&mut self, label: &str, listed: bool, expected: &Json, actual: &Json) {
        let diff = first_difference("", &normalized(expected), &normalized(actual));
        match (diff, listed) {
            (None, false) => {}
            (None, true) => self.differences.push(format!("{label} matches now; unlist it")),
            (Some(_), true) => self.deviations.push(label.to_string()),
            (Some(diff), false) => self.differences.push(format!("{label}: {diff}")),
        }
    }
}

pub async fn run_all(session: &mut Session, statements: &[String]) {
    let (tx, rx) = async_channel::unbounded();
    let _ = session.run_script(statements, &tx, &Cancel::new()).await;
    drop(tx);
    while rx.try_recv().is_ok() {}
}

// One entry per result set, in stream order.
pub async fn probe(session: &mut Session, driver: &DriverType, sql: &str) -> Json {
    let statements = split_statement_texts(driver, sql);
    let (tx, rx) = async_channel::unbounded();
    let script = session.run_script(&statements, &tx, &Cancel::new()).await;
    drop(tx);
    let mut results: BTreeMap<usize, Map<String, Json>> = BTreeMap::new();
    let mut order = Vec::new();
    let mut at = |idx: usize, results: &mut BTreeMap<usize, Map<String, Json>>| {
        if !results.contains_key(&idx) {
            order.push(idx);
        }
        results.entry(idx).or_default().len();
    };
    while let Ok(event) = rx.try_recv() {
        match event {
            ScriptEvent::Meta { result_index, columns } => {
                at(result_index, &mut results);
                let r = results.get_mut(&result_index).unwrap();
                if !columns.is_empty() {
                    r.insert("columns".into(), json!(columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>()));
                    r.insert("columnTypes".into(), json!(stable_types(columns.iter().map(|c| c.type_name.clone()))));
                }
            }
            ScriptEvent::Rows { result_index, chunk } => {
                at(result_index, &mut results);
                let r = results.get_mut(&result_index).unwrap();
                for row in 0..chunk.rows() {
                    let cells: Vec<Json> = (0..chunk.columns()).map(|c| cell_json(chunk.cell(row, c))).collect();
                    let display: Vec<Json> = (0..chunk.columns()).map(|c| json!(chunk.display(row, c))).collect();
                    push_array(r, "rows", json!(cells));
                    push_array(r, "display", json!(display));
                }
            }
            ScriptEvent::Messages { result_index, messages, dropped } => {
                at(result_index, &mut results);
                let r = results.get_mut(&result_index).unwrap();
                for m in messages {
                    push_array(r, "messages", json!({ "level": m.level, "code": m.code, "text": m.text }));
                }
                if dropped > 0 {
                    r.insert("messagesDropped".into(), json!(dropped));
                }
            }
            ScriptEvent::Result(result) => {
                at(result.result_index, &mut results);
                let r = results.get_mut(&result.result_index).unwrap();
                r.insert("statement".into(), json!(result.statement));
                let summary = result.summary.clone().unwrap_or_default();
                r.insert("rowCount".into(), json!(summary.row_count));
                r.insert("affectedRows".into(), json!(summary.affected_rows));
                if !summary.message.is_empty() {
                    r.insert("message".into(), json!(summary.message));
                }
                if result.plan.is_some() {
                    r.insert("plan".into(), json!(true));
                }
                if let Some(error) = &result.error {
                    r.insert("error".into(), serde_json::to_value(error).unwrap());
                }
            }
        }
    }
    let attributed = results.values().any(|r| r.contains_key("error"));
    let mut out = json!({ "results": order.iter().map(|i| Json::Object(results[i].clone())).collect::<Vec<_>>() });
    if let (Err(error), false) = (script, attributed) {
        out["scriptError"] = serde_json::to_value(error).unwrap();
    }
    out
}

fn push_array(r: &mut Map<String, Json>, key: &str, value: Json) {
    r.entry(key.to_string()).or_insert_with(|| json!([])).as_array_mut().unwrap().push(value);
}

fn cell_json(cell: Cell<'_>) -> Json {
    match cell {
        Cell::Null => Json::Null,
        Cell::Bool(b) => json!(b),
        Cell::Text(s) => json!(s),
        Cell::Number(s) => s.parse::<i64>().map(|i| json!(i)).unwrap_or_else(|_| json!(s.parse::<f64>().ok())),
    }
}

fn stable_types(types: impl Iterator<Item = String>) -> Vec<String> {
    types
        .map(|t| match t.parse::<u32>() {
            Ok(oid) if oid >= 16384 => "$userOid".to_string(),
            _ => t,
        })
        .collect()
}

pub fn fixture_probe(probe: &Json) -> Json {
    let keep = [
        "statement",
        "columns",
        "columnTypes",
        "rows",
        "display",
        "rowCount",
        "affectedRows",
        "message",
        "messages",
        "messagesDropped",
        "plan",
        "error",
    ];
    let results: Vec<Json> = probe["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| Json::Object(keep.iter().filter_map(|k| r.get(*k).map(|v| (k.to_string(), v.clone()))).collect()))
        .collect();
    let mut out = json!({ "results": results });
    if let Some(error) = probe.get("scriptError") {
        out["scriptError"] = error.clone();
    }
    out
}

// A probe marked `emitError` counts as a deviation without being listed.
pub fn undeliverable(probe: &Json) -> bool {
    probe["results"].as_array().is_some_and(|rs| rs.iter().any(|r| r.get("emitError").is_some()))
}

pub async fn compare_probes(
    session: &mut Session,
    driver: &DriverType,
    probes: &Json,
    label: &str,
    listed: &[&str],
    report: &mut Report,
) {
    for p in probes.as_array().expect("probes") {
        let name = p["name"].as_str().unwrap();
        let actual = probe(session, driver, p["sql"].as_str().unwrap()).await;
        if undeliverable(p) {
            report.deviations.push(format!("{label}/{name} (undeliverable)"));
            continue;
        }
        report.check(&format!("{label}/{name}"), listed.contains(&name), &fixture_probe(p), &actual);
    }
}

pub async fn schema_snapshot(engine: &Engine, schema: &str, fixture: &Json) -> Json {
    let schemas: Vec<String> =
        engine.list_schemas().await.unwrap().into_iter().map(|s| s.name).filter(|s| s == schema).collect();
    let mut tables = Vec::new();
    let mut ddl = Vec::new();
    for table in engine.list_tables(schema).await.unwrap() {
        let columns = engine.list_columns(schema, &table.name).await.unwrap();
        let indexes = engine.list_indexes(schema, &table.name).await.unwrap();
        let constraints = engine.list_constraints(schema, &table.name).await.unwrap();
        let triggers = engine.list_triggers(schema, &table.name).await.unwrap();
        let mut refs = vec![ObjectRef {
            schema: schema.into(),
            name: table.name.clone(),
            kind: ObjectKind::for_relation(&table.kind),
            ..Default::default()
        }];
        refs.extend(indexes.iter().map(|i| ObjectRef {
            schema: schema.into(),
            name: i.name.clone(),
            kind: ObjectKind::Index,
            table: table.name.clone(),
            ..Default::default()
        }));
        refs.extend(constraints.iter().map(|c| ObjectRef {
            schema: schema.into(),
            name: c.name.clone(),
            kind: ObjectKind::Constraint,
            table: table.name.clone(),
            ..Default::default()
        }));
        refs.extend(triggers.iter().map(|t| ObjectRef {
            schema: schema.into(),
            name: t.name.clone(),
            kind: ObjectKind::Trigger,
            table: table.name.clone(),
            ..Default::default()
        }));
        tables.push(json!({ "table": table, "columns": columns, "indexes": indexes, "constraints": constraints, "triggers": triggers }));
        ddl.extend(object_ddl(engine, refs).await);
    }
    let routines = engine.list_routines(schema).await.unwrap();
    let routine_refs = routines
        .iter()
        .map(|r| ObjectRef {
            schema: schema.into(),
            name: r.name.clone(),
            kind: r.kind.clone(),
            args: r.args.clone(),
            ..Default::default()
        })
        .collect();
    ddl.extend(object_ddl(engine, routine_refs).await);
    // ClickHouse's own functions belong to the server, not a database.
    let functions: Vec<_> = engine
        .list_functions()
        .await
        .unwrap()
        .functions
        .into_iter()
        .filter(|f| f.schema == schema || (f.schema.is_empty() && !f.builtin))
        .collect();
    json!({
        "engine": fixture["engine"], "schema": schema, "setup": fixture["setup"], "schemas": schemas,
        "tables": tables, "routines": routines, "functions": functions, "ddl": ddl,
    })
}

async fn object_ddl(engine: &Engine, refs: Vec<ObjectRef>) -> Vec<Json> {
    let mut out = Vec::new();
    for object in refs {
        let mut entry = json!({ "ref": object });
        match engine.object_ddl(&object).await {
            Ok(ddl) if !ddl.is_empty() => entry["ddl"] = json!(ddl),
            Ok(_) => {}
            Err(err) => entry["error"] = json!(err.message),
        }
        out.push(entry);
    }
    out
}

// Numbers compare by value, and null equals an empty list.
pub fn normalized(v: &Json) -> Json {
    match v {
        Json::Null => Json::Null,
        Json::Number(n) => json!(n.as_f64()),
        Json::Array(items) if items.is_empty() => Json::Null,
        Json::Array(items) => Json::Array(items.iter().map(normalized).collect()),
        Json::Object(map) => Json::Object(
            map.iter()
                .filter(|(_, v)| !matches!(v, Json::Array(a) if a.is_empty()) && !v.is_null())
                .map(|(k, v)| (k.clone(), normalized(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

// Writes <name>.actual.json beside a fixture that drifted.
pub fn assert_matches(rel: &str, expected: &Json, actual: &Json) {
    if let Some(diff) = first_difference("", &normalized(expected), &normalized(actual)) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/golden")
            .join(rel.replace(".json", ".actual.json"));
        let _ = fs::write(&path, serde_json::to_string_pretty(actual).unwrap());
        panic!("{rel} differs from the fixture ({}): {diff}", path.display());
    }
}

pub fn first_difference(path: &str, expected: &Json, actual: &Json) -> Option<String> {
    match (expected, actual) {
        (Json::Object(a), Json::Object(b)) => {
            for key in a.keys().chain(b.keys()) {
                let (x, y) = (a.get(key).unwrap_or(&Json::Null), b.get(key).unwrap_or(&Json::Null));
                if let Some(diff) = first_difference(&format!("{path}.{key}"), x, y) {
                    return Some(diff);
                }
            }
            None
        }
        (Json::Array(a), Json::Array(b)) if a.len() == b.len() => {
            a.iter().zip(b).enumerate().find_map(|(i, (x, y))| first_difference(&format!("{path}[{i}]"), x, y))
        }
        _ if expected == actual => None,
        _ => Some(format!("{path}: expected {expected} actual {actual}")),
    }
}
