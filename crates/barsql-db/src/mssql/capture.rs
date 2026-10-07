// tiberius reports PRINT text, row counts, transaction changes and every error of a batch only as tracing events,
// so a subscriber of BarSQL's own reads them while a tiberius future is polled. The events are told apart by where
// tiberius emits them, which is why the crate is pinned to one version; the e2e suite fails if they move.

use std::cell::RefCell;
use std::fmt;
use std::future::Future;
use std::pin::pin;
use std::sync::LazyLock;

use regex::Regex;
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Dispatch, Event, Metadata, Subscriber};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    // PRINT, and RAISERROR below severity 11.
    Info(String),
    // The batch's first error also comes back from tiberius, with its line, state and class.
    Error { code: u32, message: String },
    Done { kind: DoneKind, status: u16, rows: u64 },
    EnvChange(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoneKind {
    // A statement of the batch.
    Statement,
    // A procedure the batch ran.
    Procedure,
    // A statement inside such a procedure.
    InProcedure,
}

// DONE status bits, MS-TDS 2.2.7.6.
pub(crate) const DONE_ERROR: u16 = 0x2;
pub(crate) const DONE_COUNT: u16 = 0x10;
#[cfg(test)]
pub(crate) const DONE_ATTENTION: u16 = 0x20;
pub(crate) const DONE_SERVER_ERROR: u16 = 0x100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Error,
    Done(DoneKind),
    EnvChange,
    Info,
}

// tiberius 0.13.0 emits these from src/tds/stream/token.rs.
const MODULE: &str = "tiberius::tds::stream::token";

fn role(meta: &Metadata<'_>) -> Option<Role> {
    if !meta.is_event() || meta.module_path() != Some(MODULE) {
        return None;
    }
    match meta.line()? {
        330 => Some(Role::Error),
        354 => Some(Role::Done(DoneKind::Statement)),
        360 => Some(Role::Done(DoneKind::Procedure)),
        366 => Some(Role::Done(DoneKind::InProcedure)),
        398 => Some(Role::EnvChange),
        405 => Some(Role::Info),
        _ => None,
    }
}

thread_local! {
    // The tokens of the captured poll running on this thread.
    static CURRENT: RefCell<Option<Vec<Token>>> = const { RefCell::new(None) };
}

// Created before any tiberius code runs: interest is settled when a callsite is first hit, and a callsite no
// subscriber wanted then stays off for good.
static DISPATCH: LazyLock<Dispatch> = LazyLock::new(|| Dispatch::new(Capture));

pub(crate) fn install() {
    LazyLock::force(&DISPATCH);
}

// Runs `fut`, adding what tiberius reports while it's polled to `tokens`, in order.
pub(crate) async fn captured<F: Future>(tokens: &mut Vec<Token>, fut: F) -> F::Output {
    let mut fut = pin!(fut);
    std::future::poll_fn(|cx| {
        let outer = CURRENT.with(|current| current.borrow_mut().replace(std::mem::take(tokens)));
        let polled = tracing::dispatcher::with_default(&DISPATCH, || fut.as_mut().poll(cx));
        *tokens = CURRENT.with(|current| std::mem::replace(&mut *current.borrow_mut(), outer)).unwrap_or_default();
        polled
    })
    .await
}

struct Capture;

impl Subscriber for Capture {
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> Interest {
        if role(meta).is_some() { Interest::always() } else { Interest::never() }
    }

    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        role(meta).is_some()
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let Some(role) = role(event.metadata()) else { return };
        let mut fields = Fields::default();
        event.record(&mut fields);
        let token = match role {
            Role::Error => Some(Token::Error { code: fields.code, message: fields.message }),
            Role::Done(kind) => parse_done(&fields.message).map(|(status, rows)| Token::Done { kind, status, rows }),
            Role::EnvChange => Some(Token::EnvChange(fields.message)),
            Role::Info => Some(Token::Info(fields.message)),
        };
        CURRENT.with(|current| {
            if let (Some(tokens), Some(token)) = (current.borrow_mut().as_mut(), token) {
                tokens.push(token);
            }
        });
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

#[derive(Default)]
struct Fields {
    message: String,
    code: u32,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "code" {
            self.code = u32::try_from(value).unwrap_or(u32::MAX);
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "code" {
            self.code = u32::try_from(value).unwrap_or(u32::MAX);
        }
    }
}

// `Done with status BitFlags<DoneStatus>(0b10001, More | Count) (5 rows left)`: enumflags2's Debug, then the
// row count, which tiberius words as rows left. The bits are read, not the names.
static DONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Done with status BitFlags<DoneStatus>\(0b([01]+)(?:, [^)]*)?\)(?: \((\d+) rows? left\))?$").unwrap()
});

pub(crate) fn parse_done(text: &str) -> Option<(u16, u64)> {
    let caps = DONE.captures(text)?;
    let status = u16::from_str_radix(&caps[1], 2).ok()?;
    let rows = caps.get(2).map_or(Some(0), |rows| rows.as_str().parse().ok())?;
    Some((status, rows))
}

#[cfg(test)]
mod tests {
    use enumflags2::{BitFlags, bitflags};

    use super::*;

    // tiberius's DoneStatus, to pin enumflags2's Debug output.
    #[bitflags]
    #[repr(u16)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DoneStatus {
        More = 1 << 0,
        Error = 1 << 1,
        Inexact = 1 << 2,
        Count = 1 << 4,
        Attention = 1 << 5,
        RpcInBatch = 1 << 7,
        SrvError = 1 << 8,
    }

    fn done(status: BitFlags<DoneStatus>, rows: u64) -> String {
        match rows {
            0 => format!("Done with status {status:?}"),
            1 => format!("Done with status {status:?} (1 row left)"),
            n => format!("Done with status {status:?} ({n} rows left)"),
        }
    }

    #[test]
    fn done_lines_give_their_bits_and_rows() {
        let counted = DoneStatus::More | DoneStatus::Count;
        assert_eq!(parse_done(&done(counted, 5)), Some((0x11, 5)));
        assert_eq!(parse_done(&done(BitFlags::from(DoneStatus::Count), 1)), Some((DONE_COUNT, 1)));
        assert_eq!(parse_done(&done(BitFlags::empty(), 0)), Some((0, 0)));
        let failed = DoneStatus::Error | DoneStatus::SrvError | DoneStatus::Attention;
        assert_eq!(parse_done(&done(failed, 0)), Some((DONE_ERROR | DONE_SERVER_ERROR | DONE_ATTENTION, 0)));
        assert_eq!(parse_done("Done with status something else"), None);
    }

    #[test]
    fn other_events_are_left_alone() {
        install();
        let mut tokens = Vec::new();
        futures_util::FutureExt::now_or_never(captured(&mut tokens, async { tracing::debug!("not tiberius") }));
        assert!(tokens.is_empty());
    }
}
