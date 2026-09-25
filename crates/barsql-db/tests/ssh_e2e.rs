#![cfg(feature = "e2e")]

mod bastion;
mod common;

use barsql_core::SshConfig;
use barsql_core::config::SSH_AUTH_PASSWORD;
use barsql_db::{Cancel, Engine};
use bastion::{PASSWORD, USER, known_hosts, start_bastion};
use common::{mysql_config, postgres_config};

fn tunnelled(
    mut cfg: barsql_core::ConnectionConfig,
    port: u16,
    known_hosts: &std::path::Path,
) -> barsql_core::ConnectionConfig {
    cfg.ssh = SshConfig {
        enabled: true,
        host: "127.0.0.1".into(),
        port: i64::from(port),
        username: USER.into(),
        auth: SSH_AUTH_PASSWORD.into(),
        password: PASSWORD.into(),
        known_hosts: known_hosts.display().to_string(),
        ..Default::default()
    };
    cfg
}

#[tokio::test]
async fn postgres_runs_over_an_ssh_channel_without_a_local_port() {
    let bastion = start_bastion().await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = tunnelled(postgres_config(), bastion.port, &known_hosts(&dir, bastion.port, Some(&bastion.key)));
    let engine = Engine::connect(&cfg).await.expect("postgres over ssh");
    let user = engine.session().await.unwrap().buffered("SELECT current_user", &Cancel::new()).await.unwrap();
    assert_eq!(user.text(0, 0), Some(cfg.username.as_str()));
    assert!(!engine.list_schemas().await.unwrap().is_empty());
}

#[tokio::test]
async fn mysql_runs_through_a_loopback_forward() {
    let bastion = start_bastion().await;
    let dir = tempfile::tempdir().unwrap();
    let cfg =
        tunnelled(mysql_config("MYSQL", "33306"), bastion.port, &known_hosts(&dir, bastion.port, Some(&bastion.key)));
    let engine = Engine::connect(&cfg).await.expect("mysql over ssh");
    let two = engine.session().await.unwrap().buffered("SELECT 1 + 1 AS two", &Cancel::new()).await.unwrap();
    assert_eq!(two.text(0, 0), Some("2"));
}
