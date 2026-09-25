mod bastion;

use std::path::PathBuf;

use barsql_core::SshConfig;
use barsql_core::config::{SSH_AUTH_KEY, SSH_AUTH_PASSWORD};
use barsql_db::ssh::{SshOptions, SshTunnel, parse_private_key};
use bastion::{PASSWORD, USER, known_hosts, start_bastion, start_echo};
use russh::keys::PublicKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn options(port: u16, known_hosts: Option<PathBuf>, password: &str) -> SshOptions {
    SshOptions::from_config(&SshConfig {
        enabled: true,
        host: "127.0.0.1".into(),
        port: i64::from(port),
        username: USER.into(),
        auth: SSH_AUTH_PASSWORD.into(),
        password: password.into(),
        known_hosts: known_hosts.map(|p| p.display().to_string()).unwrap_or_default(),
        ignore_host_key: false,
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test]
async fn the_tunnel_forwards_bytes_to_the_target() {
    let bastion = start_bastion().await;
    let echo = start_echo().await;
    let dir = tempfile::tempdir().unwrap();
    let tunnel =
        SshTunnel::connect(&options(bastion.port, Some(known_hosts(&dir, bastion.port, Some(&bastion.key))), PASSWORD))
            .await
            .expect("tunnel");
    let mut stream = tunnel.open("127.0.0.1", echo).await.expect("channel");
    stream.write_all(b"SELECT 1").await.unwrap();
    let mut buf = [0u8; 8];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"SELECT 1");
}

#[tokio::test]
async fn a_refused_target_names_the_failing_hop() {
    let bastion = start_bastion().await;
    let dir = tempfile::tempdir().unwrap();
    let tunnel =
        SshTunnel::connect(&options(bastion.port, Some(known_hosts(&dir, bastion.port, Some(&bastion.key))), PASSWORD))
            .await
            .expect("tunnel");
    let err = tunnel.open("127.0.0.1", 1).await.err().expect("dial error");
    assert!(err.message.contains("from the bastion"), "{err:?}");
}

#[tokio::test]
async fn public_key_auth_works_and_encrypted_keys_need_their_passphrase() {
    let bastion = start_bastion().await;
    let dir = tempfile::tempdir().unwrap();
    let mut opts = SshOptions::from_config(&SshConfig {
        enabled: true,
        host: "127.0.0.1".into(),
        port: i64::from(bastion.port),
        username: USER.into(),
        auth: SSH_AUTH_KEY.into(),
        key_path: fixture_path("client_ed25519").display().to_string(),
        known_hosts: known_hosts(&dir, bastion.port, Some(&bastion.key)).display().to_string(),
        ..Default::default()
    })
    .unwrap();
    SshTunnel::connect(&opts).await.expect("key auth");

    let encrypted = std::fs::read_to_string(fixture_path("client_ed25519_encrypted")).unwrap();
    let err = parse_private_key(&encrypted, None).expect_err("passphrase required");
    assert!(err.message.contains("passphrase"), "{err:?}");
    assert!(parse_private_key(&encrypted, Some("wrong")).is_err());
    assert!(parse_private_key(&encrypted, Some("s3cret")).is_ok());

    opts.auth = barsql_db::ssh::SshAuth::Key { path: fixture_path("missing_key"), passphrase: None };
    let err = SshTunnel::connect(&opts).await.err().expect("missing key");
    assert!(err.message.contains("cannot read private key"), "{err:?}");
}

#[tokio::test]
async fn an_unknown_bastion_is_refused_with_guidance() {
    let bastion = start_bastion().await;
    let dir = tempfile::tempdir().unwrap();
    let err = SshTunnel::connect(&options(bastion.port, Some(known_hosts(&dir, bastion.port, None)), PASSWORD))
        .await
        .err()
        .expect("unknown host key");
    assert!(err.message.contains("ssh-keyscan"), "{err:?}");
}

#[tokio::test]
async fn a_changed_host_key_is_refused() {
    let bastion = start_bastion().await;
    let dir = tempfile::tempdir().unwrap();
    let other = PublicKey::from_openssh(&std::fs::read_to_string(fixture_path("client_ed25519.pub")).unwrap()).unwrap();
    let err = SshTunnel::connect(&options(bastion.port, Some(known_hosts(&dir, bastion.port, Some(&other))), PASSWORD))
        .await
        .err()
        .expect("mismatch");
    assert!(err.message.contains("does not match known_hosts"), "{err:?}");
}

#[tokio::test]
async fn skipping_the_host_key_check_connects_anyway() {
    let bastion = start_bastion().await;
    let mut opts = options(bastion.port, None, PASSWORD);
    opts.ignore_host_key = true;
    SshTunnel::connect(&opts).await.expect("ignore host key");
}

#[tokio::test]
async fn wrong_credentials_are_reported() {
    let bastion = start_bastion().await;
    let dir = tempfile::tempdir().unwrap();
    let err =
        SshTunnel::connect(&options(bastion.port, Some(known_hosts(&dir, bastion.port, Some(&bastion.key))), "wrong"))
            .await
            .err()
            .expect("authentication must fail");
    assert!(err.message.contains("rejected the credentials"), "{err:?}");
}

#[tokio::test]
async fn an_unreachable_bastion_fails_fast() {
    let mut opts = options(1, None, PASSWORD);
    opts.ignore_host_key = true;
    let started = std::time::Instant::now();
    let err = SshTunnel::connect(&opts).await.err().expect("unreachable");
    assert!(err.message.contains("cannot reach bastion"), "{err:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}
