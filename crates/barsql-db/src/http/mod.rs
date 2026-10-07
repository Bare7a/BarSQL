// A small HTTP/1.1 client for the servers BarSQL speaks HTTP to. It dials over TCP or the SSH tunnel, adds TLS,
// streams bodies, and keeps one connection alive between requests. Dropping a response or the future that
// awaits it abandons the request and its connection, which is how a run is cancelled.

#[cfg(test)]
pub(crate) mod fake;
mod framing;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use barsql_core::QueryError;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::ssh::TunnelSlot;
use framing::{Framing, Step};

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
// Bigger heads mean something other than an HTTP server answered.
const MAX_HEAD: usize = 64 * 1024;
const READ_SIZE: usize = 16 * 1024;
// How long an idle connection is kept when the server doesn't say.
const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(2);

pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Origin {
    pub tls: bool,
    pub host: String,
    pub port: u16,
}

impl Origin {
    // The Host header leaves the port out when it's the scheme's default.
    fn host_header(&self) -> String {
        let default = if self.tls { 443 } else { 80 };
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == default { host } else { format!("{host}:{}", self.port) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpError {
    pub message: String,
    // Nothing of a response arrived, so a reused connection had gone stale and the request never ran.
    pub before_response: bool,
}

impl HttpError {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), before_response: false }
    }

    fn before(message: impl Into<String>) -> Self {
        Self { message: message.into(), before_response: true }
    }
}

impl From<HttpError> for QueryError {
    fn from(err: HttpError) -> Self {
        QueryError::message(err.message)
    }
}

pub(crate) struct Request<'a> {
    pub method: &'a str,
    // Path and query.
    pub target: &'a str,
    pub headers: Vec<(&'a str, String)>,
    pub body: &'a [u8],
}

impl<'a> Request<'a> {
    pub(crate) fn get(target: &'a str) -> Self {
        Self { method: "GET", target, headers: Vec::new(), body: &[] }
    }

    pub(crate) fn post(target: &'a str, body: &'a [u8]) -> Self {
        Self { method: "POST", target, headers: Vec::new(), body }
    }

    pub(crate) fn header(mut self, name: &'a str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

struct Conn {
    io: Box<dyn Io>,
    // Read but not yet consumed.
    buf: Vec<u8>,
}

impl Conn {
    // Appends what the next read brings. Zero means the peer closed.
    async fn fill(&mut self) -> std::io::Result<usize> {
        let start = self.buf.len();
        self.buf.resize(start + READ_SIZE, 0);
        let read = self.io.read(&mut self.buf[start..]).await;
        self.buf.truncate(start + *read.as_ref().unwrap_or(&0));
        read
    }
}

struct Idle {
    conn: Conn,
    until: Instant,
}

struct Inner {
    origin: Origin,
    tls: Option<Arc<ClientConfig>>,
    tunnel: Option<Arc<TunnelSlot>>,
    idle: Mutex<Option<Idle>>,
}

#[derive(Clone)]
pub(crate) struct HttpClient(Arc<Inner>);

impl HttpClient {
    // `tls` must be set for an https origin.
    pub(crate) fn new(origin: Origin, tls: Option<Arc<ClientConfig>>, tunnel: Option<Arc<TunnelSlot>>) -> Self {
        Self(Arc::new(Inner { origin, tls, tunnel, idle: Mutex::new(None) }))
    }

    pub(crate) fn origin(&self) -> &Origin {
        &self.0.origin
    }

    fn take_idle(&self) -> Option<Conn> {
        let idle = self.0.idle.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        (Instant::now() < idle.until).then_some(idle.conn)
    }

    fn put_idle(&self, conn: Conn, keep: Duration) {
        *self.0.idle.lock().unwrap_or_else(|e| e.into_inner()) = Some(Idle { conn, until: Instant::now() + keep });
    }

    async fn dial(&self) -> Result<Conn, HttpError> {
        let origin = &self.0.origin;
        let unreachable = |err: &dyn std::fmt::Display| {
            HttpError::new(format!("can't connect to {}:{}: {err}", origin.host, origin.port))
        };
        let stream: Box<dyn Io> = match &self.0.tunnel {
            Some(slot) => Box::new(
                tokio::time::timeout(CONNECT_TIMEOUT, slot.open(&origin.host, origin.port))
                    .await
                    .map_err(|_| unreachable(&"timed out"))?
                    .map_err(|err| HttpError::new(err.message))?,
            ),
            None => {
                let tcp =
                    tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((origin.host.as_str(), origin.port)))
                        .await
                        .map_err(|_| unreachable(&"timed out"))?
                        .map_err(|err| unreachable(&err))?;
                let _ = tcp.set_nodelay(true);
                Box::new(tcp)
            }
        };
        let io: Box<dyn Io> = match (&self.0.tls, origin.tls) {
            (Some(config), true) => {
                let name = ServerName::try_from(origin.host.clone()).map_err(|err| unreachable(&err))?;
                let tls =
                    tokio::time::timeout(CONNECT_TIMEOUT, TlsConnector::from(config.clone()).connect(name, stream))
                        .await
                        .map_err(|_| unreachable(&"TLS handshake timed out"))?
                        .map_err(|err| unreachable(&err))?;
                Box::new(tls)
            }
            (None, true) => return Err(HttpError::new("https needs a TLS configuration")),
            (_, false) => stream,
        };
        Ok(Conn { io, buf: Vec::new() })
    }

    // A reused connection the server closed meanwhile fails before any response arrives, and then the request is
    // sent once more on a new one.
    pub(crate) async fn send(&self, req: &Request<'_>) -> Result<Response, HttpError> {
        if let Some(conn) = self.take_idle() {
            match self.exchange(conn, req).await {
                Err(err) if err.before_response => {}
                other => return other,
            }
        }
        let conn = self.dial().await?;
        self.exchange(conn, req).await
    }

    async fn exchange(&self, mut conn: Conn, req: &Request<'_>) -> Result<Response, HttpError> {
        let mut head = format!("{} {} HTTP/1.1\r\nHost: {}\r\n", req.method, req.target, self.0.origin.host_header());
        for (name, value) in &req.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        if req.method != "GET" || !req.body.is_empty() {
            head.push_str(&format!("Content-Length: {}\r\n", req.body.len()));
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(req.body);
        let lost = |err: std::io::Error| HttpError::before(format!("the connection to the server was lost: {err}"));
        conn.io.write_all(&bytes).await.map_err(lost)?;
        conn.io.flush().await.map_err(lost)?;
        loop {
            let head = read_head(&mut conn).await?;
            // An interim response, like 100 Continue. The real one follows.
            if (100..200).contains(&head.status) && head.status != 101 {
                continue;
            }
            let framing = Framing::for_response(req.method, &head)?;
            return Ok(Response {
                status: head.status,
                headers: head.headers,
                keep: head.keep_alive,
                client: self.clone(),
                conn: Some(conn),
                framing,
                pending: Vec::new(),
            });
        }
    }
}

pub(crate) struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    // How long the connection may be reused after this response, or None when it can't be.
    pub keep_alive: Option<Duration>,
}

impl Head {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

fn header<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

async fn read_head(conn: &mut Conn) -> Result<Head, HttpError> {
    let mut received = !conn.buf.is_empty();
    loop {
        let mut headers = [httparse::EMPTY_HEADER; 96];
        let mut parsed = httparse::Response::new(&mut headers);
        match parsed.parse(&conn.buf) {
            Ok(httparse::Status::Complete(len)) => {
                let status = parsed.code.unwrap_or(0);
                let http11 = parsed.version == Some(1);
                let headers: Vec<(String, String)> = parsed
                    .headers
                    .iter()
                    .map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).trim().to_string()))
                    .collect();
                conn.buf.drain(..len);
                let keep_alive = keep_alive(http11, &headers);
                return Ok(Head { status, headers, keep_alive });
            }
            Ok(httparse::Status::Partial) if conn.buf.len() > MAX_HEAD => {
                return Err(HttpError::new("the server's response header is too large"));
            }
            Ok(httparse::Status::Partial) => {}
            Err(err) => return Err(HttpError::new(format!("the server's response isn't HTTP: {err}"))),
        }
        let read = conn.fill().await.map_err(|err| {
            let message = format!("the connection to the server was lost: {err}");
            if received { HttpError::new(message) } else { HttpError::before(message) }
        })?;
        if read == 0 {
            let message = "the server closed the connection without a response";
            return Err(if received { HttpError::new(message) } else { HttpError::before(message) });
        }
        received = true;
    }
}

// HTTP/1.1 keeps connections open unless told otherwise. `Keep-Alive: timeout=N` gives the server's idle limit;
// a second less leaves room for the request to arrive in time.
fn keep_alive(http11: bool, headers: &[(String, String)]) -> Option<Duration> {
    let connection = header(headers, "connection").unwrap_or_default().to_ascii_lowercase();
    if !http11 || connection.split(',').any(|token| token.trim() == "close") {
        return None;
    }
    let timeout = header(headers, "keep-alive").and_then(|value| {
        value.split(',').find_map(|part| part.trim().strip_prefix("timeout=")?.trim().parse::<u64>().ok())
    });
    match timeout {
        Some(0 | 1) => None,
        Some(seconds) => Some(Duration::from_secs(seconds - 1)),
        None => Some(DEFAULT_KEEP_ALIVE),
    }
}

pub(crate) struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    keep: Option<Duration>,
    client: HttpClient,
    // Taken once the body ends, back to the client if it can be reused.
    conn: Option<Conn>,
    framing: Framing,
    // Body bytes read past the last line handed out.
    pending: Vec<u8>,
}

impl Response {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    pub(crate) fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    // The next piece of the body, or None at its end.
    pub(crate) async fn chunk(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        if !self.pending.is_empty() {
            return Ok(Some(std::mem::take(&mut self.pending)));
        }
        let Some(conn) = self.conn.as_mut() else { return Ok(None) };
        loop {
            match self.framing.step(&mut conn.buf)? {
                Step::Data(data) => return Ok(Some(data)),
                Step::NeedMore => {
                    let read =
                        conn.fill().await.map_err(|err| HttpError::new(format!("reading the response: {err}")))?;
                    if read == 0 {
                        if self.framing.ends_at_close() {
                            self.conn = None;
                            return Ok(None);
                        }
                        return Err(HttpError::new("the server closed the connection mid-response"));
                    }
                }
                Step::End => {
                    let conn = self.conn.take().expect("open");
                    // Leftover bytes would be the start of something the server shouldn't have sent.
                    if let (Some(keep), true) = (self.keep, conn.buf.is_empty()) {
                        self.client.put_idle(conn, keep);
                    }
                    return Ok(None);
                }
            }
        }
    }

    // The whole body. More than `limit` bytes fails rather than filling memory.
    pub(crate) async fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>, HttpError> {
        let mut body = Vec::new();
        while let Some(chunk) = self.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() > limit {
                return Err(HttpError::new(format!("the response is larger than {} MiB", limit / (1024 * 1024))));
            }
        }
        Ok(body)
    }

    // The next line of the body without its `\n`, or None at the end. A last line without one still counts.
    pub(crate) async fn next_line(&mut self, limit: usize) -> Result<Option<Vec<u8>>, HttpError> {
        let mut line: Vec<u8> = Vec::new();
        loop {
            if let Some(at) = self.pending.iter().position(|&b| b == b'\n') {
                line.extend_from_slice(&self.pending[..at]);
                self.pending.drain(..=at);
                return Ok(Some(line));
            }
            line.append(&mut self.pending);
            if line.len() > limit {
                return Err(HttpError::new(format!(
                    "a line of the response is longer than {} MiB",
                    limit / (1024 * 1024)
                )));
            }
            match self.chunk().await? {
                Some(chunk) => self.pending = chunk,
                None => return Ok((!line.is_empty()).then_some(line)),
            }
        }
    }
}
