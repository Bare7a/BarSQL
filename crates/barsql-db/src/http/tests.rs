use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::fake::{Reply, fake};
use super::{HttpClient, Origin, Request};

const OK_HELLO: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";

#[tokio::test]
async fn a_finished_body_leaves_the_connection_for_the_next_request() {
    let server = fake(vec![Reply::Send(OK_HELLO), Reply::Send(OK_HELLO)]).await;
    let client = server.client();
    for _ in 0..2 {
        let mut res = client.send(&Request::get("/v3")).await.unwrap();
        assert_eq!((res.status, res.read_to_end(1024).await.unwrap()), (200, b"hello".to_vec()));
    }
    assert_eq!(server.accepted(), 1);
    let first = server.requests.lock().unwrap()[0].clone();
    assert!(first.starts_with(&format!("GET /v3 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n", server.addr.port())), "{first}");
}

#[tokio::test]
async fn posts_carry_their_body_and_headers() {
    let server = fake(vec![Reply::Send(OK_HELLO)]).await;
    let req = Request::post("/q?x=1", b"SELECT 1").header("Authorization", "Bearer t");
    server.client().send(&req).await.unwrap().read_to_end(1024).await.unwrap();
    let sent = server.requests.lock().unwrap()[0].clone();
    assert!(sent.starts_with("POST /q?x=1 HTTP/1.1\r\n"), "{sent}");
    assert!(sent.contains("\r\nAuthorization: Bearer t\r\n") && sent.contains("\r\nContent-Length: 8\r\n"), "{sent}");
    assert!(sent.ends_with("\r\n\r\nSELECT 1"), "{sent}");
}

#[tokio::test]
async fn chunked_bodies_skip_extensions_and_trailers() {
    let reply: &[u8] =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4;ext=1\r\nWiki\r\n5\r\npedia\r\n0\r\nX-T: a\r\n\r\n";
    let server = fake(vec![Reply::Send(reply), Reply::Send(OK_HELLO)]).await;
    let client = server.client();
    let mut res = client.send(&Request::get("/")).await.unwrap();
    assert_eq!(res.read_to_end(1024).await.unwrap(), b"Wikipedia");
    client.send(&Request::get("/")).await.unwrap().read_to_end(1024).await.unwrap();
    assert_eq!(server.accepted(), 1, "a chunked body ends cleanly, so the connection is reused");
}

#[tokio::test]
async fn a_body_without_a_length_runs_to_the_close() {
    let server = fake(vec![Reply::SendClose(b"HTTP/1.1 200 OK\r\n\r\nall of it"), Reply::Send(OK_HELLO)]).await;
    let client = server.client();
    assert_eq!(client.send(&Request::get("/")).await.unwrap().read_to_end(1024).await.unwrap(), b"all of it");
    client.send(&Request::get("/")).await.unwrap().read_to_end(1024).await.unwrap();
    assert_eq!(server.accepted(), 2);
}

#[tokio::test]
async fn interim_responses_are_skipped() {
    let reply: &[u8] = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok";
    let server = fake(vec![Reply::Send(reply)]).await;
    let mut res = server.client().send(&Request::post("/", b"x")).await.unwrap();
    assert_eq!((res.status, res.read_to_end(10).await.unwrap()), (201, b"ok".to_vec()));
}

#[tokio::test]
async fn a_stale_idle_connection_is_redialed_once() {
    let server = fake(vec![Reply::Send(OK_HELLO), Reply::Close, Reply::Send(OK_HELLO)]).await;
    let client = server.client();
    client.send(&Request::get("/")).await.unwrap().read_to_end(1024).await.unwrap();
    let mut res = client.send(&Request::get("/")).await.unwrap();
    assert_eq!(res.read_to_end(1024).await.unwrap(), b"hello");
    assert_eq!(server.accepted(), 2);
}

#[tokio::test]
async fn connection_close_and_short_keep_alive_aren_t_reused() {
    let close: &[u8] = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 1\r\n\r\nx";
    let brief: &[u8] = b"HTTP/1.1 200 OK\r\nKeep-Alive: timeout=1\r\nContent-Length: 1\r\n\r\nx";
    let server = fake(vec![Reply::Send(close), Reply::Send(brief), Reply::Send(OK_HELLO)]).await;
    let client = server.client();
    for _ in 0..3 {
        client.send(&Request::get("/")).await.unwrap().read_to_end(1024).await.unwrap();
    }
    assert_eq!(server.accepted(), 3);
}

#[tokio::test]
async fn an_unfinished_body_drops_its_connection() {
    let big: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\npartial";
    let server = fake(vec![Reply::Send(big), Reply::Send(OK_HELLO)]).await;
    let client = server.client();
    let mut res = client.send(&Request::get("/")).await.unwrap();
    assert_eq!(res.chunk().await.unwrap().unwrap(), b"partial");
    drop(res);
    client.send(&Request::get("/")).await.unwrap().read_to_end(1024).await.unwrap();
    assert_eq!(server.accepted(), 2);
}

#[tokio::test]
async fn truncated_and_oversized_responses_fail() {
    let server = fake(vec![Reply::SendClose(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello")]).await;
    let mut res = server.client().send(&Request::get("/")).await.unwrap();
    let err = res.read_to_end(1024).await.unwrap_err();
    assert!(err.message.contains("mid-response"), "{err:?}");

    let head: &'static [u8] =
        Box::leak([b"HTTP/1.1 200 OK\r\nX-Pad: ".as_slice(), &[b'a'; 70_000]].concat().into_boxed_slice());
    let server = fake(vec![Reply::Send(head)]).await;
    let err = server.client().send(&Request::get("/")).await.err().unwrap();
    assert!(err.message.contains("too large"), "{err:?}");

    let server = fake(vec![Reply::Send(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n01234567890123456789")]).await;
    let err = server.client().send(&Request::get("/")).await.unwrap().read_to_end(10).await.unwrap_err();
    assert!(err.message.contains("larger than"), "{err:?}");
}

#[tokio::test]
async fn lines_come_out_whole_whatever_the_chunking() {
    let reply: &[u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n6\r\n\":1}\n{\r\n7\r\n}\n\nlast\r\n0\r\n\r\n";
    let server = fake(vec![Reply::Send(reply)]).await;
    let mut res = server.client().send(&Request::get("/")).await.unwrap();
    let mut lines = Vec::new();
    while let Some(line) = res.next_line(1024).await.unwrap() {
        lines.push(String::from_utf8(line).unwrap());
    }
    assert_eq!(lines, ["{\"a\":1}", "{}", "", "last"]);
}

#[tokio::test]
async fn an_unreachable_server_says_where() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let client = HttpClient::new(Origin { tls: false, host: "127.0.0.1".into(), port }, None, None);
    let err = client.send(&Request::get("/")).await.err().unwrap();
    assert!(err.message.starts_with(&format!("can't connect to 127.0.0.1:{port}")), "{err:?}");
}

// sslmode=require only encrypts, so a self-signed server works. verify-full checks the certificate.
#[tokio::test]
async fn https_negotiates_http1_and_verifies_unless_told_not_to() {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio_rustls::TlsAcceptor;

    use crate::tls::{TlsMode, http_client_config};

    let cert = CertificateDer::from_pem_slice(include_bytes!("testdata/cert.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("testdata/key.pem")).unwrap();
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(sock).await else { return };
                let mut buf = [0u8; 4096];
                let _ = tls.read(&mut buf).await;
                let alpn = tls.get_ref().1.alpn_protocol().map(|p| String::from_utf8_lossy(p).into_owned());
                let body = format!("alpn={}", alpn.unwrap_or_default());
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                let _ = tls.write_all(reply.as_bytes()).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    let origin = Origin { tls: true, host: "127.0.0.1".into(), port };
    let relaxed = HttpClient::new(origin.clone(), http_client_config(TlsMode::Require), None);
    let body = relaxed.send(&Request::get("/")).await.unwrap().read_to_end(1024).await.unwrap();
    assert_eq!(body, b"alpn=http/1.1", "the server mustn't pick h2");
    let strict = HttpClient::new(origin, http_client_config(TlsMode::VerifyFull), None);
    let err = strict.send(&Request::get("/")).await.err().unwrap();
    assert!(err.message.contains("certificate"), "{err:?}");
}
