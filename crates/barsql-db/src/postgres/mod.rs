mod schema;
mod session;
mod types;
mod values;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use barsql_core::{ConnectionConfig, MessageLevel, QueryError, ServerMessage};
use jiff::tz::TimeZone;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio_postgres::config::SslMode;
use tokio_postgres::error::{DbError, ErrorPosition, Severity};
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::{AsyncMessage, CancelToken, Client, Config, NoTls};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::event::{MessageBuffer, ScriptEvent};
use crate::ssh::{SshOptions, TunnelSlot};
use crate::tls::TlsMode;

pub use session::PgSession;
pub(crate) use session::pg_literal;
pub use types::type_name;
pub use values::Decoder;
pub(crate) use values::MAX_SAFE_INTEGER;

pub(crate) const MAX_CONNECTIONS: usize = 10;
const MAX_IDLE: usize = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_SETUP: &str = "SET DateStyle = 'ISO'; SET extra_float_digits = 3; SET bytea_output = 'hex'";

#[derive(Debug, Clone)]
pub struct PgConnectOptions {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub ssl_mode: String,
    pub schema: String,
    pub ssh: Option<SshOptions>,
    pub read_only: bool,
    pub local: TimeZone,
}

impl PgConnectOptions {
    pub fn from_config(cfg: &ConnectionConfig) -> Result<Self, QueryError> {
        Ok(Self {
            host: cfg.host.clone(),
            port: port_or(cfg.port, 5432)?,
            user: cfg.username.clone(),
            password: cfg.password.clone(),
            database: cfg.database.clone(),
            ssl_mode: cfg.ssl_mode.clone(),
            schema: if cfg.schema.is_empty() { "public".into() } else { cfg.schema.clone() },
            ssh: if cfg.ssh.enabled { Some(SshOptions::from_config(&cfg.ssh)?) } else { None },
            read_only: cfg.read_only,
            local: TimeZone::system(),
        })
    }
}

pub(crate) fn pg_config(options: &PgConnectOptions, tls: TlsMode) -> Config {
    let mut config = Config::new();
    config
        .host(&options.host)
        .port(options.port)
        .user(&options.user)
        .dbname(&options.database)
        .connect_timeout(CONNECT_TIMEOUT)
        .ssl_mode(match tls {
            TlsMode::Disable => SslMode::Disable,
            TlsMode::Prefer => SslMode::Prefer,
            _ => SslMode::Require,
        });
    // Omit an empty password entirely, like libpq.
    if !options.password.is_empty() {
        config.password(&options.password);
    }
    config
}

pub(crate) fn port_or(port: i64, default: u16) -> Result<u16, QueryError> {
    if port == 0 {
        return Ok(default);
    }
    u16::try_from(port).map_err(|_| QueryError::message(format!("invalid port {port}")))
}

pub struct PgEngine {
    pub(crate) options: PgConnectOptions,
    tls: TlsMode,
    tunnel: Option<Arc<TunnelSlot>>,
    idle: Mutex<Vec<PgConn>>,
    permits: Arc<Semaphore>,
}

pub(crate) struct PgConn {
    pub(crate) client: Client,
    pub(crate) cancel: CancelToken,
    pub(crate) notices: Arc<Notices>,
}

// The connection's notices, collected by its task as they arrive. A notice comes in before the CommandComplete
// of the statement that raised it, so it's here by the time the statement ends.
#[derive(Default)]
pub(crate) struct Notices {
    buffer: Mutex<MessageBuffer>,
    arrived: Notify,
}

impl Notices {
    fn push(&self, notice: &DbError) {
        self.lock().push(notice_message(notice));
        self.arrived.notify_one();
    }

    pub(crate) fn reset(&self) {
        self.lock().reset();
    }

    pub(crate) fn take(&self, result_index: usize) -> Option<ScriptEvent> {
        self.lock().take(result_index)
    }

    pub(crate) async fn arrived(&self) {
        self.arrived.notified().await;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MessageBuffer> {
        self.buffer.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn notice_message(notice: &DbError) -> ServerMessage {
    let level = match notice.parsed_severity() {
        Some(Severity::Warning) => MessageLevel::Warning,
        Some(Severity::Notice) => MessageLevel::Notice,
        // INFO, LOG and DEBUG. The rest are errors, which never come as notices.
        _ => MessageLevel::Info,
    };
    // 00000 is what RAISE gives without an ERRCODE: no code at all.
    let code = notice.code().code();
    ServerMessage {
        level,
        code: if code == "00000" { String::new() } else { code.to_string() },
        text: notice.message().to_string(),
        detail: notice.detail().unwrap_or_default().to_string(),
        hint: notice.hint().unwrap_or_default().to_string(),
    }
}

impl PgEngine {
    pub async fn connect(options: PgConnectOptions) -> Result<Arc<Self>, QueryError> {
        let tls = TlsMode::postgres(&options.ssl_mode).map_err(QueryError::message)?;
        let tunnel = options.ssh.clone().map(TunnelSlot::new);
        let engine = Arc::new(Self {
            options,
            tls,
            tunnel,
            idle: Mutex::new(Vec::new()),
            permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        });
        let first = engine.open_conn().await?;
        engine.lock_idle().push(first);
        Ok(engine)
    }

    pub fn read_only(&self) -> bool {
        self.options.read_only
    }

    pub fn default_schema(&self) -> &str {
        &self.options.schema
    }

    pub(crate) fn schema_or<'a>(&'a self, schema: &'a str) -> &'a str {
        if schema.is_empty() { &self.options.schema } else { schema }
    }

    pub(crate) async fn lease(self: &Arc<Self>) -> Result<PgLease, QueryError> {
        let permit =
            self.permits.clone().acquire_owned().await.map_err(|_| QueryError::message("connection is closed"))?;
        let pooled = {
            let mut idle = self.lock_idle();
            idle.retain(|c| !c.client.is_closed());
            idle.pop()
        };
        let conn = match pooled {
            Some(conn) => conn,
            None => self.open_conn().await?,
        };
        Ok(PgLease { engine: self.clone(), conn: Some(conn), _permit: permit, reusable: true })
    }

    pub async fn session(self: &Arc<Self>) -> Result<PgSession, QueryError> {
        let mut lease = self.lease().await?;
        lease.reusable = false;
        PgSession::new(lease).await
    }

    // Goes back to the pool afterwards, so only for SQL the app generates, like table pages.
    pub async fn pooled_session(self: &Arc<Self>) -> Result<PgSession, QueryError> {
        PgSession::new(self.lease().await?).await
    }

    pub fn close(&self) {
        self.permits.close();
        self.lock_idle().clear();
    }

    async fn open_conn(&self) -> Result<PgConn, QueryError> {
        let config = pg_config(&self.options, self.tls);
        let open = async {
            match (&self.tunnel, self.tls.client_config()) {
                (None, None) => spawn_connection(config.connect(NoTls).await),
                (None, Some(tls)) => spawn_connection(config.connect(MakeRustlsConnect::new(tls)).await),
                (Some(slot), tls) => {
                    let stream = slot.open(&self.options.host, self.options.port).await?;
                    match tls {
                        None => spawn_connection(config.connect_raw(stream, NoTls).await),
                        Some(tls) => {
                            let connector =
                                MakeTlsConnect::<russh::ChannelStream<russh::client::Msg>>::make_tls_connect(
                                    &mut MakeRustlsConnect::new(tls),
                                    &self.options.host,
                                )
                                .map_err(|err| QueryError::message(err.to_string()))?;
                            spawn_connection(config.connect_raw(stream, connector).await)
                        }
                    }
                }
            }
        };
        let (client, notices) = tokio::time::timeout(CONNECT_TIMEOUT, open)
            .await
            .map_err(|_| QueryError::message("failed to connect: timeout"))??;
        client.batch_execute(SESSION_SETUP).await.map_err(|err| pg_error(&err))?;
        let cancel = client.cancel_token();
        Ok(PgConn { client, cancel, notices })
    }

    // Runs on the caller's runtime. If the cancel can't reach the server, the statement keeps running.
    pub(crate) fn server_cancel(self: &Arc<Self>, token: CancelToken) -> impl Fn() + Send + Sync + 'static {
        let engine = self.clone();
        let runtime = tokio::runtime::Handle::try_current().ok();
        move || {
            let (Some(runtime), engine, token) = (runtime.clone(), engine.clone(), token.clone()) else {
                return;
            };
            runtime.spawn(async move {
                let _ = engine.send_cancel(token).await;
            });
        }
    }

    async fn send_cancel(&self, token: CancelToken) -> Result<(), QueryError> {
        match (&self.tunnel, self.tls.client_config()) {
            (None, None) => token.cancel_query(NoTls).await,
            (None, Some(tls)) => token.cancel_query(MakeRustlsConnect::new(tls)).await,
            (Some(slot), tls) => {
                let stream = slot.open(&self.options.host, self.options.port).await?;
                match tls {
                    None => token.cancel_query_raw(stream, NoTls).await,
                    Some(tls) => {
                        let connector = MakeTlsConnect::<russh::ChannelStream<russh::client::Msg>>::make_tls_connect(
                            &mut MakeRustlsConnect::new(tls),
                            &self.options.host,
                        )
                        .map_err(|err| QueryError::message(err.to_string()))?;
                        token.cancel_query_raw(stream, connector).await
                    }
                }
            }
        }
        .map_err(|err| pg_error(&err))
    }

    fn lock_idle(&self) -> std::sync::MutexGuard<'_, Vec<PgConn>> {
        self.idle.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn spawn_connection<S, T>(
    result: Result<(Client, tokio_postgres::Connection<S, T>), tokio_postgres::Error>,
) -> Result<(Client, Arc<Notices>), QueryError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (client, mut connection) = result.map_err(|err| pg_error(&err))?;
    let notices = Arc::new(Notices::default());
    let collected = notices.clone();
    // Polled by hand: awaiting the connection would only log its notices.
    tokio::spawn(async move {
        while let Some(message) = std::future::poll_fn(|cx| connection.poll_message(cx)).await {
            match message {
                Ok(AsyncMessage::Notice(notice)) => collected.push(&notice),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    Ok((client, notices))
}

pub(crate) struct PgLease {
    engine: Arc<PgEngine>,
    conn: Option<PgConn>,
    _permit: OwnedSemaphorePermit,
    pub(crate) reusable: bool,
}

impl PgLease {
    pub(crate) fn client(&self) -> &Client {
        &self.conn.as_ref().expect("leased connection").client
    }

    pub(crate) fn cancel_token(&self) -> CancelToken {
        self.conn.as_ref().expect("leased connection").cancel.clone()
    }

    pub(crate) fn engine(&self) -> &Arc<PgEngine> {
        &self.engine
    }

    pub(crate) fn notices(&self) -> &Notices {
        &self.conn.as_ref().expect("leased connection").notices
    }
}

impl Drop for PgLease {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        if self.reusable && !conn.client.is_closed() {
            let mut idle = self.engine.lock_idle();
            if idle.len() < MAX_IDLE {
                idle.push(conn);
            }
        }
    }
}

pub(crate) fn pg_error(err: &tokio_postgres::Error) -> QueryError {
    let Some(db) = err.as_db_error() else {
        return QueryError::message(err.to_string());
    };
    QueryError {
        message: db.message().to_string(),
        code: db.code().code().to_string(),
        detail: db.detail().unwrap_or_default().to_string(),
        hint: db.hint().unwrap_or_default().to_string(),
        position: match db.position() {
            Some(ErrorPosition::Original(position)) => *position,
            _ => 0,
        },
        severity: db.severity().to_string(),
        cancelled: db.code().code() == "57014",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use barsql_core::DriverType;

    fn options(password: &str) -> PgConnectOptions {
        let cfg = ConnectionConfig {
            driver: DriverType::Postgres,
            host: "db.example.com".into(),
            database: "blog".into(),
            username: "postgres".into(),
            password: password.into(),
            ..Default::default()
        };
        PgConnectOptions::from_config(&cfg).unwrap()
    }

    #[test]
    fn an_empty_password_is_left_out() {
        let config = pg_config(&options(""), TlsMode::Disable);
        assert_eq!(config.get_password(), None);
        assert_eq!(config.get_dbname(), Some("blog"));
        assert_eq!(config.get_user(), Some("postgres"));
        assert_eq!(pg_config(&options("secret"), TlsMode::Disable).get_password(), Some(&b"secret"[..]));
    }
}
