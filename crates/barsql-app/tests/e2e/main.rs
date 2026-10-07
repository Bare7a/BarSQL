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

            #[test]
            fn postgres() {
                crate::harness::block_on(crate::harness::run(crate::harness::Kind::Postgres, |$e| async move $body))
            }

            #[test]
            fn mysql() {
                crate::harness::block_on(crate::harness::run(crate::harness::Kind::MySql, |$e| async move $body))
            }

            #[test]
            fn mariadb() {
                crate::harness::block_on(crate::harness::run(crate::harness::Kind::MariaDb, |$e| async move $body))
            }

            #[test]
            fn turso() {
                crate::harness::block_on(crate::harness::run(crate::harness::Kind::Turso, |$e| async move $body))
            }

            #[test]
            fn clickhouse() {
                crate::harness::block_on(crate::harness::run(crate::harness::Kind::ClickHouse, |$e| async move $body))
            }

            #[test]
            fn sqlserver() {
                crate::harness::block_on(crate::harness::run(crate::harness::Kind::SqlServer, |$e| async move $body))
            }
        }
    };
}

// Ends the test early on an engine without the capability, like interactive transactions on Turso.
macro_rules! require {
    ($e:ident, $capability:ident) => {
        if !$e.driver().capabilities().$capability {
            return;
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
mod system_views;
mod transactions;
