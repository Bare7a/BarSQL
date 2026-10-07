#![allow(clippy::result_large_err)]

mod cancel;
pub mod clickhouse;
pub mod display;
mod dump;
mod engine;
mod event;
pub(crate) mod http;
pub mod lite;
pub mod mssql;
pub mod mysql;
pub mod postgres;
pub mod result;
mod script;
pub mod sqlite;
pub mod ssh;
mod temporal;
pub mod tls;
pub mod turso;

pub use barsql_core::QueryError;
pub use cancel::Cancel;
pub use dump::DumpQuery;
pub use engine::{Engine, Session, TableQuery};
pub use event::{BATCH_ROWS, MAX_MESSAGES, ScriptEvent, Sink, StatementResult};
pub use result::{Cell, CellKind, ChunkBuilder, ColumnMeta, ResultChunk, ResultSet};
pub use script::{Buffered, StatementRun};
