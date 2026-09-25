use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use barsql_core::config::{SSH_AUTH_AGENT, SSH_AUTH_KEY, SSH_AUTH_PASSWORD};
use barsql_core::{QueryError, SshConfig};
use russh::ChannelStream;
use russh::client::{self, AuthResult, Handle, Msg};
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate, check_known_hosts_path, decode_secret_key};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const DIAL_TIMEOUT: Duration = Duration::from_secs(15);
const KEEPALIVE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum SshAuth {
    Password(String),
    Key { path: PathBuf, passphrase: Option<String> },
    Agent,
}

#[derive(Debug, Clone)]
pub struct SshOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,
    // None means ~/.ssh/known_hosts.
    pub known_hosts: Option<PathBuf>,
    // Accepts any bastion key, leaving the hop open to interception.
    pub ignore_host_key: bool,
}

impl SshOptions {
    pub fn from_config(ssh: &SshConfig) -> Result<Self, QueryError> {
        if ssh.host.trim().is_empty() {
            return Err(tunnel_error("bastion host is required"));
        }
        if ssh.username.trim().is_empty() {
            return Err(tunnel_error("bastion username is required"));
        }
        let auth = match ssh.auth.as_str() {
            SSH_AUTH_PASSWORD => SshAuth::Password(ssh.password.clone()),
            SSH_AUTH_AGENT => SshAuth::Agent,
            SSH_AUTH_KEY | "" => {
                let path = expand_path(&ssh.key_path)?;
                if path.as_os_str().is_empty() {
                    return Err(tunnel_error("private key file is required"));
                }
                SshAuth::Key { path, passphrase: Some(ssh.passphrase.clone()).filter(|p| !p.is_empty()) }
            }
            other => return Err(tunnel_error(&format!("unknown authentication method {other:?}"))),
        };
        let known_hosts = if ssh.known_hosts.trim().is_empty() { None } else { Some(expand_path(&ssh.known_hosts)?) };
        Ok(Self {
            host: ssh.host.clone(),
            port: u16::try_from(if ssh.port == 0 { 22 } else { ssh.port })
                .map_err(|_| QueryError::message("SSH port must be between 1 and 65535"))?,
            username: ssh.username.clone(),
            auth,
            known_hosts,
            ignore_host_key: ssh.ignore_host_key,
        })
    }

    fn addr(&self) -> String {
        join_host_port(&self.host, self.port)
    }
}

pub struct SshTunnel {
    handle: Arc<Handle<HostKeyCheck>>,
}

pub struct Forward {
    pub addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, Clone)]
enum HostKeyProblem {
    Unknown,
    Changed(String),
}

impl SshTunnel {
    pub async fn connect(options: &SshOptions) -> Result<Self, QueryError> {
        let key = match &options.auth {
            SshAuth::Key { path, passphrase } => Some(load_key(path, passphrase.as_deref())?),
            _ => None,
        };
        let known_hosts = if options.ignore_host_key { None } else { Some(known_hosts_path(options)?) };

        let socket = tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect((options.host.as_str(), options.port)))
            .await
            .map_err(|_| tunnel_error(&format!("cannot reach bastion {}: i/o timeout", options.addr())))?
            .map_err(|err| tunnel_error(&format!("cannot reach bastion {}: {err}", options.addr())))?;
        let problem = Arc::new(Mutex::new(None));
        let handler = HostKeyCheck {
            host: options.host.clone(),
            port: options.port,
            known_hosts: known_hosts.clone(),
            problem: problem.clone(),
        };
        let config = Arc::new(client::Config {
            keepalive_interval: Some(KEEPALIVE),
            inactivity_timeout: None,
            ..Default::default()
        });
        let handshake = tokio::time::timeout(DIAL_TIMEOUT, client::connect_stream(config, socket, handler)).await;
        let mut handle = match handshake {
            Ok(Ok(handle)) => handle,
            Ok(Err(err)) => {
                let problem = problem.lock().unwrap_or_else(|e| e.into_inner()).clone();
                return Err(describe_handshake_error(options, known_hosts.as_deref(), problem, &err.to_string()));
            }
            Err(_) => return Err(tunnel_error(&format!("handshake with {} failed: i/o timeout", options.addr()))),
        };

        let authenticated = match &options.auth {
            SshAuth::Password(password) => handle.authenticate_password(&options.username, password).await,
            SshAuth::Key { .. } => {
                let key = key.expect("key loaded above");
                let hash = handle.best_supported_rsa_hash().await.map_err(ssh_error)?.flatten();
                handle.authenticate_publickey(&options.username, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await
            }
            SshAuth::Agent => return agent_auth(handle, options).await,
        };
        match authenticated.map_err(ssh_error)? {
            AuthResult::Success => Ok(Self { handle: Arc::new(handle) }),
            AuthResult::Failure { .. } => Err(rejected(options)),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    pub async fn open(&self, host: &str, port: u16) -> Result<ChannelStream<Msg>, QueryError> {
        open_channel(&self.handle, host, port).await
    }

    // Loopback-only listener for drivers that can't take a custom stream, like mysql_async.
    pub async fn forward(&self, host: &str, port: u16) -> Result<Forward, QueryError> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|err| QueryError::message(err.to_string()))?;
        let addr = listener.local_addr().map_err(|err| QueryError::message(err.to_string()))?;
        let handle = self.handle.clone();
        let host = host.to_string();
        let task = tokio::spawn(async move {
            while let Ok((mut local, _)) = listener.accept().await {
                let handle = handle.clone();
                let host = host.clone();
                tokio::spawn(async move {
                    if let Ok(mut remote) = open_channel(&handle, &host, port).await {
                        let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
                    }
                });
            }
        });
        Ok(Forward { addr, task })
    }
}

// A dead bastion connection is replaced the next time a connection is opened.
pub(crate) struct TunnelSlot {
    options: SshOptions,
    current: tokio::sync::Mutex<Option<Arc<SshTunnel>>>,
}

impl TunnelSlot {
    pub(crate) fn new(options: SshOptions) -> Arc<Self> {
        Arc::new(Self { options, current: tokio::sync::Mutex::new(None) })
    }

    pub(crate) async fn get(&self) -> Result<Arc<SshTunnel>, QueryError> {
        let mut current = self.current.lock().await;
        if let Some(tunnel) = current.as_ref().filter(|t| !t.is_closed()) {
            return Ok(tunnel.clone());
        }
        let tunnel = Arc::new(SshTunnel::connect(&self.options).await?);
        *current = Some(tunnel.clone());
        Ok(tunnel)
    }

    pub(crate) async fn open(&self, host: &str, port: u16) -> Result<ChannelStream<Msg>, QueryError> {
        self.get().await?.open(host, port).await
    }

    // Loopback-only listener for drivers like mysql_async. Reopens the bastion connection if it died.
    pub(crate) async fn forward(self: &Arc<Self>, host: &str, port: u16) -> Result<Forward, QueryError> {
        self.get().await?;
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|err| QueryError::message(err.to_string()))?;
        let addr = listener.local_addr().map_err(|err| QueryError::message(err.to_string()))?;
        let slot = self.clone();
        let host = host.to_string();
        let task = tokio::spawn(async move {
            while let Ok((mut local, _)) = listener.accept().await {
                let slot = slot.clone();
                let host = host.clone();
                tokio::spawn(async move {
                    if let Ok(mut remote) = slot.open(&host, port).await {
                        let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
                    }
                });
            }
        });
        Ok(Forward { addr, task })
    }
}

async fn open_channel(handle: &Handle<HostKeyCheck>, host: &str, port: u16) -> Result<ChannelStream<Msg>, QueryError> {
    match handle.channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0).await {
        Ok(channel) => Ok(channel.into_stream()),
        Err(err) => Err(tunnel_error(&format!("cannot reach {} from the bastion: {err}", join_host_port(host, port)))),
    }
}

async fn agent_auth(mut handle: Handle<HostKeyCheck>, options: &SshOptions) -> Result<SshTunnel, QueryError> {
    let mut agent = connect_agent().await?;
    let identities =
        agent.request_identities().await.map_err(|err| tunnel_error(&format!("cannot reach ssh-agent: {err}")))?;
    let hash = handle.best_supported_rsa_hash().await.map_err(ssh_error)?.flatten();
    for identity in identities {
        let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        if let Ok(AuthResult::Success) =
            handle.authenticate_publickey_with(&options.username, key, hash, &mut agent).await
        {
            return Ok(SshTunnel { handle: Arc::new(handle) });
        }
    }
    Err(rejected(options))
}

#[cfg(unix)]
async fn connect_agent() -> Result<russh::keys::agent::client::AgentClient<tokio::net::UnixStream>, QueryError> {
    let sock = std::env::var_os("SSH_AUTH_SOCK").filter(|s| !s.is_empty());
    let Some(sock) = sock else {
        return Err(tunnel_error("no ssh-agent found (SSH_AUTH_SOCK is not set)"));
    };
    russh::keys::agent::client::AgentClient::connect_uds(sock)
        .await
        .map_err(|err| tunnel_error(&format!("cannot reach ssh-agent: {err}")))
}

#[cfg(windows)]
async fn connect_agent()
-> Result<russh::keys::agent::client::AgentClient<tokio::net::windows::named_pipe::NamedPipeClient>, QueryError> {
    let pipe = std::env::var("SSH_AUTH_SOCK").unwrap_or_else(|_| r"\\.\pipe\openssh-ssh-agent".into());
    russh::keys::agent::client::AgentClient::connect_named_pipe(pipe)
        .await
        .map_err(|err| tunnel_error(&format!("cannot reach ssh-agent: {err}")))
}

fn load_key(path: &Path, passphrase: Option<&str>) -> Result<russh::keys::PrivateKey, QueryError> {
    let pem = std::fs::read_to_string(path)
        .map_err(|err| tunnel_error(&format!("cannot read private key {}: {err}", path.display())))?;
    parse_private_key(&pem, passphrase)
}

pub fn parse_private_key(pem: &str, passphrase: Option<&str>) -> Result<russh::keys::PrivateKey, QueryError> {
    match (decode_secret_key(pem, passphrase), passphrase) {
        (Ok(key), None) if key.is_encrypted() => Err(tunnel_error("private key is encrypted - enter its passphrase")),
        (Ok(key), _) => Ok(key),
        (Err(err), Some(_)) => Err(tunnel_error(&format!("cannot decrypt private key (wrong passphrase?): {err}"))),
        (Err(russh::keys::Error::KeyIsEncrypted), None) => {
            Err(tunnel_error("private key is encrypted - enter its passphrase"))
        }
        (Err(err), None) if err.to_string().to_lowercase().contains("encrypted") => {
            Err(tunnel_error("private key is encrypted - enter its passphrase"))
        }
        (Err(err), None) => Err(tunnel_error(&format!("cannot parse private key: {err}"))),
    }
}

fn known_hosts_path(options: &SshOptions) -> Result<PathBuf, QueryError> {
    if let Some(path) = &options.known_hosts {
        return match std::fs::metadata(path) {
            Ok(_) => Ok(path.clone()),
            Err(err) => Err(tunnel_error(&format!("cannot read known_hosts {}: {err}", path.display()))),
        };
    }
    let home = home_dir()
        .ok_or_else(|| tunnel_error("cannot locate the home directory for known_hosts: $HOME is not defined"))?;
    let path = home.join(".ssh").join("known_hosts");
    if std::fs::metadata(&path).is_err() {
        return Err(tunnel_error(&format!(
            "no known_hosts file at {path} - add the bastion with `ssh-keyscan -H {host} >> {path}`, or enable \"Skip host key check\"",
            path = path.display(),
            host = options.host
        )));
    }
    Ok(path)
}

fn describe_handshake_error(
    options: &SshOptions,
    known_hosts: Option<&Path>,
    problem: Option<HostKeyProblem>,
    err: &str,
) -> QueryError {
    match problem {
        Some(HostKeyProblem::Unknown) => {
            let path = known_hosts
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| Path::new("~").join(".ssh").join("known_hosts").display().to_string());
            tunnel_error(&format!(
                "bastion {host} is not in known_hosts - add it with `ssh-keyscan -H {host} >> {path}`, or enable \"Skip host key check\"",
                host = options.host
            ))
        }
        Some(HostKeyProblem::Changed(detail)) => tunnel_error(&format!(
            "host key for {} does not match known_hosts - the bastion key changed, or the connection is being intercepted: {detail}",
            options.host
        )),
        None => tunnel_error(&format!("handshake with {} failed: {err}", options.addr())),
    }
}

fn rejected(options: &SshOptions) -> QueryError {
    tunnel_error(&format!(
        "bastion rejected the credentials for user {:?}: unable to authenticate, no supported methods remain",
        options.username
    ))
}

// A leading ~ resolves to the home directory, so paths can be stored the way users type them.
pub fn expand_path(path: &str) -> Result<PathBuf, QueryError> {
    let path = path.trim();
    if path != "~" && !path.starts_with("~/") && !path.starts_with("~\\") {
        return Ok(PathBuf::from(path));
    }
    let home = home_dir().ok_or_else(|| tunnel_error(&format!("cannot expand {path:?}: $HOME is not defined")))?;
    Ok(if path == "~" { home } else { home.join(&path[2..]) })
}

fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).filter(|h| !h.is_empty()).map(PathBuf::from)
}

fn join_host_port(host: &str, port: u16) -> String {
    if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

struct HostKeyCheck {
    host: String,
    port: u16,
    known_hosts: Option<PathBuf>,
    problem: Arc<Mutex<Option<HostKeyProblem>>>,
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let Some(path) = &self.known_hosts else {
            return Ok(true);
        };
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_key else {
            *self.problem.lock().unwrap_or_else(|e| e.into_inner()) = Some(HostKeyProblem::Unknown);
            return Ok(false);
        };
        let verdict = match check_known_hosts_path(&self.host, self.port, key, path) {
            Ok(true) => return Ok(true),
            Ok(false) => HostKeyProblem::Unknown,
            Err(err) => HostKeyProblem::Changed(err.to_string()),
        };
        *self.problem.lock().unwrap_or_else(|e| e.into_inner()) = Some(verdict);
        Ok(false)
    }
}

fn tunnel_error(message: &str) -> QueryError {
    QueryError::message(format!("ssh tunnel: {message}"))
}

fn ssh_error(err: russh::Error) -> QueryError {
    tunnel_error(&err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_paths() {
        let home = home_dir().expect("home");
        assert_eq!(expand_path("~").unwrap(), home);
        assert_eq!(expand_path(" ~/.ssh/id_ed25519 ").unwrap(), home.join(".ssh/id_ed25519"));
        assert_eq!(expand_path("/etc/ssh/key").unwrap(), PathBuf::from("/etc/ssh/key"));
        assert_eq!(expand_path("~other/key").unwrap(), PathBuf::from("~other/key"));
        assert_eq!(expand_path("relative/id_rsa").unwrap(), PathBuf::from("relative/id_rsa"));
        assert_eq!(expand_path("/tmp/we~ird").unwrap(), PathBuf::from("/tmp/we~ird"));
    }

    #[test]
    fn incomplete_configs_are_refused() {
        let cfg = |host: &str, user: &str, auth: &str| SshConfig {
            enabled: true,
            host: host.into(),
            username: user.into(),
            auth: auth.into(),
            ignore_host_key: true,
            ..Default::default()
        };
        let cases = [
            (cfg("", "dev", SSH_AUTH_AGENT), "bastion host is required"),
            (cfg("h", "", SSH_AUTH_AGENT), "bastion username is required"),
            (cfg("h", "d", SSH_AUTH_KEY), "private key file is required"),
            (cfg("h", "d", "carrier-pigeon"), "unknown authentication method"),
        ];
        for (ssh, want) in cases {
            let err = SshOptions::from_config(&ssh).expect_err(want);
            assert!(err.message.contains(want), "{err:?}");
        }
    }
}
