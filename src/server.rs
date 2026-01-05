// Server module - Connection handling, TLS, health check
// Moderate path - connection setup is less frequent than command execution

use bytes::Bytes;
use http_body_util::Full as HttpFull;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls::ServerConfig as RustlsServerConfig;

use crate::config::CONFIG;
use crate::store::{ACTIVE_CONNECTIONS, MEMORY_USED, TOTAL_COMMANDS, TOTAL_CONNECTIONS};

// ==================== Unified Stream ====================

/// Unified stream type for both plain TCP and TLS connections.
/// TLS stream is boxed to reduce enum size (1216 bytes -> ~48 bytes)
pub enum MaybeStream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl AsyncRead for MaybeStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            MaybeStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeStream::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            MaybeStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeStream::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            MaybeStream::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            MaybeStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeStream::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

impl MaybeStream {
    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        match self {
            MaybeStream::Plain(s) => s.set_nodelay(nodelay),
            MaybeStream::Tls(s) => s.get_ref().0.set_nodelay(nodelay),
        }
    }
}

// ==================== TLS Configuration ====================

pub async fn load_tls_config(
    cert_path: &str,
    key_path: &str,
) -> Result<Arc<RustlsServerConfig>, Box<dyn std::error::Error>> {
    use rustls_pemfile::{certs, pkcs8_private_keys};
    use std::io::BufReader;

    let cert_file = tokio::fs::read(cert_path).await?;
    let mut cert_reader = BufReader::new(cert_file.as_slice());
    let certs: Vec<_> = certs(&mut cert_reader).collect::<Result<Vec<_>, _>>()?;

    if certs.is_empty() {
        return Err("No certificates found in cert file".into());
    }

    let key_file = tokio::fs::read(key_path).await?;
    let mut key_reader = BufReader::new(key_file.as_slice());
    let mut keys = pkcs8_private_keys(&mut key_reader).collect::<Result<Vec<_>, _>>()?;

    if keys.is_empty() {
        return Err("No private keys found in key file".into());
    }

    let key = keys.remove(0).into();

    let config = RustlsServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(Arc::new(config))
}

// ==================== Health Check Server ====================

async fn health_handler(
    _req: Request<hyper::body::Incoming>,
) -> Result<Response<HttpFull<Bytes>>, Infallible> {
    let active = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
    let total_cmds = TOTAL_COMMANDS.load(Ordering::Relaxed);
    let total_conns = TOTAL_CONNECTIONS.load(Ordering::Relaxed);
    let memory = MEMORY_USED.load(Ordering::Relaxed);

    let status = format!(
        r#"{{"status":"healthy","active_connections":{},"total_commands":{},"total_connections":{},"memory_used":{}}}"#,
        active, total_cmds, total_conns, memory
    );

    let response = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(HttpFull::new(Bytes::from(status)))
        .unwrap();

    Ok(response)
}

pub async fn start_health_check_server(port: u16) {
    let addr = format!("0.0.0.0:{}", port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to start health check server on {}: {}", addr, e);
            return;
        }
    };

    println!("Health check endpoint: http://{}/health", addr);

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(_) => continue,
        };

        let io = TokioIo::new(stream);

        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service_fn(health_handler))
                .await
            {
                eprintln!("Health check connection error: {}", e);
            }
        });
    }
}

// ==================== Connection State ====================

pub struct ConnectionState {
    pub authenticated: bool,
}

impl ConnectionState {
    pub fn new() -> Self {
        Self {
            authenticated: CONFIG.security.password.is_empty(),
        }
    }
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self::new()
    }
}

// ==================== Rate Limiting ====================

use std::sync::atomic::AtomicU64;

static LAST_RATE_CHECK: AtomicU64 = AtomicU64::new(0);
static CONNECTIONS_THIS_SECOND: AtomicU64 = AtomicU64::new(0);

pub fn check_rate_limit() -> bool {
    let rate_limit = CONFIG.server.connection_rate_limit;
    if rate_limit == 0 {
        return true;
    }

    let now = crate::store::get_timestamp();
    let last_check = LAST_RATE_CHECK.load(Ordering::Relaxed);

    if now != last_check {
        LAST_RATE_CHECK.store(now, Ordering::Relaxed);
        CONNECTIONS_THIS_SECOND.store(1, Ordering::Relaxed);
        return true;
    }

    let count = CONNECTIONS_THIS_SECOND.fetch_add(1, Ordering::Relaxed);
    count < rate_limit
}
