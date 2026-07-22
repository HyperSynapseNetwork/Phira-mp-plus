//! TCP/TLS 连接句柄管理 — WASM 插件通过 WIT host API 使用的
//! 出站/入站 TLS 连接。
//!
//! connect   → TCP 连接 + TLS 客户端握手 + 双向读写循环
//! listen    → TCP 监听 + TLS 服务端握手 + 按连接读写循环
//! send      → 通过共享连接注册表写入对应 TLS 流
//! close     → 信号关闭 + 清理
//!
//! 连接注册表 (`ConnectionMap`) 在 actor 与 accept 生成的
//! 子任务间共享，确保 `send(handle, bytes)` 对 listen 接受的
//! 连接同样有效。

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, ServerConfig};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tracing::{error, info, warn};

/// TLS configuration for a federated connection.
#[derive(Debug, Clone)]
pub struct FederationTlsOpts {
    pub expected_ca_ids: Vec<String>,
    pub verify_peer: bool,
    pub local_cert_chain: Option<String>,
    pub local_private_key: Option<String>,
    pub min_tls_version: Option<String>,
    /// Timeout (seconds) for TCP connection establishment. Default 10s.
    pub connect_timeout_secs: u64,
    /// Timeout (seconds) for TLS handshake. Default 10s.
    pub handshake_timeout_secs: u64,
    /// Timeout (seconds) for idle reads. 0 = disabled (default).
    pub read_timeout_secs: u64,
}

const fn default_connect_timeout_secs() -> u64 { 10 }
const fn default_handshake_timeout_secs() -> u64 { 10 }

/// Commands plugins send to the federation actor.
#[derive(Debug)]
pub enum FederationCommand {
    Connect {
        addr: String,
        tls_opts: FederationTlsOpts,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Listen {
        addr: String,
        tls_opts: FederationTlsOpts,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Send { handle: u64, bytes: Vec<u8> },
    SetReadTimeout { handle: u64, timeout_ms: u64 },
    Close { handle: u64 },
}

/// Events from the federation actor back to the plugin system.
#[derive(Debug, Clone)]
pub enum FederationEvent {
    Accepted {
        listener_handle: u64,
        conn_handle: u64,
        peer_pubkey: String,
        peer_ca_id: String,
        remote_addr: String,
    },
    Received {
        handle: u64,
        bytes: Vec<u8>,
    },
    Disconnected {
        handle: u64,
        reason: String,
    },
    Error {
        handle: u64,
        error: String,
    },
}

/// Shared connection registry: conn_handle → sender for outgoing data.
type ConnectionMap = Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>>;

/// Shared close-signal registry: conn_handle → close sender.
/// Each accepted TLS connection has a close signal that the actor
/// triggers when the plugin calls Close on the listener-handle.
type CloseMap = Arc<Mutex<HashMap<u64, oneshot::Sender<()>>>>;

/// Per-connection state tracked by the actor.
struct Connection {
    remote_addr: String,
    close_tx: Option<oneshot::Sender<()>>,
    /// Read timeout configured at connection time.
    /// Updated by SetReadTimeout (takes effect for future reads).
    read_timeout_secs: u64,
}

/// Per-listener state.
struct Listener {
    addr: String,
    close_tx: Option<oneshot::Sender<()>>,
}

/// Federation actor managing connection and listener handles.
pub struct FederationActor {
    rx: mpsc::Receiver<FederationCommand>,
    connections: HashMap<u64, Connection>,
    listeners: HashMap<u64, Listener>,
    next_handle: u64,
    conn_map: ConnectionMap,
    close_map: CloseMap,
    event_callback: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
}

impl FederationActor {
    pub fn new(rx: mpsc::Receiver<FederationCommand>) -> Self {
        Self {
            rx,
            connections: HashMap::new(),
            listeners: HashMap::new(),
            next_handle: 1,
            conn_map: Arc::new(Mutex::new(HashMap::new())),
            close_map: Arc::new(Mutex::new(HashMap::new())),
            event_callback: None,
        }
    }

    pub fn set_event_callback(
        &mut self,
        cb: Arc<dyn Fn(String, serde_json::Value) + Send + Sync>,
    ) {
        self.event_callback = Some(cb);
    }

    fn alloc_handle(&mut self) -> u64 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }

    fn emit_event(&self, event_type: &str, payload: serde_json::Value) {
        if let Some(cb) = &self.event_callback {
            cb(event_type.to_string(), payload);
        }
    }

    pub async fn run(&mut self) {
        info!("federation actor started");

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                FederationCommand::Connect {
                    addr,
                    tls_opts,
                    reply,
                } => {
                    let handle = self.alloc_handle();
                    let cb = self.event_callback.clone();
                    let cm = Arc::clone(&self.conn_map);

                    let read_timeout_secs = tls_opts.read_timeout_secs;
                    match connect_with_tls(&addr, &tls_opts, handle, cb, cm).await {
                        Ok((_data_tx, close_tx)) => {
                            self.connections.insert(
                                handle,
                                Connection {
                                    remote_addr: addr.clone(),
                                    close_tx: Some(close_tx),
                                    read_timeout_secs,
                                },
                            );
                            info!(%handle, %addr, "federation connected");
                            let _ = reply.send(Ok(handle));
                        }
                        Err(e) => {
                            warn!(%handle, %addr, error = %e, "federation connect failed");
                            let _ = reply.send(Err(e));
                        }
                    }
                }
                FederationCommand::Listen {
                    addr,
                    tls_opts,
                    reply,
                } => {
                    let handle = self.alloc_handle();
                    let cb = self.event_callback.clone();
                    let cm = Arc::clone(&self.conn_map);
                    let clm = Arc::clone(&self.close_map);

                    match listen_with_tls(&addr, &tls_opts, handle, cb, cm, clm).await {
                        Ok(close_tx) => {
                            self.listeners.insert(
                                handle,
                                Listener {
                                    addr: addr.clone(),
                                    close_tx: Some(close_tx),
                                },
                            );
                            info!(%handle, %addr, "federation listener started");
                            let _ = reply.send(Ok(handle));
                        }
                        Err(e) => {
                            warn!(%handle, %addr, error = %e, "federation listen failed");
                            let _ = reply.send(Err(e));
                        }
                    }
                }
                FederationCommand::Send { handle, bytes } => {
                    let map = self.conn_map.lock().unwrap();
                    if let Some(tx) = map.get(&handle) {
                        if let Err(e) = tx.try_send(bytes) {
                            warn!(%handle, error = %e, "federation send failed");
                            self.emit_event(
                                "federation:error",
                                serde_json::json!({"handle": handle, "error": e.to_string()}),
                            );
                        }
                    } else {
                        warn!(%handle, "federation send on unknown handle");
                        self.emit_event(
                            "federation:error",
                            serde_json::json!({"handle": handle, "error": "unknown handle"}),
                        );
                    }
                }
                FederationCommand::SetReadTimeout { handle, timeout_ms } => {
                    if let Some(conn) = self.connections.get_mut(&handle) {
                        let secs = (timeout_ms / 1000).max(1);
                        conn.read_timeout_secs = secs;
                        info!(%handle, %timeout_ms, %secs, "federation read timeout updated");
                    } else {
                        warn!(%handle, "federation SetReadTimeout on unknown handle");
                    }
                }
                FederationCommand::Close { handle } => {
                    let _ = self.conn_map.lock().unwrap().remove(&handle);
                    // Try close_map first (accepted listener connections).
                    if let Some(close_tx) = self.close_map.lock().unwrap().remove(&handle) {
                        let _ = close_tx.send(());
                        info!(%handle, "federation accepted connection closed");
                    } else if let Some(conn) = self.connections.remove(&handle) {
                        if let Some(tx) = conn.close_tx {
                            let _ = tx.send(());
                        }
                        info!(%handle, addr = %conn.remote_addr, "federation connection closed");
                    } else if let Some(listener) = self.listeners.remove(&handle) {
                        if let Some(tx) = listener.close_tx {
                            let _ = tx.send(());
                        }
                        info!(%handle, addr = %listener.addr, "federation listener stopped");
                    } else {
                        warn!(%handle, "federation close on unknown handle");
                    }
                }
            }
        }

        info!("federation actor stopped");
    }
}

// ── TCP/TLS connection helpers ──────────────────────────────────────────

fn extract_dns_name(addr: &str) -> Result<ServerName<'static>, String> {
    let host = addr
        .split(':')
        .next()
        .ok_or_else(|| format!("invalid addr (no host): {addr}"))?;
    ServerName::try_from(host.to_string())
        .map_err(|e| format!("invalid TLS server name '{host}': {e}"))
}

fn tls_protocol_versions(min_version: &Option<String>) -> Result<Vec<&'static rustls::SupportedProtocolVersion>, String> {
    match min_version.as_deref() {
        None | Some("1.2") => Ok(vec![&rustls::version::TLS12, &rustls::version::TLS13]),
        Some("1.3") => Ok(vec![&rustls::version::TLS13]),
        Some(other) => Err(format!("unsupported min TLS version: {other}")),
    }
}

fn build_client_tls_config(tls_opts: &FederationTlsOpts) -> Result<Arc<ClientConfig>, String> {
    let root_store = rustls::RootCertStore::from_iter(
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
    );
    let versions = tls_protocol_versions(&tls_opts.min_tls_version)?;

    let builder = ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_protocol_versions(&versions)
    .map_err(|e| format!("protocol versions: {e}"))?
    .with_root_certificates(root_store);

    let mut config = if let (Some(cert_pem), Some(key_pem)) =
        (&tls_opts.local_cert_chain, &tls_opts.local_private_key)
    {
        let certs = parse_pem_certs(cert_pem)?;
        let key = parse_pem_key(key_pem)?;
        builder
            .with_client_auth_cert(certs, key)
            .map_err(|e| format!("invalid client cert/key: {e}"))?
    } else {
        builder.with_no_client_auth()
    };

    if !tls_opts.verify_peer {
        config
            .dangerous()
            .set_certificate_verifier(Arc::new(AcceptAllVerifier));
    }

    Ok(Arc::new(config))
}

/// Check that the peer certificate's CA hash is in the expected list.
fn check_expected_ca(expected_ca_ids: &[String], ca_id: &str) -> Result<(), String> {
    if expected_ca_ids.is_empty() {
        return Ok(());
    }
    if expected_ca_ids.iter().any(|id| id == ca_id) {
        Ok(())
    } else {
        Err(format!("peer CA {ca_id} not in expected CA list"))
    }
}

/// Extract peer identity from peer certificates.
fn peer_identity(peer_certs: &[CertificateDer]) -> (String, String) {
    let cert_id = peer_certs.first().map(|c| {
        let h = Sha256::digest(c.as_ref());
        h.iter().map(|b| format!("{:02x}", b)).collect::<String>()
    }).unwrap_or_default();
    let ca_id = if peer_certs.len() > 1 {
        let h = Sha256::digest(peer_certs[1].as_ref());
        h.iter().map(|b| format!("{:02x}", b)).collect::<String>()
    } else {
        cert_id.clone()
    };
    (cert_id, ca_id)
}

fn build_server_tls_config(tls_opts: &FederationTlsOpts) -> Result<Arc<ServerConfig>, String> {
    let cert_pem = tls_opts
        .local_cert_chain
        .as_deref()
        .ok_or_else(|| "local_cert_chain required for TLS listener".to_string())?;
    let key_pem = tls_opts
        .local_private_key
        .as_deref()
        .ok_or_else(|| "local_private_key required for TLS listener".to_string())?;
    let certs = parse_pem_certs(cert_pem)?;
    let key = parse_pem_key(key_pem)?;

    let versions = tls_protocol_versions(&tls_opts.min_tls_version)?;
    let builder = ServerConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_protocol_versions(&versions)
    .map_err(|e| format!("protocol versions: {e}"))?;

    let server_config = if tls_opts.verify_peer {
        let root_store = rustls::RootCertStore::from_iter(
            webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
        );
        builder
            .with_client_cert_verifier(
                rustls::server::WebPkiClientVerifier::builder(root_store.into())
                    .build()
                    .map_err(|e| format!("build client verifier: {e}"))?,
            )
            .with_single_cert(certs, key)
            .map_err(|e| format!("build server config: {e}"))?
    } else {
        builder
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| format!("build server config: {e}"))?
    };

    Ok(Arc::new(server_config))
}

/// Connect to a remote peer: TCP + TLS handshake, spawn read/write task.
async fn connect_with_tls(
    addr: &str,
    tls_opts: &FederationTlsOpts,
    handle: u64,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    conn_map: ConnectionMap,
) -> Result<(mpsc::Sender<Vec<u8>>, oneshot::Sender<()>), String> {
    use tokio::time::timeout;

    let connect_timeout = Duration::from_secs(tls_opts.connect_timeout_secs.max(1));
    let handshake_timeout = Duration::from_secs(tls_opts.handshake_timeout_secs.max(1));

    let dns_name = extract_dns_name(addr)?;
    let tls_config = build_client_tls_config(tls_opts)?;

    let stream = timeout(connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| format!("TCP connect to {addr} timed out ({}s)", connect_timeout.as_secs()))?
        .map_err(|e| format!("TCP connect to {addr}: {e}"))?;

    let connector = TlsConnector::from(tls_config);
    let tls_stream = timeout(handshake_timeout, connector.connect(dns_name, stream))
        .await
        .map_err(|_| format!("TLS handshake to {addr} timed out ({}s)", handshake_timeout.as_secs()))?
        .map_err(|e| format!("TLS handshake to {addr}: {e}"))?;

    // Extract peer identity and verify expected CA.
    let (_cert_id, ca_id) = peer_identity(tls_stream.get_ref().1.peer_certificates().unwrap_or_default());
    check_expected_ca(&tls_opts.expected_ca_ids, &ca_id)?;

    let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(64);
    let (close_tx, close_rx) = oneshot::channel();

    conn_map
        .lock()
        .unwrap()
        .insert(handle, data_tx.clone());

    let remote = addr.to_string();
    let read_to = tls_opts.read_timeout_secs;
    let cm = Arc::clone(&conn_map);
    tokio::spawn(async move {
        connection_read_task(tls_stream, handle, data_rx, close_rx, event_cb, remote, read_to).await;
        cm.lock().unwrap().remove(&handle);
    });

    Ok((data_tx, close_tx))
}

/// Start a TLS listener: bind TCP, accept loop.
async fn listen_with_tls(
    addr: &str,
    tls_opts: &FederationTlsOpts,
    listener_handle: u64,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    conn_map: ConnectionMap,
    close_map: CloseMap,
) -> Result<oneshot::Sender<()>, String> {
    let tls_config = build_server_tls_config(tls_opts)?;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("TCP bind {addr}: {e}"))?;

    let (close_tx, close_rx) = oneshot::channel();
    let acceptor = TlsAcceptor::from(tls_config);
    let read_to = tls_opts.read_timeout_secs;

    tokio::spawn(accept_loop(
        listener,
        acceptor,
        listener_handle,
        close_rx,
        event_cb,
        conn_map,
        read_to,
        close_map,
    ));

    Ok(close_tx)
}

/// Accept loop.
async fn accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    listener_handle: u64,
    mut close_rx: oneshot::Receiver<()>,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    conn_map: ConnectionMap,
    read_timeout_secs: u64,
    close_map: CloseMap,
) {
    let mut next_conn: u64 = 1;
    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((stream, addr)) => {
                        let peer = addr.to_string();
                        let conn_handle = (listener_handle << 32) | next_conn;
                        next_conn += 1;
                        let acceptor = acceptor.clone();
                        let cb = event_cb.clone();
                        let cm = Arc::clone(&conn_map);

                        tokio::spawn(async move {
                            match acceptor.accept(stream).await {
                                Ok(tls_stream) => {
                                    let (cert_id, ca_id) = peer_identity(tls_stream.get_ref().1.peer_certificates().unwrap_or_default());
                                    let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(64);
                                    let (close_tx, close_rx) = oneshot::channel::<()>();

                                    cm.lock().unwrap().insert(conn_handle, data_tx);
                                    close_map.lock().unwrap().insert(conn_handle, close_tx);

                                    if let Some(ref cb) = cb {
                                        cb("federation:accept".into(), serde_json::json!({
                                            "listener_handle": listener_handle,
                                            "conn_handle": conn_handle,
                                            "peer_pubkey": cert_id,
                                            "peer_ca_id": ca_id,
                                            "remote_addr": peer,
                                        }));
                                    }

                                    connection_read_task(
                                        tls_stream, conn_handle, data_rx, close_rx, cb, peer, read_timeout_secs,
                                    ).await;

                                    cm.lock().unwrap().remove(&conn_handle);
                                    close_map.lock().unwrap().remove(&conn_handle);
                                }
                                Err(e) => {
                                    warn!(%peer, error = %e, "TLS accept failed");
                                }
                            }
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "accept failed");
                        continue;
                    }
                }
            }
            _ = &mut close_rx => {
                info!(%listener_handle, "federation listener shutting down");
                break;
            }
        }
    }
}

/// Read task: reads from TLS stream, writes outgoing data, dispatches events.
async fn connection_read_task(
    tls_stream: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    handle: u64,
    mut data_rx: mpsc::Receiver<Vec<u8>>,
    mut close_rx: oneshot::Receiver<()>,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    _remote_addr: String,
    read_timeout_secs: u64,
) {
    use tokio::io::AsyncWriteExt;

    let (mut reader, mut writer) = tokio::io::split(tls_stream);
    let mut buf = vec![0u8; 8192];
    let cb = event_cb.unwrap_or_else(|| Arc::new(|_, _| {}));

    loop {
        tokio::select! {
            data = data_rx.recv() => {
                match data {
                    Some(bytes) => {
                        if let Err(e) = writer.write_all(&bytes).await {
                            cb("federation:error".into(), serde_json::json!({
                                "handle": handle, "error": format!("write: {e}"),
                            }));
                            break;
                        }
                    }
                    None => break,
                }
            }
            result = async {
                if read_timeout_secs > 0 {
                    tokio::time::timeout(
                        Duration::from_secs(read_timeout_secs),
                        reader.read(&mut buf),
                    )
                    .await
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "read timeout"))?
                } else {
                    reader.read(&mut buf).await
                }
            } => {
                match result {
                    Ok(0) => {
                        cb("federation:disconnect".into(), serde_json::json!({
                            "handle": handle, "reason": "remote peer closed connection",
                        }));
                        break;
                    }
                    Ok(n) => {
                        cb("federation:receive".into(), serde_json::json!({
                            "handle": handle, "bytes": buf[..n].to_vec(),
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                        cb("federation:error".into(), serde_json::json!({
                            "handle": handle, "error": "read timed out",
                        }));
                        break;
                    }
                    Err(e) => {
                        cb("federation:error".into(), serde_json::json!({
                            "handle": handle, "error": format!("read: {e}"),
                        }));
                        break;
                    }
                }
            }
            _ = &mut close_rx => {
                break;
            }
        }
    }

    cb("federation:disconnect".into(), serde_json::json!({
        "handle": handle, "reason": "connection task exited",
    }));
}

// ── PEM parsing ─────────────────────────────────────────────────────────

fn parse_pem_certs(pem: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs = rustls_pemfile::certs(&mut pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("parse PEM certs: {e}"))?;
    if certs.is_empty() {
        return Err("no certificates found in PEM data".to_string());
    }
    Ok(certs)
}

fn parse_pem_key(pem: &str) -> Result<PrivateKeyDer<'static>, String> {
    let reader = &mut pem.as_bytes();
    if let Ok(Some(key)) = rustls_pemfile::pkcs8_private_keys(reader)
        .collect::<Result<Vec<_>, _>>()
        .map(|mut ks| if ks.len() == 1 { Some(PrivateKeyDer::Pkcs8(ks.remove(0))) } else { None })
    {
        return Ok(key);
    }
    let reader = &mut pem.as_bytes();
    if let Ok(Some(key)) = rustls_pemfile::ec_private_keys(reader)
        .collect::<Result<Vec<_>, _>>()
        .map(|mut ks| if ks.len() == 1 { Some(PrivateKeyDer::Sec1(ks.remove(0))) } else { None })
    {
        return Ok(key);
    }
    Err("no valid private key found in PEM data (tried PKCS#8 and SEC1)".to_string())
}

// ── Accept-all cert verifier ─────────────────────────────────────────────

#[derive(Debug)]
struct AcceptAllVerifier;

impl ServerCertVerifier for AcceptAllVerifier {
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }

    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        let algs = &rustls::crypto::ring::default_provider().signature_verification_algorithms;
        rustls::crypto::verify_tls12_signature(message, cert, dss, algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        let algs = &rustls::crypto::ring::default_provider().signature_verification_algorithms;
        rustls::crypto::verify_tls13_signature(message, cert, dss, algs)
    }
}
