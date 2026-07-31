//! Plugin TCP actor — manages connection and listener handles.
//!
//! The actor processes commands from plugins (via PluginTcpCommand) and
//! internal events from accept loops (via PluginTcpInternal), enforcing
//! per-plugin resource quotas.

use crate::plugin_tcp::events::{tcp_connect, tcp_listen};
use crate::plugin_tcp::{
    CloseMap, ConnectionMap, HandleReadBytesMap, PluginEventChannel, PluginTcpCommand, PluginTcpInternal, ReadBufMap,
    SyncReply,
    MAX_CONNECTIONS_PER_PLUGIN, MAX_LISTENERS_PER_PLUGIN, MAX_PENDING_EVENTS_PER_PLUGIN,
};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};
use tracing::{info, warn};

/// Maximum concurrent event callbacks per plugin.  Prevents a single slow
/// callback from blocking the entire event channel worker for that plugin.
const MAX_CONCURRENT_CALLBACKS: usize = 4;

/// Maximum wall-clock time a single plugin event callback may run before it
/// is abandoned.  Prevents a hung/hostile plugin from pinning a worker.
const CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

struct Connection {
    remote_addr: String,
    close_tx: Option<oneshot::Sender<()>>,
}

struct Listener {
    addr: String,
    close_tx: Option<oneshot::Sender<()>>,
    /// Queue of accepted connections waiting for the plugin to call accept().
    pending_accepts: Vec<u64>,
}

/// TCP actor managing connection and listener handles.
pub struct PluginTcpActor {
    rx: mpsc::Receiver<PluginTcpCommand>,
    /// Internal events (e.g. from accept loop).
    internal_rx: mpsc::Receiver<PluginTcpInternal>,
    /// Sender cloned into accept loops for sending internal events.
    internal_tx: mpsc::Sender<PluginTcpInternal>,
    connections: HashMap<u64, Connection>,
    listeners: HashMap<u64, Listener>,
    next_handle: u64,
    conn_map: ConnectionMap,
    close_map: CloseMap,
    read_buf_map: ReadBufMap,
    event_callback: Option<Arc<dyn Fn(String, serde_json::Value) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>>,
    /// Per-plugin bounded event channels.
    event_channels: HashMap<String, Arc<PluginEventChannel>>,
    /// Per-plugin event worker task handles.  One entry per worker (there are
    /// MAX_CONCURRENT_CALLBACKS per plugin) so unload aborts ALL of them —
    /// previously only one handle was stored and the others leaked (P1).
    event_workers: HashMap<String, Vec<tokio::task::JoinHandle<()>>>,
    // ── Resource ownership (PMP25 P5) ───────────────────────────────
    /// handle → owning plugin_id
    handle_owner: HashMap<u64, String>,
    /// plugin_id → connection count
    plugin_connections: HashMap<String, u32>,
    /// plugin_id → listener count
    plugin_listeners: HashMap<String, u32>,
    /// plugin_id → total buffered read bytes
    plugin_read_bytes: Arc<Mutex<HashMap<String, usize>>>,
    /// handle → buffered read bytes (per-handle tracking for accurate cleanup)
    handle_read_bytes: HandleReadBytesMap,
    /// Handles whose connect has been spawned but not yet completed.
    /// Maps handle -> (plugin_id, reply) for cleanup on plugin removal.
    pending_connects: HashMap<u64, (String, SyncReply<u64>)>,
}

impl PluginTcpActor {
    pub fn new(rx: mpsc::Receiver<PluginTcpCommand>) -> Self {
        let (internal_tx, internal_rx) = mpsc::channel::<PluginTcpInternal>(256);
        Self {
            rx,
            internal_rx,
            internal_tx,
            connections: HashMap::new(),
            listeners: HashMap::new(),
            next_handle: 1,
            conn_map: Arc::new(Mutex::new(HashMap::new())),
            close_map: Arc::new(Mutex::new(HashMap::new())),
            read_buf_map: Arc::new(Mutex::new(HashMap::new())),
            event_callback: None,
            event_channels: HashMap::new(),
            event_workers: HashMap::new(),
            handle_owner: HashMap::new(),
            plugin_connections: HashMap::new(),
            plugin_listeners: HashMap::new(),
            plugin_read_bytes: Arc::new(Mutex::new(HashMap::new())),
            handle_read_bytes: Arc::new(Mutex::new(HashMap::new())),
            pending_connects: HashMap::new(),
        }
    }

    /// Return the event callback for wiring (used by PluginManager).
    pub fn event_callback(
        &self,
    ) -> Option<Arc<dyn Fn(String, serde_json::Value) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>>
    {
        self.event_callback.clone()
    }

    pub fn set_event_callback(
        &mut self,
        cb: Arc<dyn Fn(String, serde_json::Value) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>,
    ) {
        self.event_callback = Some(cb);
    }

    /// Remove all handles owned by a plugin (for unload cleanup).
    pub fn remove_plugin_handles(&mut self, plugin_id: &str) {
        let handles: Vec<u64> = self.handle_owner.iter()
            .filter(|(_, owner)| owner.as_str() == plugin_id)
            .map(|(h, _)| *h)
            .collect();
        for h in handles {
            self.close_handle(h);
        }
        // Clean up pending connect attempts for this plugin
        let pending: Vec<u64> = self.pending_connects.iter()
            .filter(|(_, (pid, _))| pid == plugin_id)
            .map(|(h, _)| *h)
            .collect();
        for h in pending {
            if let Some((_, reply)) = self.pending_connects.remove(&h) {
                let _ = reply.send(Err(format!("plugin '{plugin_id}' removed while connecting")));
            }
        }
        self.plugin_connections.remove(plugin_id);
        self.plugin_listeners.remove(plugin_id);
        let _ = self.plugin_read_bytes.lock().unwrap().remove(plugin_id);
        // Stop ALL event workers for this plugin and remove the channel.
        if let Some(handles) = self.event_workers.remove(plugin_id) {
            for handle in handles {
                handle.abort();
            }
        }
        self.event_channels.remove(plugin_id);
    }

    fn count_plugin_connections(&self, plugin_id: &str) -> u32 {
        self.plugin_connections.get(plugin_id).copied().unwrap_or(0)
    }

    fn count_plugin_listeners(&self, plugin_id: &str) -> u32 {
        self.plugin_listeners.get(plugin_id).copied().unwrap_or(0)
    }

    fn check_owner(&self, handle: u64, plugin_id: &str) -> Result<(), String> {
        match self.handle_owner.get(&handle) {
            Some(owner) if owner == plugin_id => Ok(()),
            Some(owner) => Err(format!("handle {handle} owned by plugin '{owner}'")),
            None => Err(format!("handle {handle} not found")),
        }
    }

    fn close_handle(&mut self, handle: u64) {
        let _ = self.conn_map.lock().unwrap().remove(&handle);
        let _ = self.close_map.lock().unwrap().remove(&handle);
        let _ = self.read_buf_map.lock().unwrap().remove(&handle);
        let buf_len = self.handle_read_bytes.lock().unwrap().remove(&handle).unwrap_or(0);
        let plugin_id = self.handle_owner.remove(&handle);
        if let Some(conn) = self.connections.remove(&handle) {
            if let Some(ref pid) = plugin_id {
                if let Some(cnt) = self.plugin_connections.get_mut(pid) {
                    *cnt = cnt.saturating_sub(1);
                    if *cnt == 0 { self.plugin_connections.remove(pid); }
                }
                if buf_len > 0 {
                    let mut prb = self.plugin_read_bytes.lock().unwrap();
                    if let Some(total) = prb.get_mut(pid) {
                        *total = total.saturating_sub(buf_len);
                        if *total == 0 { prb.remove(pid); }
                    }
                }
            }
            if let Some(tx) = conn.close_tx {
                let _ = tx.send(());
            }
            info!(%handle, addr = %conn.remote_addr, "tcp connection closed");
        } else if let Some(listener) = self.listeners.remove(&handle) {
            if let Some(ref pid) = plugin_id {
                if let Some(cnt) = self.plugin_listeners.get_mut(pid) {
                    *cnt = cnt.saturating_sub(1);
                    if *cnt == 0 { self.plugin_listeners.remove(pid); }
                }
            }
            if let Some(tx) = listener.close_tx {
                let _ = tx.send(());
            }
            info!(%handle, addr = %listener.addr, "tcp listener stopped");
        } else {
            warn!(%handle, "tcp close on unknown handle");
        }
    }

    fn alloc_handle(&mut self) -> u64 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }

    fn emit_event(&self, plugin_id: &str, event_type: &str, payload: serde_json::Value) {
        if let Some(channel) = self.event_channels.get(plugin_id) {
            channel.push(event_type.to_string(), payload);
        }
    }

    fn ensure_event_channel(&mut self, plugin_id: &str) -> Arc<PluginEventChannel> {
        use std::collections::hash_map::Entry;
        let cb = self.event_callback.clone().unwrap_or_else(|| {
            Arc::new(|_: String, _: serde_json::Value| Box::pin(async {}))
        });
        match self.event_channels.entry(plugin_id.to_string()) {
            Entry::Occupied(e) => Arc::clone(e.get()),
            Entry::Vacant(e) => {
                let channel = Arc::new(PluginEventChannel::new(MAX_PENDING_EVENTS_PER_PLUGIN));
                let (high_queue, normal_queue, worker_notify) = channel.shared();
                // Use MAX_CONCURRENT_CALLBACKS FIXED worker tasks that directly
                // await each callback.  This bounds BOTH the number of queued
                // events (bounded queues) AND the number of in-flight callbacks
                // (one per worker), unlike the previous design which spawned a
                // tokio task per event and then waited on a semaphore inside
                // each task — that could accumulate an unbounded number of
                // waiting tasks when a slow plugin was fed continuously.
                // Per-handle serialization (P0-G): events for the SAME
                // connection handle must be delivered in FIFO order, while
                // different handles may be processed concurrently.  Workers
                // acquire the handle's lock before running its callback.
                let handle_locks: Arc<Mutex<HashMap<u64, Arc<TokioMutex<()>>>>> =
                    Arc::new(Mutex::new(HashMap::new()));
                let mut worker_handles = Vec::with_capacity(MAX_CONCURRENT_CALLBACKS);
                for _ in 0..MAX_CONCURRENT_CALLBACKS {
                    let high = Arc::clone(&high_queue);
                    let normal = Arc::clone(&normal_queue);
                    let notify = Arc::clone(&worker_notify);
                    let cb = Arc::clone(&cb);
                    let handle_locks = Arc::clone(&handle_locks);
                    worker_handles.push(tokio::spawn(async move {
                        loop {
                            notify.notified().await;
                            loop {
                                // Drain high-priority (lifecycle) events first,
                                // then normal (receive) events.
                                let event = high
                                    .lock()
                                    .unwrap()
                                    .pop_front()
                                    .or_else(|| normal.lock().unwrap().pop_front());
                                match event {
                                    Some((type_, payload)) => {
                                        let event_type = type_;
                                        // Serialize per handle so events for the
                                        // same connection stay in stream order
                                        // even across the fixed workers (P0-G).
                                        // Bind the per-handle Arc in the outer
                                        // scope first; the async guard borrows
                                        // from it, so its lifetime is valid.
                                        let handle = payload
                                            .get("handle")
                                            .and_then(|h| h.as_u64());
                                        let _handle_lock: Option<Arc<TokioMutex<()>>> =
                                            handle.map(|h| {
                                                handle_locks
                                                    .lock()
                                                    .unwrap()
                                                    .entry(h)
                                                    .or_insert_with(|| Arc::new(TokioMutex::new(())))
                                                    .clone()
                                            });
                                        let _per_handle = match _handle_lock.as_ref() {
                                            Some(l) => Some(l.lock().await),
                                            None => None,
                                        };
                                        let fut = cb(event_type.clone(), payload);
                                        // Bound each callback so a hung plugin
                                        // cannot pin the worker forever.
                                        if tokio::time::timeout(CALLBACK_TIMEOUT, fut)
                                            .await
                                            .is_err()
                                        {
                                            tracing::warn!(
                                                event_type,
                                                "plugin TCP event callback timed out after \
                                                 {CALLBACK_TIMEOUT:?}"
                                            );
                                        }
                                    }
                                    None => break,
                                }
                            }
                        }
                    }));
                }
                // Track ALL worker handles so unload can abort every task
                // (previously only one was kept; the others leaked).
                self.event_workers.insert(plugin_id.to_string(), worker_handles);
                e.insert(channel).clone()
            }
        }
    }

    pub async fn run(&mut self) {
        info!("tcp actor started");

        loop {
            tokio::select! {
                biased;
                cmd = self.rx.recv() => {
                    let Some(cmd) = cmd else { break; };
                    self.handle_command(cmd).await;
                }
                internal = self.internal_rx.recv() => {
                    let Some(msg) = internal else { break; };
                    match msg {
                        PluginTcpInternal::Accepted { listener_handle, conn_handle, remote_addr, data_tx, close_tx, plugin_id_tx } => {
                            // Register accepted connection with listener's plugin
                            let plugin_id = self.handle_owner.get(&listener_handle).cloned();
                            if let Some(ref pid) = plugin_id {
                                self.ensure_event_channel(pid);
                                if self.count_plugin_connections(pid) >= MAX_CONNECTIONS_PER_PLUGIN {
                                    warn!(%conn_handle, %pid, "inbound connection quota exceeded, dropping");
                                    self.emit_event(pid, "tcp:error",
                                        serde_json::json!({"handle": conn_handle, "plugin_id": pid, "error": format!("connection quota exceeded for {pid}")}));
                                    let _ = plugin_id_tx.send(String::new());
                                    continue;
                                }
                                self.handle_owner.insert(conn_handle, pid.clone());
                                *self.plugin_connections.entry(pid.clone()).or_insert(0) += 1;
                                self.connections.insert(conn_handle, Connection {
                                    remote_addr: remote_addr.clone(),
                                    close_tx: Some(close_tx),
                                });
                                // Register in shared maps for send/close
                                self.conn_map.lock().unwrap().insert(conn_handle, data_tx);
                                // Push to pending_accepts for the plugin to accept()
                                if let Some(listener) = self.listeners.get_mut(&listener_handle) {
                                    listener.pending_accepts.push(conn_handle);
                                }
                                self.emit_event(pid, "tcp:accept",
                                    serde_json::json!({"listener_handle": listener_handle, "conn_handle": conn_handle, "remote_addr": remote_addr, "plugin_id": pid}));
                                let _ = plugin_id_tx.send(pid.clone());
                            } else {
                                let _ = plugin_id_tx.send(String::new());
                            }
                        }
                        PluginTcpInternal::Disconnected { handle, plugin_id, remote_addr } => {
                            // Remote peer closed — clean up connection state.
                            self.close_handle(handle);
                            self.emit_event(&plugin_id, "tcp:disconnect",
                                serde_json::json!({"handle": handle, "plugin_id": &plugin_id, "reason": "remote peer closed connection", "remote_addr": remote_addr}));
                        }
                        PluginTcpInternal::ConnectCompleted { handle, plugin_id, addr, result } => {
                            if let Some((_, reply)) = self.pending_connects.remove(&handle) {
                                match result {
                                    Ok((_data_tx, close_tx)) => {
                                        self.handle_owner.insert(handle, plugin_id.clone());
                                        self.connections.insert(
                                            handle,
                                            Connection {
                                                remote_addr: addr.clone(),
                                                close_tx: Some(close_tx),
                                            },
                                        );
                                        info!(%handle, %addr, "tcp connected");
                                        let _ = reply.send(Ok(handle));
                                    }
                                    Err(e) => {
                                        // Release the pre-reserved connection slot
                                        if let Some(cnt) = self.plugin_connections.get_mut(&plugin_id) {
                                            *cnt = cnt.saturating_sub(1);
                                            if *cnt == 0 { self.plugin_connections.remove(&plugin_id); }
                                        }
                                        warn!(%handle, %addr, error = %e, "tcp connect failed");
                                        let _ = reply.send(Err(e));
                                    }
                                }
                            } else {
                                // Plugin was removed while connecting — clean up maps
                                // that tcp_connect may have registered.
                                self.conn_map.lock().unwrap().remove(&handle);
                                self.read_buf_map.lock().unwrap().remove(&handle);
                                self.handle_read_bytes.lock().unwrap().remove(&handle);
                            }
                        }
                    }
                }
            }
        }
        // Actor is shutting down — cancel ALL per-plugin event workers so no
        // callback task continues to run after the actor is gone (P1: actor
        // shutdown cancellation).
        for (_plugin_id, handles) in self.event_workers.drain() {
            for handle in handles {
                handle.abort();
            }
        }
        info!("tcp actor stopped");
    }

    async fn handle_command(&mut self, cmd: PluginTcpCommand) {
        match cmd {
                PluginTcpCommand::Connect { plugin_id, addr, reply } => {
                    if self.count_plugin_connections(&plugin_id) >= MAX_CONNECTIONS_PER_PLUGIN {
                        let _ = reply.send(Err(format!("plugin '{plugin_id}' connection quota exceeded ({MAX_CONNECTIONS_PER_PLUGIN})")));
                        return;
                    }
                    let handle = self.alloc_handle();
                    // Store reply so we can send it asynchronously when connect completes
                    self.pending_connects.insert(handle, (plugin_id.clone(), reply));
                    // Pre-reserve the connection slot for quota accuracy
                    *self.plugin_connections.entry(plugin_id.clone()).or_insert(0) += 1;

                    let event_channel = self.ensure_event_channel(&plugin_id);

                    let cm = Arc::clone(&self.conn_map);
                    let rbm = Arc::clone(&self.read_buf_map);
                    let hrb = Arc::clone(&self.handle_read_bytes);
                    let itx = self.internal_tx.clone();
                    let itx_for_result = self.internal_tx.clone();
                    let prb = Arc::clone(&self.plugin_read_bytes);
                    let pid = plugin_id.clone();
                    let addr_clone = addr.clone();

                    // Spawn connect in its own task so a slow address doesn't
                    // block the actor loop.
                    tokio::spawn(async move {
                        let result = tcp_connect(
                            &addr_clone, handle, pid.clone(), cm, rbm, hrb, itx, prb, event_channel,
                        ).await;
                        let _ = itx_for_result.send(PluginTcpInternal::ConnectCompleted {
                            handle,
                            plugin_id: pid,
                            addr: addr_clone,
                            result,
                        }).await;
                    });
                }
                PluginTcpCommand::Listen { plugin_id, addr, reply } => {
                    if self.count_plugin_listeners(&plugin_id) >= MAX_LISTENERS_PER_PLUGIN {
                        let _ = reply.send(Err(format!("plugin '{plugin_id}' listener quota exceeded ({MAX_LISTENERS_PER_PLUGIN})")));
                        return;
                    }
                    let handle = self.alloc_handle();
                    let event_channel = self.ensure_event_channel(&plugin_id);
                    let rbm = Arc::clone(&self.read_buf_map);
                    let hrb = Arc::clone(&self.handle_read_bytes);
                    let prb = Arc::clone(&self.plugin_read_bytes);

                    match tcp_listen(&addr, handle, self.internal_tx.clone(), rbm, hrb, prb, event_channel).await {
                        Ok(close_tx) => {
                            self.handle_owner.insert(handle, plugin_id.clone());
                            *self.plugin_listeners.entry(plugin_id.clone()).or_insert(0) += 1;
                            self.listeners.insert(
                                handle,
                                Listener {
                                    addr: addr.clone(),
                                    close_tx: Some(close_tx),
                                    pending_accepts: Vec::new(),
                                },
                            );
                            info!(%handle, %addr, "tcp listener started");
                            let _ = reply.send(Ok(handle));
                        }
                        Err(e) => {
                            warn!(%handle, %addr, error = %e, "tcp listen failed");
                            let _ = reply.send(Err(e));
                        }
                    }
                }
                PluginTcpCommand::Send { plugin_id, handle, bytes } => {
                    if let Err(_e) = self.check_owner(handle, &plugin_id) { return; }
                    let map = self.conn_map.lock().unwrap();
                    if let Some(tx) = map.get(&handle) {
                        if let Err(e) = tx.try_send(bytes) {
                            warn!(%handle, error = %e, "tcp send failed");
                            self.emit_event(&plugin_id, "tcp:error",
                                serde_json::json!({"handle": handle, "plugin_id": &plugin_id, "error": e.to_string()}));
                        }
                    } else {
                        warn!(%handle, "tcp send on unknown handle");
                        self.emit_event(&plugin_id, "tcp:error",
                            serde_json::json!({"handle": handle, "plugin_id": &plugin_id, "error": "unknown handle"}));
                    }
                }
                PluginTcpCommand::Close { plugin_id, handle } => {
                    if self.check_owner(handle, &plugin_id).is_ok() {
                        self.close_handle(handle);
                    }
                }
                PluginTcpCommand::Accept { plugin_id, listener_handle, reply } => {
                    let result = if let Err(e) = self.check_owner(listener_handle, &plugin_id) {
                        Err(e)
                    } else if let Some(listener) = self.listeners.get_mut(&listener_handle) {
                        if let Some(conn_handle) = listener.pending_accepts.pop() {
                            Ok(Some(conn_handle))
                        } else {
                            Ok(None)
                        }
                    } else {
                        Err(format!("unknown listener handle: {listener_handle}"))
                    };
                    let _ = reply.send(result);
                }
                PluginTcpCommand::Recv { plugin_id, handle, max_bytes, reply } => {
                    let result = if let Err(e) = self.check_owner(handle, &plugin_id) {
                        Err(e)
                    } else {
                        let map = self.read_buf_map.lock().unwrap();
                        if let Some(buf) = map.get(&handle) {
                            if buf.is_empty() {
                                Ok(None)
                            } else {
                                let len = (max_bytes as usize).min(buf.len());
                                let data = buf[..len].to_vec();
                                drop(map);
                                if let Some(b) = self.read_buf_map.lock().unwrap().get_mut(&handle) {
                                    b.drain(..len);
                                }
                                // Update per-handle and per-plugin read byte tracking
                                let mut hrb = self.handle_read_bytes.lock().unwrap();
                                if let Some(v) = hrb.get_mut(&handle) {
                                    *v = v.saturating_sub(len);
                                    if *v == 0 { hrb.remove(&handle); }
                                }
                                drop(hrb);
                                let mut prb = self.plugin_read_bytes.lock().unwrap();
                                if let Some(total) = prb.get_mut(&plugin_id) {
                                    *total = total.saturating_sub(len);
                                    if *total == 0 { prb.remove(&plugin_id); }
                                }
                                drop(prb);
                                Ok(Some(data))
                            }
                        } else {
                            Err(format!("unknown connection handle: {handle}"))
                        }
                    };
                    let _ = reply.send(result);
                }
                PluginTcpCommand::PeerAddr { plugin_id, handle, reply } => {
                    let result = if let Err(e) = self.check_owner(handle, &plugin_id) {
                        Err(e)
                    } else if let Some(conn) = self.connections.get(&handle) {
                        Ok(conn.remote_addr.clone())
                    } else {
                        Err(format!("unknown handle: {handle}"))
                    };
                    let _ = reply.send(result);
                }
                PluginTcpCommand::RemovePlugin { plugin_id, reply } => {
                    self.remove_plugin_handles(&plugin_id);
                    let _ = reply.send(Ok(()));
                }
                PluginTcpCommand::Stats { reply } => {
                    let prb = self.plugin_read_bytes.lock().unwrap();
                    let mut plugins = serde_json::Map::new();
                    for (plugin_id, channel) in &self.event_channels {
                        plugins.insert(
                            plugin_id.clone(),
                            serde_json::json!({
                                "pending_events": channel.pending_count(),
                                "pending_bytes": channel.pending_bytes(),
                                "dropped_events": channel.dropped_count(),
                                "dropped_lifecycle": channel.dropped_lifecycle(),
                                "dropped_receive": channel.dropped_receive(),
                                "dropped_bytes": channel.dropped_bytes(),
                                "pending_read_bytes": prb.get(plugin_id).copied().unwrap_or(0),
                            }),
                        );
                    }
                    let _ = reply.send(Ok(serde_json::Value::Object(plugins)));
                }
        }
    }
}
