use std::sync::Arc;

use barsql_core::{MessageLevel, QueryError, ResultSummary, ServerMessage};
use barsql_sql::QueryPlan;

use crate::{ColumnMeta, ResultChunk};

pub const BATCH_ROWS: usize = 5000;
// Per statement. Past it, messages are only counted.
pub const MAX_MESSAGES: usize = 1000;

// Every result set ends with a Result. A plan statement sends just one Result, carrying the plan and no rows.
// Messages can come before their statement's Result, in more than one event, or after it: MySQL's arrive once
// the statement is done.
#[derive(Debug)]
pub enum ScriptEvent {
    Meta { result_index: usize, columns: Arc<[ColumnMeta]> },
    Rows { result_index: usize, chunk: Arc<ResultChunk> },
    // `dropped` counts the messages past MAX_MESSAGES since the last event.
    Messages { result_index: usize, messages: Vec<ServerMessage>, dropped: usize },
    Result(Box<StatementResult>),
}

#[derive(Debug, Clone, Default)]
pub struct StatementResult {
    pub result_index: usize,
    pub statement: String,
    pub summary: Option<ResultSummary>,
    pub plan: Option<QueryPlan>,
    pub error: Option<QueryError>,
}

pub type Sink = async_channel::Sender<ScriptEvent>;

pub(crate) async fn emit(sink: &Sink, event: ScriptEvent) {
    let _ = sink.send(event).await;
}

// One statement's messages as they arrive, kept up to MAX_MESSAGES.
#[derive(Debug, Default)]
pub(crate) struct MessageBuffer {
    pending: Vec<ServerMessage>,
    sent: usize,
    dropped: usize,
}

impl MessageBuffer {
    pub(crate) fn push(&mut self, message: ServerMessage) {
        if self.sent + self.pending.len() < MAX_MESSAGES {
            self.pending.push(message);
        } else {
            self.dropped += 1;
        }
    }

    // For the next statement.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    // What arrived since the last call, as an event for `result_index`.
    pub(crate) fn take(&mut self, result_index: usize) -> Option<ScriptEvent> {
        if self.pending.is_empty() && self.dropped == 0 {
            return None;
        }
        let messages = std::mem::take(&mut self.pending);
        self.sent += messages.len();
        Some(ScriptEvent::Messages { result_index, messages, dropped: std::mem::take(&mut self.dropped) })
    }
}

// BarSQL's own notes on how it ran a statement, sent before its Result.
pub(crate) async fn emit_notes(sink: &Sink, result_index: usize, notes: Vec<String>) {
    if !notes.is_empty() {
        let messages = notes.into_iter().map(|note| ServerMessage::new(MessageLevel::Warning, note)).collect();
        emit(sink, ScriptEvent::Messages { result_index, messages, dropped: 0 }).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statement_keeps_its_first_messages_and_counts_the_rest() {
        let mut buffer = MessageBuffer::default();
        assert!(buffer.take(0).is_none());
        for n in 0..MAX_MESSAGES - 1 {
            buffer.push(ServerMessage::new(MessageLevel::Notice, n.to_string()));
        }
        let Some(ScriptEvent::Messages { messages, dropped: 0, .. }) = buffer.take(3) else { panic!() };
        assert_eq!(messages.len(), MAX_MESSAGES - 1);
        for _ in 0..3 {
            buffer.push(ServerMessage::new(MessageLevel::Warning, "w"));
        }
        let Some(ScriptEvent::Messages { result_index: 3, messages, dropped: 2 }) = buffer.take(3) else { panic!() };
        assert_eq!(messages.len(), 1);
        buffer.reset();
        buffer.push(ServerMessage::new(MessageLevel::Info, "next statement"));
        assert!(matches!(buffer.take(4), Some(ScriptEvent::Messages { dropped: 0, .. })));
    }
}
