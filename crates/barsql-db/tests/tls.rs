#![cfg(feature = "e2e")]

mod common;

use barsql_db::{Cancel, Engine};

// tiberius links aws-lc-rs beside ring. With two rustls providers and none installed, mysql_async's TLS setup can't
// pick one and panics, so connecting has to install ring first.
#[tokio::test]
async fn mysql_connects_over_tls_with_both_rustls_providers_linked() {
    let mut cfg = common::mysql_config("MYSQL", "33306");
    cfg.ssl_mode = "require".into();
    let engine = Engine::connect(&cfg).await.expect("MySQL over TLS");
    let mut session = engine.session().await.unwrap();
    let status = session.buffered("SHOW SESSION STATUS LIKE 'Ssl_cipher'", &Cancel::new()).await.unwrap();
    assert!(status.text(0, 1).is_some_and(|cipher| !cipher.is_empty()), "the connection is encrypted");
}
