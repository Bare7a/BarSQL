// A scripted HTTP server for tests: each request gets the next reply, whichever connection it arrives on.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{HttpClient, Origin};

pub(crate) enum Reply {
    Send(&'static [u8]),
    // Sends, then closes the connection.
    SendClose(&'static [u8]),
    // Closes the connection without answering.
    Close,
}

pub(crate) struct Fake {
    pub addr: SocketAddr,
    pub accepted: Arc<AtomicUsize>,
    pub requests: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    pub(crate) fn client(&self) -> HttpClient {
        HttpClient::new(Origin { tls: false, host: "127.0.0.1".into(), port: self.addr.port() }, None, None)
    }

    pub(crate) fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

// The head and body of the next request on `sock`, or None once the client closes it.
pub(crate) async fn read_request(sock: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
        .unwrap_or(0usize);
    while buf.len() < head_end + length {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

// Answers each request with the next reply, on whichever connection it arrives.
pub(crate) async fn fake(script: Vec<Reply>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let script = Arc::new(Mutex::new(VecDeque::from(script)));
    let accepted = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (counter, log) = (accepted.clone(), requests.clone());
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let (script, log) = (script.clone(), log.clone());
            tokio::spawn(async move {
                while let Some(request) = read_request(&mut sock).await {
                    log.lock().unwrap().push(request);
                    let reply = script.lock().unwrap().pop_front();
                    match reply {
                        Some(Reply::Send(bytes)) => {
                            let _ = sock.write_all(bytes).await;
                        }
                        Some(Reply::SendClose(bytes)) => {
                            let _ = sock.write_all(bytes).await;
                            return;
                        }
                        Some(Reply::Close) | None => return,
                    }
                }
            });
        }
    });
    Fake { addr, accepted, requests }
}

impl Fake {
    pub(crate) fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

// A whole response with its length, leaked since tests script them up front.
pub(crate) fn response(status: &str, body: &str) -> &'static [u8] {
    Box::leak(
        format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes().into_boxed_slice(),
    )
}
