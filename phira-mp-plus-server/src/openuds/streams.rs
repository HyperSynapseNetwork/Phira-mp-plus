//! High-frequency data stream channels for OpenUDS.
//!
//! Provides dedicated channels for touch/judge data that would otherwise
//! cause head-of-line blocking on the control event channel.
//!
//! 每个流帧带连续 `sequence`、`room`/`round` 标识与服务端单调 `timestamp`，
//! 便于面板按序消费、关联房间/轮次并测量端到端延迟。
//!
//! Stream frames use the same length-prefixed format, JSON body:
//! ```json
//! {"type":"stream","stream":"touches","user_id":1001,"frames":[...],
//!  "sequence":1,"room":"bench-0","round":"uuid","timestamp":1234}
//! {"type":"stream","stream":"judges","user_id":1001,"frames":[...],
//!  "sequence":1,"room":"bench-0","round":null,"timestamp":1235}
//! ```

use crate::openuds::session::Session;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// 服务端 monotonic 时间戳基准（进程启动）。
static MONOTONIC_START: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

/// 进程启动起的单调毫秒数（只增不减，NTP 跳变不影响，供排序/延迟测量）。
pub fn monotonic_ms() -> i64 {
    std::time::Instant::now()
        .duration_since(*MONOTONIC_START)
        .as_millis() as i64
}

/// Manages active stream subscriptions and delivers stream data.
pub struct StreamManager {
    /// Active sessions indexed by session ID.
    sessions: Arc<RwLock<HashMap<Uuid, Arc<Session>>>>,
    /// touches 流连续序号。
    touches_seq: AtomicU64,
    /// judges 流连续序号。
    judges_seq: AtomicU64,
}

impl StreamManager {
    pub fn new(sessions: Arc<RwLock<HashMap<Uuid, Arc<Session>>>>) -> Self {
        Self {
            sessions,
            touches_seq: AtomicU64::new(0),
            judges_seq: AtomicU64::new(0),
        }
    }

    /// 是否有已认证会话订阅指定流。生产热路径在无订阅者时跳过
    /// round 查询与 JSON 序列化，避免白耗。
    pub async fn has_subscribers(&self, stream: &str) -> bool {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .any(|s| s.is_authenticated() && s.subscribes_to_stream(stream))
    }

    /// 向订阅 "touches" 的会话投递一帧触控数据。
    pub async fn deliver_touches(
        &self,
        user_id: i32,
        room_id: &str,
        round_uuid: Option<&str>,
        frames: serde_json::Value,
    ) {
        let seq = self.touches_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let frame = serde_json::json!({
            "type": "stream",
            "stream": "touches",
            "user_id": user_id,
            "frames": frames,
            "sequence": seq,
            "room": room_id,
            "round": round_uuid,
            "timestamp": monotonic_ms(),
        });
        self.broadcast("touches", frame).await;
    }

    /// 向订阅 "judges" 的会话投递一帧判定数据。
    pub async fn deliver_judges(
        &self,
        user_id: i32,
        room_id: &str,
        round_uuid: Option<&str>,
        events: serde_json::Value,
    ) {
        let seq = self.judges_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let frame = serde_json::json!({
            "type": "stream",
            "stream": "judges",
            "user_id": user_id,
            "frames": events,
            "sequence": seq,
            "room": room_id,
            "round": round_uuid,
            "timestamp": monotonic_ms(),
        });
        self.broadcast("judges", frame).await;
    }

    /// 向订阅指定流的会话广播一帧。
    async fn broadcast(&self, stream: &str, frame: serde_json::Value) {
        let sessions = self.sessions.read().await;
        for (_id, session) in sessions.iter() {
            if session.is_authenticated() && session.subscribes_to_stream(stream) {
                let _ = session.send(frame.clone()).await;
            }
        }
    }

    /// Deliver one sequenced server-log occurrence to subscribers of `logs`.
    pub async fn deliver_logs(&self, entry: crate::history::LogHistoryEntry) {
        let sessions = self.sessions.read().await;
        for (_id, session) in sessions.iter() {
            if session.is_authenticated() && session.subscribes_to_stream("logs") {
                let frame = Session::stream_response(
                    "logs",
                    0,
                    serde_json::json!({ "seq": entry.seq, "line": entry.line }),
                );
                let _ = session.send(frame).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openuds::session::Session;

    /// 构造一个已认证且订阅指定流的会话，返回 (Arc<Session>, 接收端)。
    fn mock_subscriber(stream: &str) -> (Arc<Session>, tokio::sync::mpsc::Receiver<serde_json::Value>) {
        let (session, rx) = Session::new(64);
        session.set_authenticated();
        session.add_stream_subscriptions(&[stream.to_string()]);
        (Arc::new(session), rx)
    }

    fn manager() -> StreamManager {
        let sessions: Arc<RwLock<HashMap<Uuid, Arc<Session>>>> = Arc::new(RwLock::new(HashMap::new()));
        StreamManager::new(sessions)
    }

    #[tokio::test]
    async fn touches_frame_has_context_fields() {
        let mgr = manager();
        let (session, mut rx) = mock_subscriber("touches");
        {
            let mut s = mgr.sessions.write().await;
            s.insert(session.id, session);
        }
        mgr.deliver_touches(1001, "bench-0", Some("round-abc"), serde_json::json!([{"x": 0.5}])).await;

        let frame = rx.recv().await.expect("frame");
        assert_eq!(frame["type"], "stream");
        assert_eq!(frame["stream"], "touches");
        assert_eq!(frame["user_id"], 1001);
        assert_eq!(frame["sequence"], 1);
        assert_eq!(frame["room"], "bench-0");
        assert_eq!(frame["round"], "round-abc");
        assert_eq!(frame["frames"][0]["x"], 0.5);
        assert!(frame["timestamp"].as_i64().unwrap() >= 0);
    }

    #[tokio::test]
    async fn judges_frame_round_null_when_absent() {
        let mgr = manager();
        let (session, mut rx) = mock_subscriber("judges");
        {
            let mut s = mgr.sessions.write().await;
            s.insert(session.id, session);
        }
        mgr.deliver_judges(1002, "bench-1", None, serde_json::json!([{"note_id": 7}])).await;

        let frame = rx.recv().await.expect("frame");
        assert_eq!(frame["stream"], "judges");
        assert_eq!(frame["sequence"], 1);
        assert_eq!(frame["room"], "bench-1");
        assert!(frame["round"].is_null());
        assert_eq!(frame["frames"][0]["note_id"], 7);
    }

    #[tokio::test]
    async fn sequence_increments_per_stream() {
        let mgr = manager();
        let (t, mut t_rx) = mock_subscriber("touches");
        let (j, mut j_rx) = mock_subscriber("judges");
        {
            let mut s = mgr.sessions.write().await;
            s.insert(t.id, t);
            s.insert(j.id, j);
        }
        mgr.deliver_touches(1, "r", None, serde_json::json!([])).await;
        mgr.deliver_judges(2, "r", None, serde_json::json!([])).await;
        mgr.deliver_touches(1, "r", None, serde_json::json!([])).await;

        assert_eq!(t_rx.recv().await.unwrap()["sequence"], 1);
        assert_eq!(j_rx.recv().await.unwrap()["sequence"], 1);
        assert_eq!(t_rx.recv().await.unwrap()["sequence"], 2);
    }

    #[test]
    fn monotonic_timestamp_never_decreases() {
        let a = monotonic_ms();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = monotonic_ms();
        assert!(b >= a);
    }

    #[tokio::test]
    async fn has_subscribers_only_counts_authenticated_matching_stream() {
        let mgr = manager();
        let (sess, _rx) = Session::new(64);
        sess.set_authenticated();
        sess.add_stream_subscriptions(&["touches".to_string()]);
        {
            let mut s = mgr.sessions.write().await;
            s.insert(sess.id, Arc::new(sess));
        }
        // 有 touches 订阅：命中。
        assert!(mgr.has_subscribers("touches").await);
        // 无 judges 订阅：未命中。
        assert!(!mgr.has_subscribers("judges").await);
    }
}
