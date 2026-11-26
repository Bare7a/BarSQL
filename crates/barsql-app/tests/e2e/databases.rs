use barsql_core::{ConnectionConfig, DriverType, scope};

use crate::harness::{E2e, Kind, run, unique_table};
use crate::support::text;

fn current_database(driver: &DriverType) -> &'static str {
    match driver {
        DriverType::Postgres => "SELECT current_database()",
        DriverType::SqlServer => "SELECT DB_NAME()",
        DriverType::ClickHouse => "SELECT currentDatabase()",
        _ => "SELECT DATABASE()",
    }
}

async fn scalar(e: &E2e, id: &str, sql: &str) -> String {
    let result =
        e.app.execute_query(id, sql).await.unwrap_or_else(|err| panic!("[{}] {sql:?}: {err:?}", e.kind.name()));
    text(&result.rows[0][0])
}

each_engine!(async fn every_database_is_listed_and_runs_on_its_own_pool(e) {
    require!(e, all_databases);
    let second = e.second_database().await;
    let own = e.kind.config().default_database();
    let listed = e.app.database_list(&e.id).await.unwrap();
    let names: Vec<&str> = listed.iter().map(|d| d.name.as_str()).collect();
    for name in [own.as_str(), second.as_str()] {
        let found = listed.iter().find(|d| d.name == name).unwrap_or_else(|| panic!("{name} not in {names:?}"));
        assert!(found.can_connect && !found.system, "{found:?}");
    }
    assert!(listed.iter().any(|d| d.system), "system databases are listed and flagged: {names:?}");
    let sizes = e.app.database_sizes(&e.id).await.unwrap_or_default();
    if matches!(e.kind, Kind::Postgres | Kind::SqlServer) {
        assert!(sizes.contains_key(&own), "{sizes:?}");
    }

    let scoped = scope::scoped_id(&e.id, &second);
    let current = current_database(&e.driver());
    assert_eq!(scalar(&e, &scoped, current).await, second);
    assert_eq!(scalar(&e, &e.id, current).await, own);
    assert!(e.app.is_connected(&scoped));
    e.app.disconnect(&e.id).await;
    assert!(!e.app.is_connected(&scoped), "the connection closes with its databases");
});

// CONNECT decides: a login without it still sees the database, as one it can't open and without a size.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_marks_databases_a_login_cant_open() {
    run(Kind::Postgres, |e| async move {
        let second = e.second_database().await;
        let role = unique_table("no_connect");
        e.exec(&format!("CREATE ROLE {role} LOGIN PASSWORD 'pw'")).await;
        e.defer(format!("DROP ROLE IF EXISTS {role}"));
        e.exec(&format!("REVOKE CONNECT ON DATABASE {second} FROM PUBLIC")).await;
        e.defer(format!("GRANT CONNECT ON DATABASE {second} TO PUBLIC"));
        let config = ConnectionConfig { username: role.clone(), password: "pw".into(), ..Kind::Postgres.config() };
        let id = e.app.save_connection(config).await.unwrap().id;
        let listed = e.app.database_list(&id).await.unwrap();
        let locked = listed.iter().find(|d| d.name == second).unwrap_or_else(|| panic!("{listed:?}"));
        assert!(!locked.can_connect);
        assert!(listed.iter().any(|d| d.name == Kind::Postgres.config().database && d.can_connect));
        assert!(!e.app.database_sizes(&id).await.unwrap().contains_key(&second));
        assert!(e.app.connect(&scope::scoped_id(&id, &second)).await.is_err());
        e.app.disconnect(&id).await;
    })
    .await
}
