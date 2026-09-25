// SQL language service for the editor. All offsets are byte offsets into the text passed in, and the
// editor maps them to its own positions.
mod cache;
pub mod catalog;
pub mod completion;
pub mod context;
pub mod diagnostics;
pub mod hover;
pub mod labels;
pub mod query;
pub mod quoting;
pub mod statements;
pub mod suggestions;
pub mod text;
pub mod tokens;
pub mod transaction;

pub use catalog::{Catalog, ColumnMap, OrderedMap, TableBinding};
pub use completion::{bindings_needing_columns, build_completion_items, completion_replace_range};
pub use context::{CursorSlot, SqlCursor, StatementShape, analyze_cursor};
pub use diagnostics::{SqlDiagnostic, collect_schema_diagnostics};
pub use hover::{ColumnLookup, HoverQuery, analyze_hover, column_hover_lines, table_columns_markdown};
pub use labels::SqlLabels;
pub use query::{ParsedQuery, QueryTableRef, parse_query, resolve_dot_completion, resolve_qualifier_to_table};
pub use statements::{
    EditorStatement, current_statement_range, current_statement_start, parse_statements, statement_at_offset,
    statement_at_run_line,
};
pub use suggestions::{CompletionContext, CompletionItem, ItemKind};
pub use tokens::{Token, TokenKind, tokenize};
pub use transaction::{TxnControl, detect_transaction_control};
