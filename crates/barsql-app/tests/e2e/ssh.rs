use std::path::Path;

use barsql_core::SshConfig;
use barsql_core::config::SSH_AUTH_PASSWORD;
use tempfile::TempDir;

use crate::bastion::{Bastion, PASSWORD, USER, known_hosts, start_bastion};
use crate::harness::{E2e, Kind, run, run_error, unique_table};

fn ssh_config(bastion: &Bastion, known_hosts: &Path) -> SshConfig {
    SshConfig {
        enabled: true,
        host: "127.0.0.1".into(),
        port: i64::from(bastion.port),
        username: USER.into(),
        auth: SSH_AUTH_PASSWORD.into(),
        password: PASSWORD.into(),
        known_hosts: known_hosts.display().to_string(),
        ..Default::default()
    }
}

async fn tunnelled(e: &E2e, ssh: SshConfig) -> String {
    let mut cfg = e.kind.config();
    cfg.name = format!("e2e-ssh-{}", e.kind.name());
    cfg.ssh = ssh;
    e.app.save_connection(cfg).await.unwrap().id
}

async fn trusted_tunnel(e: &E2e, dir: &TempDir) -> (Bastion, String) {
    let bastion = start_bastion().await;
    let hosts = known_hosts(dir, bastion.port, Some(&bastion.key));
    let id = tunnelled(e, ssh_config(&bastion, &hosts)).await;
    (bastion, id)
}

each_engine!(async fn ssh_tunnel_queries_each_engine(e) {
    let dir = tempfile::tempdir().unwrap();
    let (_bastion, id) = trusted_tunnel(&e, &dir).await;
    e.app.connect(&id).await.expect("connect through the tunnel");
    let res = e.app.execute_query(&id, "SELECT 1 AS one").await.unwrap();
    assert!(res.rows.len() == 1 && res.rows[0].len() == 1, "{:?}", res.rows);
    e.app.load_schema_data(&id).await.expect("the schema explorer works through the tunnel");
    assert!(e.app.connection_status(&id).await.unwrap().connected);
    e.app.disconnect(&id).await;
});

// The tunnel has to carry a whole pool, not just one round trip.
each_engine!(async fn ssh_tunnel_survives_writes_and_reconnect(e) {
    let dir = tempfile::tempdir().unwrap();
    let (_bastion, id) = trusted_tunnel(&e, &dir).await;
    e.app.connect(&id).await.unwrap();
    let table = unique_table("ssh_tunnel");
    e.app.execute_query(&id, &e.auto_pk_table(&table)).await.unwrap();
    e.defer(format!("DROP TABLE IF EXISTS {}", e.qualified(&table)));
    e.app.execute_query(&id, &format!("INSERT INTO {table} (name) VALUES ('through-the-tunnel')")).await.unwrap();
    let res = e.app.execute_query(&id, &format!("SELECT name FROM {table}")).await.unwrap();
    assert_eq!(res.rows.len(), 1);
    e.app.disconnect(&id).await;
    e.app.connect(&id).await.expect("reconnect through a new tunnel");
    e.app.execute_query(&id, &format!("SELECT name FROM {table}")).await.expect("query after reconnect");
    e.app.disconnect(&id).await;
});

each_engine!(async fn ssh_tunnel_recovers_after_the_bastion_drops_it(e) {
    let dir = tempfile::tempdir().unwrap();
    let (bastion, id) = trusted_tunnel(&e, &dir).await;
    e.app.connect(&id).await.unwrap();
    e.app.execute_query(&id, "SELECT 1").await.unwrap();
    bastion.drop_sessions();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let res = e.app.execute_query(&id, "SELECT 2 AS two").await;
    assert!(res.is_ok(), "the next query rebuilds the tunnel: {res:?}");
    let run = run_error(&e.run_on(&id, "tab-tunnel", "SELECT 3 AS three").await);
    assert!(run.is_none(), "a tab session opened after the drop works too: {run:?}");
    e.app.disconnect(&id).await;
});

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_tunnel_rejects_unknown_bastion_key() {
    run(Kind::Postgres, |e| async move {
        let dir = tempfile::tempdir().unwrap();
        let bastion = start_bastion().await;
        let empty = known_hosts(&dir, bastion.port, None);
        let id = tunnelled(&e, ssh_config(&bastion, &empty)).await;
        let err = e.app.connect(&id).await.expect_err("an unknown bastion key must be rejected");
        assert!(err.message.contains("ssh-keyscan"), "the error should say how to fix it: {err:?}");
        assert!(!e.app.is_connected(&id));
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_tunnel_bad_credentials_do_not_connect() {
    run(Kind::Postgres, |e| async move {
        let dir = tempfile::tempdir().unwrap();
        let bastion = start_bastion().await;
        let hosts = known_hosts(&dir, bastion.port, Some(&bastion.key));
        let id = tunnelled(&e, SshConfig { password: "wrong".into(), ..ssh_config(&bastion, &hosts) }).await;
        assert!(e.app.connect(&id).await.is_err(), "bad bastion credentials must fail the connection");
        assert!(!e.app.is_connected(&id), "a failed tunnel must not leave the connection registered");
    })
    .await
}
