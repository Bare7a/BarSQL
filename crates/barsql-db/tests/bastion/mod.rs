#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::Channel;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

pub const USER: &str = "barsql";
pub const PASSWORD: &str = "tunnel-secret";

pub struct Bastion {
    pub port: u16,
    pub key: PublicKey,
    kill: broadcast::Sender<()>,
}

impl Bastion {
    // Simulates a bastion restart. Open sessions drop but the listener stays up.
    pub fn drop_sessions(&self) {
        let _ = self.kill.send(());
    }
}

struct Handler {
    client_key: PublicKey,
    kill: broadcast::Sender<()>,
}

impl server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == USER && password == PASSWORD { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        Ok(if user == USER && key.key_data() == self.client_key.key_data() { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Ok(mut upstream) = TcpStream::connect(format!("{host_to_connect}:{port_to_connect}")).await else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        reply.accept().await;
        let mut killed = self.kill.subscribe();
        tokio::spawn(async move {
            let mut stream = channel.into_stream();
            tokio::select! {
                _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream) => {}
                _ = killed.recv() => {}
            }
        });
        Ok(())
    }
}

// Embedded, so other crates' tests can mount this module with #[path].
const HOST_KEY: &str = include_str!("../fixtures/bastion_host_ed25519");
const CLIENT_KEY: &str = include_str!("../fixtures/client_ed25519.pub");

pub async fn start_bastion() -> Bastion {
    let key = PrivateKey::from_openssh(HOST_KEY).expect("bastion key");
    let client_key = PublicKey::from_openssh(CLIENT_KEY).expect("client key");
    let public = key.public_key().clone();
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind bastion");
    let port = listener.local_addr().expect("bastion addr").port();
    let (kill, _) = broadcast::channel(1);
    let sessions = kill.clone();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let config = config.clone();
            let handler = Handler { client_key: client_key.clone(), kill: sessions.clone() };
            let mut killed = sessions.subscribe();
            tokio::spawn(async move {
                if let Ok(session) = server::run_stream(config, socket, handler).await {
                    let handle = session.handle();
                    tokio::select! {
                        _ = session => {}
                        _ = killed.recv() => {
                            let _ = handle.disconnect(russh::Disconnect::ByApplication, "dropped".into(), "en".into()).await;
                        }
                    }
                }
            });
        }
    });
    Bastion { port, key: public, kill }
}

pub async fn start_echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind echo");
    let port = listener.local_addr().expect("echo addr").port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = socket.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    port
}

pub fn known_hosts(dir: &tempfile::TempDir, port: u16, key: Option<&PublicKey>) -> PathBuf {
    let path = dir.path().join("known_hosts");
    let line =
        key.map(|k| format!("[127.0.0.1]:{port} {}\n", k.to_openssh().expect("openssh key"))).unwrap_or_default();
    std::fs::write(&path, line).expect("write known_hosts");
    path
}
