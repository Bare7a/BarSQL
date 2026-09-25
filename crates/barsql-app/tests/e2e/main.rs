// Needs the stack from `cargo xtask e2e up`. Tests are named <case>::<engine>, so
// `cargo test -p barsql-app --features e2e postgres` runs one engine.
#![cfg(feature = "e2e")]
#![allow(clippy::result_large_err)]

#[path = "../../../barsql-db/tests/bastion/mod.rs"]
mod bastion;
mod harness;
#[path = "../support/mod.rs"]
mod support;

macro_rules! each_engine {
    (async fn $name:ident($e:ident) $body:block) => {
        mod $name {
            #[allow(unused_imports)]
            use super::*;

            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn postgres() {
                crate::harness::run(crate::harness::Kind::Postgres, |$e| async move $body).await
            }

            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn mysql() {
                crate::harness::run(crate::harness::Kind::MySql, |$e| async move $body).await
            }

            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn mariadb() {
                crate::harness::run(crate::harness::Kind::MariaDb, |$e| async move $body).await
            }
        }
    };
}

mod changes;
mod connections;
mod explain;
mod grid;
mod query;
mod roundtrip;
mod schema;
mod ssh;
mod transactions;
