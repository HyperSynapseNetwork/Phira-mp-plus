//! Async TCP I/O tasks — connect, listen, accept loops, and read tasks.
//!
//! These functions perform the actual network I/O on behalf of the
//! PluginTcpActor, sending internal events back to it for state management.

use crate::plugin_tcp::quota::{MAX_READ_BUF_PER_CONNECTION, MAX_READ_BUF_PER_PLUGIN};
use crate::plugin_tcp::{ConnectionMap, PluginTcpInternal, ReadBufMap};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

pub(crate) async fn tcp_connect(
    addr: &str,
    handle: u64,
    plugin_id: String,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    conn_map: ConnectionMap,
    read_buf_map: ReadBufMap,
    internal_tx: mpsc::Sender<PluginTcpInternal>,
    plugin_read_bytes: Arc<Mutex<HashMap<String, usize>>>,
) -> Result<(mpsc::Sender<Vec<u8>>, oneshot::Sender<()>), String> {
    let stream = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("TCP connect to {addr}: {e}"))?;

    let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(64);
    let (close_tx, close_rx) = oneshot::channel();
    conn_map.lock().unwrap().insert(handle, data_tx.clone());
    read_buf_map.lock().unwrap().insert(handle, Vec::new());

    let remote = addr.to_string();
    let cm = Arc::clone(&conn_map);
    let rbm = Arc::clone(&read_buf_map);
    let prb = Arc::clone(&plugin_read_bytes);
    tokio::spawn(async move {
        tcp_read_task(stream, handle, data_rx, close_rx, event_cb, remote, rbm, internal_tx, plugin_id, prb).await;
        cm.lock().unwrap().remove(&handle);
    });

    Ok((data_tx, close_tx))
}

pub(crate) async fn tcp_listen(
    addr: &str,
    listener_handle: u64,
    internal_tx: mpsc::Sender<PluginTcpInternal>,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    read_buf_map: ReadBufMap,
    plugin_read_bytes: Arc<Mutex<HashMap<String, usize>>>,
) -> Result<oneshot::Sender<()>, String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("TCP bind {addr}: {e}"))?;

    let (close_tx, close_rx) = oneshot::channel();
    tokio::spawn(tcp_accept_loop(
        listener, listener_handle, internal_tx, close_rx, event_cb, read_buf_map, plugin_read_bytes,
    ));
    Ok(close_tx)
}

async fn tcp_accept_loop(
    listener: TcpListener,
    listener_handle: u64,
    internal_tx: mpsc::Sender<PluginTcpInternal>,
    mut close_rx: oneshot::Receiver<()>,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    read_buf_map: ReadBufMap,
    plugin_read_bytes: Arc<Mutex<HashMap<String, usize>>>,
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
                        let cb = event_cb.clone();
                        let rbm = Arc::clone(&read_buf_map);
                        let itx = internal_tx.clone();
                        let prb = Arc::clone(&plugin_read_bytes);
                        let peer_for_msg = peer.clone();
                        tokio::spawn(async move {
                            let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(64);
                            let (close_tx, close_rx) = oneshot::channel::<()>();
                            let (plugin_id_tx, plugin_id_rx) = oneshot::channel::<String>();
                            let _ = itx.send(PluginTcpInternal::Accepted {
                                listener_handle,
                                conn_handle,
                                remote_addr: peer_for_msg,
                                data_tx,
                                close_tx,
                                plugin_id_tx,
                            }).await;
                            let plugin_id = plugin_id_rx.await.unwrap_or_default();
                            tcp_read_task(stream, conn_handle, data_rx, close_rx, cb, peer, rbm, itx, plugin_id, prb).await;
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "accept failed");
                        continue;
                    }
                }
            }
            _ = &mut close_rx => {
                info!(%listener_handle, "tcp listener shutting down");
                break;
            }
        }
    }
}

async fn tcp_read_task(
    stream: TcpStream,
    handle: u64,
    mut data_rx: mpsc::Receiver<Vec<u8>>,
    mut close_rx: oneshot::Receiver<()>,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    remote_addr: String,
    read_buf_map: ReadBufMap,
    internal_tx: mpsc::Sender<PluginTcpInternal>,
    plugin_id: String,
    plugin_read_bytes: Arc<Mutex<HashMap<String, usize>>>,
) {
    use tokio::io::AsyncWriteExt;

    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut buf = vec![0u8; 8192];
    let cb = event_cb.unwrap_or_else(|| Arc::new(|_, _| {}));
    let cb_plugin_id = plugin_id.clone();

    loop {
        tokio::select! {
            data = data_rx.recv() => {
                match data {
                    Some(bytes) => {
                        if let Err(e) = writer.write_all(&bytes).await {
                            cb("tcp:error".into(), serde_json::json!({
                                "handle": handle, "plugin_id": cb_plugin_id.clone(),
                                "error": format!("write: {e}"),
                            }));
                            break;
                        }
                    }
                    None => break,
                }
            }
            result = reader.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        // Send Disconnected to actor before cleaning up buffers
                        let _ = internal_tx.try_send(PluginTcpInternal::Disconnected {
                            handle,
                            plugin_id: cb_plugin_id.clone(),
                            remote_addr: remote_addr.clone(),
                        });
                        read_buf_map.lock().unwrap().remove(&handle);
                        if !plugin_id.is_empty() {
                            let mut prb = plugin_read_bytes.lock().unwrap();
                            prb.remove(&plugin_id);
                        }
                        cb("tcp:disconnect".into(), serde_json::json!({
                            "handle": handle, "plugin_id": cb_plugin_id.clone(),
                            "reason": "remote peer closed connection",
                        }));
                        break;
                    }
                    Ok(n) => {
                        // Buffer for pull-based recv(), with read buffer limits
                        let mut rbm = read_buf_map.lock().unwrap();
                        let entry = rbm.entry(handle).or_default();

                        // Per-connection limit: drop oldest data if at limit
                        if entry.len() + n > MAX_READ_BUF_PER_CONNECTION {
                            let excess = entry.len() + n - MAX_READ_BUF_PER_CONNECTION;
                            let drain_end = entry.len().min(excess);
                            entry.drain(..drain_end);
                            // Track dropped bytes
                            if !plugin_id.is_empty() && drain_end > 0 {
                                let mut prb = plugin_read_bytes.lock().unwrap();
                                if let Some(total) = prb.get_mut(&plugin_id) {
                                    *total = total.saturating_sub(drain_end);
                                }
                            }
                        }

                        // Per-plugin limit: cap how much we add
                        let room = MAX_READ_BUF_PER_CONNECTION.saturating_sub(entry.len());
                        let to_buffer = n.min(room);
                        let actually_buffered = if !plugin_id.is_empty() && to_buffer > 0 {
                            let plugin_total = {
                                let prb = plugin_read_bytes.lock().unwrap();
                                prb.get(&plugin_id).copied().unwrap_or(0)
                            };
                            let plugin_room = MAX_READ_BUF_PER_PLUGIN.saturating_sub(plugin_total);
                            to_buffer.min(plugin_room)
                        } else {
                            to_buffer
                        };

                        if actually_buffered > 0 {
                            entry.extend_from_slice(&buf[..actually_buffered]);
                            if !plugin_id.is_empty() {
                                let mut prb = plugin_read_bytes.lock().unwrap();
                                *prb.entry(plugin_id.clone()).or_insert(0) += actually_buffered;
                            }
                        }
                        drop(rbm);

                        cb("tcp:receive".into(), serde_json::json!({
                            "handle": handle, "plugin_id": cb_plugin_id.clone(),
                            "bytes": buf[..n].to_vec(),
                        }));
                    }
                    Err(e) => {
                        let _ = internal_tx.try_send(PluginTcpInternal::Disconnected {
                            handle,
                            plugin_id: cb_plugin_id.clone(),
                            remote_addr: remote_addr.clone(),
                        });
                        read_buf_map.lock().unwrap().remove(&handle);
                        if !plugin_id.is_empty() {
                            let mut prb = plugin_read_bytes.lock().unwrap();
                            prb.remove(&plugin_id);
                        }
                        cb("tcp:error".into(), serde_json::json!({
                            "handle": handle, "plugin_id": cb_plugin_id.clone(),
                            "error": format!("read: {e}"),
                        }));
                        break;
                    }
                }
            }
            _ = &mut close_rx => break,
        }
    }
    cb("tcp:disconnect".into(), serde_json::json!({
        "handle": handle, "plugin_id": cb_plugin_id,
        "reason": "connection task exited",
    }));
}
