mod capture;
mod schema;
mod session;
#[cfg(test)]
mod tests;
mod values;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use barsql_core::{ConnectionConfig, QueryError};
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, SqlBrowser};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

use crate::postgres::port_or;
use crate::ssh::{SshOptions, TunnelSlot};

pub use session::MsSession;

const MAX_CONNECTIONS: usize = 10;
const MAX_IDLE: usize = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
// What SSMS and the ODBC driver set on a new session. TEXTSIZE keeps (max) values whole.
const SESSION_SETUP: &str = "SET TEXTSIZE 2147483647; SET ANSI_NULLS ON; SET ANSI_PADDING ON; SET ANSI_WARNINGS ON; \
                             SET ARITHABORT ON; SET CONCAT_NULL_YIELDS_NULL ON; SET QUOTED_IDENTIFIER ON; \
                             SET ANSI_NULL_DFLT_ON ON; SET XACT_ABORT OFF; SET NOCOUNT OFF";

pub(crate) trait Transport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Transport for T {}

// TCP or an SSH channel.
pub(crate) type MsClient = Client<Compat<Box<dyn Transport>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MsTls {
    // Only the login is encrypted.
    Disable,
    // Everything, without checking the certificate. SQL Server makes its own certificate when it has none.
    Require,
    VerifyFull,
    // TDS 8.0: TLS before anything else, certificate checked.
    Strict,
}

#[derive(Debug, Clone)]
pub struct MsOptions {
    host: String,
    port: u16,
    // Found through the SQL Browser service, in place of the port.
    instance: Option<String>,
    user: String,
    password: String,
    database: String,
    schema: String,
    tls: MsTls,
    read_only: bool,
    ssh: Option<SshOptions>,
}

impl MsOptions {
    pub fn from_config(cfg: &ConnectionConfig) -> Result<Self, QueryError> {
        // `host\INSTANCE`, as SQL Server's own tools write it.
        let (host, named) = match cfg.host.split_once('\\') {
            Some((host, instance)) => (host.to_string(), Some(instance.to_string())),
            None => (cfg.host.clone(), None),
        };
        let instance = Some(cfg.instance.clone()).filter(|i| !i.is_empty()).or(named).filter(|i| !i.is_empty());
        let ssh = if cfg.ssh.enabled { Some(SshOptions::from_config(&cfg.ssh)?) } else { None };
        if instance.is_some() && ssh.is_some() {
            return Err(QueryError::message(
                "a named instance can't be looked up through an SSH tunnel; give the instance's port instead",
            ));
        }
        let tls = match cfg.ssl_mode.as_str() {
            "disable" => MsTls::Disable,
            "require" | "" => MsTls::Require,
            "verify-full" => MsTls::VerifyFull,
            "strict" => MsTls::Strict,
            other => return Err(QueryError::message(format!("unknown TLS mode {other}"))),
        };
        Ok(Self {
            host,
            port: port_or(cfg.port, 1433)?,
            instance,
            user: cfg.username.clone(),
            password: cfg.password.clone(),
            database: cfg.database.clone(),
            schema: if cfg.schema.is_empty() { "dbo".into() } else { cfg.schema.clone() },
            tls,
            read_only: cfg.read_only,
            ssh,
        })
    }

    fn config(&self) -> Config {
        let mut config = Config::new();
        config.host(&self.host);
        config.port(self.port);
        if let Some(instance) = &self.instance {
            config.instance_name(instance);
        }
        if !self.database.is_empty() {
            config.database(&self.database);
        }
        config.application_name("BarSQL");
        config.authentication(AuthMethod::sql_server(&self.user, &self.password));
        config.lossy_utf16_decoding(true);
        // A statement may take as long as it takes. tiberius' default would end the connection after 30 s.
        config.command_timeout(None);
        config.handshake_timeout(Some(CONNECT_TIMEOUT));
        // Only routes to a readable secondary. BarSQL's own check is what keeps the connection read-only.
        config.readonly(self.read_only);
        match self.tls {
            MsTls::Disable => {
                config.encryption(EncryptionLevel::Off);
                config.trust_cert();
            }
            MsTls::Require => {
                config.encryption(EncryptionLevel::Required);
                config.trust_cert();
            }
            MsTls::VerifyFull => config.encryption(EncryptionLevel::Required),
            MsTls::Strict => config.encryption(EncryptionLevel::Strict),
        }
        config
    }
}

pub struct MsEngine {
    pub(crate) options: MsOptions,
    tunnel: Option<Arc<TunnelSlot>>,
    idle: Mutex<Vec<MsClient>>,
    permits: Arc<Semaphore>,
}

impl MsEngine {
    pub async fn connect(options: MsOptions) -> Result<Arc<Self>, QueryError> {
        capture::install();
        let tunnel = options.ssh.clone().map(TunnelSlot::new);
        let engine = Arc::new(Self {
            options,
            tunnel,
            idle: Mutex::new(Vec::new()),
            permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        });
        let first = Box::pin(engine.open_client()).await?;
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

    pub(crate) async fn lease(self: &Arc<Self>) -> Result<MsLease, QueryError> {
        let permit =
            self.permits.clone().acquire_owned().await.map_err(|_| QueryError::message("connection is closed"))?;
        let pooled = self.lock_idle().pop();
        // Boxed: a login is a big future, and every catalog call would carry it inline.
        let client = match pooled {
            Some(client) => client,
            None => Box::pin(self.open_client()).await?,
        };
        Ok(MsLease { engine: self.clone(), client: Some(client), _permit: permit, reusable: true, broken: false })
    }

    pub async fn session(self: &Arc<Self>) -> Result<MsSession, QueryError> {
        let mut lease = self.lease().await?;
        lease.reusable = false;
        Ok(MsSession::new(lease))
    }

    // Goes back to the pool afterwards, so only for SQL the app generates.
    pub async fn pooled_session(self: &Arc<Self>) -> Result<MsSession, QueryError> {
        Ok(MsSession::new(self.lease().await?))
    }

    pub fn close(&self) {
        self.permits.close();
        self.lock_idle().clear();
    }

    async fn open_client(&self) -> Result<MsClient, QueryError> {
        tokio::time::timeout(CONNECT_TIMEOUT, self.login())
            .await
            .map_err(|_| QueryError::message("failed to connect: timeout"))?
    }

    async fn login(&self) -> Result<MsClient, QueryError> {
        let mut config = self.options.config();
        let (mut host, mut port) = (self.options.host.clone(), self.options.port);
        let mut tokens = Vec::new();
        let mut routed = false;
        let mut client = loop {
            let stream = self.dial(&host, port, &config).await?;
            match capture::captured(&mut tokens, Client::connect(config.clone(), stream.compat())).await {
                Ok(client) => break client,
                // Azure SQL's gateway sends the client on to the database's own node.
                Err(tiberius::error::Error::Routing { host: to, port: at }) if !routed => {
                    config.host(&to);
                    config.port(at);
                    (host, port, routed) = (to, at, true);
                }
                Err(error) => return Err(ms_error(error, "")),
            }
        };
        let setup = async { client.simple_query(SESSION_SETUP).await?.into_results().await };
        capture::captured(&mut tokens, setup).await.map_err(|error| ms_error(error, ""))?;
        Ok(client)
    }

    async fn dial(&self, host: &str, port: u16, config: &Config) -> Result<Box<dyn Transport>, QueryError> {
        if let Some(slot) = &self.tunnel {
            return Ok(Box::new(slot.open(host, port).await?));
        }
        let tcp = match &self.options.instance {
            Some(_) => TcpStream::connect_named(config).await.map_err(|error| ms_error(error, ""))?,
            None => TcpStream::connect((host, port))
                .await
                .map_err(|error| QueryError::message(format!("failed to connect: {error}")))?,
        };
        let _ = tcp.set_nodelay(true);
        Ok(Box::new(tcp))
    }

    fn lock_idle(&self) -> std::sync::MutexGuard<'_, Vec<MsClient>> {
        self.idle.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) struct MsLease {
    engine: Arc<MsEngine>,
    client: Option<MsClient>,
    _permit: OwnedSemaphorePermit,
    pub(crate) reusable: bool,
    // Out of step with the server, after an I/O or protocol error or a cancel that didn't settle.
    pub(crate) broken: bool,
}

impl MsLease {
    pub(crate) fn client(&mut self) -> &mut MsClient {
        self.client.as_mut().expect("leased connection")
    }

    pub(crate) fn engine(&self) -> &Arc<MsEngine> {
        &self.engine
    }
}

impl Drop for MsLease {
    fn drop(&mut self) {
        let Some(client) = self.client.take() else { return };
        if self.reusable && !self.broken {
            let mut idle = self.engine.lock_idle();
            if idle.len() < MAX_IDLE {
                idle.push(client);
            }
        }
    }
}

// `sql` is the batch, to turn the error's line into a position in it.
pub(crate) fn ms_error(error: tiberius::error::Error, sql: &str) -> QueryError {
    let tiberius::error::Error::Server(token) = error else {
        return QueryError::message(error.to_string());
    };
    let mut detail =
        format!("Msg {}, Level {}, State {}, Line {}", token.code(), token.class(), token.state(), token.line());
    if !token.procedure().is_empty() {
        detail.push_str(&format!(", Procedure {}", token.procedure()));
    }
    QueryError {
        message: token.message().to_string(),
        code: token.code().to_string(),
        detail,
        // A procedure's lines are its own, not the batch's.
        position: if token.procedure().is_empty() { line_position(sql, token.line()) } else { 0 },
        severity: if token.class() >= 20 { "FATAL" } else { "ERROR" }.into(),
        ..Default::default()
    }
}

// The 1-based character position where line `line` of `sql` starts its code.
fn line_position(sql: &str, line: u32) -> u32 {
    if line == 0 {
        return 0;
    }
    let mut chars = 0;
    for (ix, text) in sql.split_inclusive('\n').enumerate() {
        if ix + 1 == line as usize {
            let indent = text.chars().take_while(|c| c.is_whitespace() && *c != '\n').count();
            return u32::try_from(chars + indent + 1).unwrap_or(0);
        }
        chars += text.chars().count();
    }
    0
}
