// Hrana over HTTP, the protocol libSQL servers and Turso speak. v3 streams a batch's rows as NDJSON from
// `/v3/cursor`; v2 only has `/v2/pipeline`, which answers with one JSON document.

use base64::Engine as _;
use base64::alphabet;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD};
use serde::{Deserialize, Serialize};

// sqld sends blobs without padding, and other servers may pad them.
const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

use crate::lite::{LiteRef, LiteValue};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum HValue {
    Null,
    // A string, since JSON numbers can't hold every 64-bit integer.
    Integer { value: String },
    Float { value: f64 },
    Text { value: String },
    Blob { base64: String },
}

impl HValue {
    // NaN and the infinities have no JSON form, so they bind as NULL like SQLite stores them.
    pub(crate) fn from_lite(value: &LiteValue) -> Self {
        match value {
            LiteValue::Null => Self::Null,
            LiteValue::Integer(i) => Self::Integer { value: i.to_string() },
            LiteValue::Real(f) if !f.is_finite() => Self::Null,
            LiteValue::Real(f) => Self::Float { value: *f },
            LiteValue::Text(s) => Self::Text { value: s.clone() },
            LiteValue::Blob(b) => Self::Blob { base64: STANDARD.encode(b) },
        }
    }

    // Integers that don't parse are shown as the server sent them.
    pub(crate) fn to_lite(&self) -> LiteValue {
        match self {
            Self::Null => LiteValue::Null,
            Self::Integer { value } => {
                value.parse().map_or_else(|_| LiteValue::Text(value.clone()), LiteValue::Integer)
            }
            Self::Float { value } => LiteValue::Real(*value),
            Self::Text { value } => LiteValue::Text(value.clone()),
            Self::Blob { base64 } => match LENIENT.decode(base64) {
                Ok(bytes) => LiteValue::Blob(bytes),
                Err(_) => LiteValue::Text(base64.clone()),
            },
        }
    }

    // Borrowed for display, with integers parsed on the way.
    pub(crate) fn with_ref<T>(&self, f: impl FnOnce(LiteRef<'_>) -> T) -> T {
        match self {
            Self::Null => f(LiteRef::Null),
            Self::Integer { value } => match value.parse() {
                Ok(i) => f(LiteRef::Integer(i)),
                Err(_) => f(LiteRef::Text(value.as_bytes())),
            },
            Self::Float { value } => f(LiteRef::Real(*value)),
            Self::Text { value } => f(LiteRef::Text(value.as_bytes())),
            Self::Blob { base64 } => match LENIENT.decode(base64) {
                Ok(bytes) => f(LiteRef::Blob(&bytes)),
                Err(_) => f(LiteRef::Text(base64.as_bytes())),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Stmt {
    pub sql: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<HValue>,
    pub want_rows: bool,
}

impl Stmt {
    pub(crate) fn new(sql: &str, args: &[LiteValue]) -> Self {
        Self { sql: sql.to_string(), args: args.iter().map(HValue::from_lite).collect(), want_rows: true }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Col {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub decltype: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct HError {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
}

impl HError {
    // A stream the server dropped. Nothing in the request ran. sqld says so in plain text, without a code.
    pub(crate) fn lost_stream(&self) -> bool {
        let message = self.message.to_ascii_lowercase();
        matches!(self.code.as_deref(), Some("STREAM_EXPIRED" | "STREAM_NOT_FOUND"))
            || ["invalid baton", "stream has expired", "stream not found", "baton was not found"]
                .iter()
                .any(|lost| message.contains(lost))
    }

    // SQLite's own message without what sqld wraps around it, and where in the statement it points.
    pub(crate) fn cleaned(&self) -> (String, Option<Loc>) {
        let mut message = self.message.trim();
        if let Some(inner) = message.strip_prefix("Internal Error: `").and_then(|m| m.strip_suffix('`')) {
            message = inner;
        }
        message = message.strip_prefix("SQLite error: ").unwrap_or(message);
        if let Some(input) = message.strip_prefix("SQL input error: ")
            && let Some((text, offset)) = input.rsplit_once(" (at offset ")
            && let Ok(offset) = offset.trim_end_matches(')').parse()
        {
            return (text.to_string(), Some(Loc::Offset(offset)));
        }
        // "syntax error around L1:6: `SELEC`" points just past the token.
        let loc = message.split_once(" around L").and_then(|(_, at)| {
            let (place, token) = at.split_once(": `")?;
            let (line, column) = place.split_once(':')?;
            let token = token.strip_suffix('`').unwrap_or(token);
            Some(Loc::LineColumn {
                line: line.parse().ok()?,
                column: column.parse().ok()?,
                token: token.chars().count(),
            })
        });
        (message.to_string(), loc)
    }
}

// Where an error points: a byte offset, or a line and column just past `token` characters, both 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Loc {
    Offset(usize),
    LineColumn { line: usize, column: usize, token: usize },
}

impl Loc {
    // A 1-based character position in `stmt`, as the editor marks errors. Zero when it doesn't fit.
    pub(crate) fn position(self, stmt: &str) -> u32 {
        let at = match self {
            Loc::Offset(offset) => stmt.get(..offset).map(|before| before.chars().count()),
            Loc::LineColumn { line, column, token } => {
                let lines: Vec<&str> = stmt.split_inclusive('\n').collect();
                (line >= 1 && line <= lines.len() && column > token)
                    .then(|| lines[..line - 1].iter().map(|l| l.chars().count()).sum::<usize>() + column - 1 - token)
            }
        };
        at.and_then(|at| u32::try_from(at + 1).ok()).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct StmtResult {
    #[serde(default)]
    pub cols: Vec<Col>,
    #[serde(default)]
    pub rows: Vec<Vec<HValue>>,
    #[serde(default)]
    pub affected_row_count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PipelineRequest {
    Execute { stmt: Stmt },
    GetAutocommit,
    Close,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PipelineBody {
    pub baton: Option<String>,
    pub requests: Vec<PipelineRequest>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PipelineResponse {
    #[serde(default)]
    pub baton: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub results: Vec<PipelineResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PipelineResult {
    Ok { response: PipelineOk },
    Error { error: HError },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PipelineOk {
    Execute {
        result: StmtResult,
    },
    GetAutocommit {
        is_autocommit: bool,
    },
    Close,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CursorStep {
    pub stmt: Stmt,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CursorBatch {
    pub steps: Vec<CursorStep>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CursorBody {
    pub baton: Option<String>,
    pub batch: CursorBatch,
}

// The first line of a cursor response.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CursorHead {
    #[serde(default)]
    pub baton: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum CursorEntry {
    // BarSQL sends one step per batch, so the step numbers aren't read.
    StepBegin {
        #[serde(default)]
        cols: Vec<Col>,
    },
    StepEnd {
        #[serde(default)]
        affected_row_count: u64,
    },
    StepError {
        error: HError,
    },
    Row {
        row: Vec<HValue>,
    },
    Error {
        error: HError,
    },
    #[serde(other)]
    Other,
}

// A whole-request failure. sqld answers with `{"message", "code"}`; some proxies with `{"error": "..."}` or text.
pub(crate) fn request_error(status: u16, body: &[u8]) -> HError {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        error: Option<serde_json::Value>,
        #[serde(default)]
        code: Option<String>,
    }
    let text = String::from_utf8_lossy(body).trim().to_string();
    let parsed = serde_json::from_slice::<Body>(body).ok();
    let message = parsed.as_ref().and_then(|b| {
        b.message.clone().or_else(|| match &b.error {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Object(o)) => o.get("message").and_then(|m| m.as_str()).map(str::to_string),
            _ => None,
        })
    });
    let message = match (message, status) {
        (Some(message), _) => message,
        (None, 401 | 403) => "the server refused the auth token".to_string(),
        (None, _) if text.is_empty() => format!("the server answered HTTP {status}"),
        (None, _) => format!("the server answered HTTP {status}: {text}"),
    };
    HError { message, code: parsed.and_then(|b| b.code) }
}

// Where the HTTP requests go. libsql:// and wss:// are https, ws:// is http, and `?tls=0` turns TLS off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoint {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    // Without a trailing slash, so "" for the root.
    pub path: String,
}

pub(crate) fn endpoint(raw: &str) -> Result<Endpoint, String> {
    let invalid = |why: &str| format!("database URL is invalid: {why}");
    let parsed = url::Url::parse(raw.trim()).map_err(|err| invalid(&err.to_string()))?;
    let plain = parsed.query_pairs().any(|(k, v)| k == "tls" && v == "0");
    let tls = match parsed.scheme() {
        "libsql" | "wss" => !plain,
        "https" => true,
        "http" | "ws" => false,
        other => return Err(invalid(&format!("unknown scheme {other}"))),
    };
    let host = parsed.host_str().filter(|h| !h.is_empty()).ok_or_else(|| invalid("no host"))?;
    // The url crate keeps IPv6 hosts bracketed.
    let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
    let port = parsed.port().unwrap_or(if tls { 443 } else { 80 });
    Ok(Endpoint { tls, host, port, path: parsed.path().trim_end_matches('/').to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_map_to_http_endpoints() {
        let at = |raw: &str| endpoint(raw).map(|e| (e.tls, e.host, e.port, e.path));
        assert_eq!(at("libsql://db-org.turso.io"), Ok((true, "db-org.turso.io".into(), 443, String::new())));
        assert_eq!(at("libsql://localhost:8080?tls=0"), Ok((false, "localhost".into(), 8080, String::new())));
        assert_eq!(at("http://127.0.0.1:38080/"), Ok((false, "127.0.0.1".into(), 38080, String::new())));
        assert_eq!(at("wss://h/v2/db"), Ok((true, "h".into(), 443, "/v2/db".into())));
        assert_eq!(at("ws://[::1]:9000"), Ok((false, "::1".into(), 9000, String::new())));
        assert!(at("ftp://h").unwrap_err().contains("unknown scheme ftp"));
        assert!(at("not a url").is_err());
    }

    #[test]
    fn values_round_trip_through_json() {
        let values = [
            LiteValue::Null,
            LiteValue::Integer(i64::MAX),
            LiteValue::Real(1.5),
            LiteValue::Text("é\n".into()),
            LiteValue::Blob(vec![0, 255]),
        ];
        let json = serde_json::to_string(&values.iter().map(HValue::from_lite).collect::<Vec<_>>()).unwrap();
        assert_eq!(
            json,
            r#"[{"type":"null"},{"type":"integer","value":"9223372036854775807"},{"type":"float","value":1.5},{"type":"text","value":"é\n"},{"type":"blob","base64":"AP8="}]"#
        );
        let back: Vec<HValue> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.iter().map(HValue::to_lite).collect::<Vec<_>>(), values);
        let unpadded = HValue::Blob { base64: "AP8".into() };
        assert_eq!(unpadded.to_lite(), LiteValue::Blob(vec![0, 255]), "sqld leaves the padding off");
        assert_eq!(HValue::from_lite(&LiteValue::Real(f64::NAN)), HValue::Null);
    }

    #[test]
    fn cursor_entries_parse() {
        let begin: CursorEntry =
            serde_json::from_str(r#"{"type":"step_begin","step":0,"cols":[{"name":"a","decltype":"INTEGER"}]}"#)
                .unwrap();
        assert!(matches!(begin, CursorEntry::StepBegin { ref cols } if cols[0].name.as_deref() == Some("a")));
        let err: CursorEntry = serde_json::from_str(
            r#"{"type":"step_error","step":1,"error":{"message":"no such table: t","code":"SQLITE_ERROR"}}"#,
        )
        .unwrap();
        assert!(matches!(err, CursorEntry::StepError { ref error } if error.message == "no such table: t"));
        let unknown: CursorEntry = serde_json::from_str(r#"{"type":"replication_index","value":"7"}"#).unwrap();
        assert!(matches!(unknown, CursorEntry::Other));
    }

    #[test]
    fn request_errors_read_every_shape() {
        let expired =
            request_error(400, br#"{"message":"The stream has expired due to inactivity","code":"STREAM_EXPIRED"}"#);
        assert!(expired.lost_stream());
        assert!(request_error(400, b"Received an invalid baton").lost_stream(), "sqld's plain-text answer");
        assert!(!request_error(400, br#"{"message":"no such table: t"}"#).lost_stream());
        assert_eq!(request_error(400, br#"{"error":"bad baton"}"#).message, "bad baton");
        let parse = request_error(
            500,
            br#"{"error":"Internal Error: `SQL string could not be parsed: syntax error around L2:6: `SELEC``"}"#,
        );
        let (message, loc) = parse.cleaned();
        assert_eq!(message, "SQL string could not be parsed: syntax error around L2:6: `SELEC`");
        assert_eq!(loc.map(|l| l.position("SELECT 1;\nSELEC 1")), Some(11), "the S of SELEC on the second line");
        let input = HError { message: "SQL input error: no such column: nope (at offset 27)".into(), code: None };
        let (message, loc) = input.cleaned();
        assert_eq!(
            (message.as_str(), loc.map(|l| l.position("SELECT 'héllo 🌍' AS v, nope"))),
            ("no such column: nope", Some(24))
        );
        assert_eq!(
            HError { message: "SQLite error: no such table: t".into(), code: None }.cleaned().0,
            "no such table: t"
        );
        assert_eq!(request_error(401, b"").message, "the server refused the auth token");
        assert_eq!(request_error(502, b"Bad Gateway").message, "the server answered HTTP 502: Bad Gateway");
    }
}
