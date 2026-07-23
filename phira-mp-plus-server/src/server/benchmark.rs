//! Server benchmark types, helpers, and execution methods.
//!
//! Extracted from the original `server.rs`.

use super::config::{normalize_phira_api_endpoint, PlusConfig};
use serde::{Deserialize, Serialize};
use tracing::{trace, warn};

/// Runtime v2 benchmark request.
pub struct BenchRequest {
    pub kind: BenchRequestKind,
    pub result_tx: std::sync::mpsc::Sender<String>,
}

impl BenchRequest {
    pub fn real(
        duration_secs: u64,
        target_rooms: usize,
        result_tx: std::sync::mpsc::Sender<String>,
    ) -> Self {
        Self {
            kind: BenchRequestKind::Real {
                duration_secs,
                target_rooms,
            },
            result_tx,
        }
    }

    pub fn hybrid(
        config: HybridBenchmarkConfig,
        result_tx: std::sync::mpsc::Sender<String>,
    ) -> Self {
        Self {
            kind: BenchRequestKind::Hybrid(config),
            result_tx,
        }
    }

    pub fn timeout_secs(&self) -> u64 {
        match &self.kind {
            BenchRequestKind::Real { duration_secs, .. } => duration_secs.saturating_add(120),
            BenchRequestKind::Hybrid(config) => config.timeout_secs(),
        }
    }
}

pub enum BenchRequestKind {
    Real {
        duration_secs: u64,
        target_rooms: usize,
    },
    Hybrid(HybridBenchmarkConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridBenchmarkConfig {
    pub duration_secs: u64,
    pub authenticate: bool,
    pub chart_lookup: Option<i32>,
    pub record_lookup: Option<i32>,
    pub upload_record: bool,
    pub endpoint_override: Option<String>,
}

impl Default for HybridBenchmarkConfig {
    fn default() -> Self {
        Self {
            duration_secs: 30,
            authenticate: false,
            chart_lookup: None,
            record_lookup: None,
            upload_record: false,
            endpoint_override: None,
        }
    }
}

impl HybridBenchmarkConfig {
    pub fn timeout_secs(&self) -> u64 {
        self.duration_secs.clamp(5, 300).saturating_add(120)
    }

    pub fn touches_phira(&self) -> bool {
        self.authenticate
            || self.chart_lookup.is_some()
            || self.record_lookup.is_some()
            || self.upload_record
    }

    pub fn enabled_switches(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.authenticate {
            out.push("authenticate".to_string());
        }
        if let Some(id) = self.chart_lookup {
            out.push(format!("chart_lookup={id}"));
        }
        if let Some(id) = self.record_lookup {
            out.push(format!("record_lookup={id}"));
        }
        if self.upload_record {
            out.push("upload_record".to_string());
        }
        out
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(5..=300).contains(&self.duration_secs) {
            return Err("hybrid duration must be between 5 and 300 seconds".to_string());
        }
        if let Some(id) = self.chart_lookup {
            if id <= 0 {
                return Err("hybrid chart_lookup id must be positive".to_string());
            }
        }
        if let Some(id) = self.record_lookup {
            if id <= 0 {
                return Err("hybrid record_lookup id must be positive".to_string());
            }
        }
        if let Some(endpoint) = &self.endpoint_override {
            normalize_phira_api_endpoint(endpoint)?;
        }
        Ok(())
    }
}

pub(crate) const BENCH_AUTH_FILE: &str = "data/benchmark-auth.json";

#[derive(Debug, Default, Deserialize, Serialize)]
struct BenchmarkAuthFile {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    tokens: Vec<String>,
}

pub(crate) fn sanitize_benchmark_tokens<I>(items: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut out: Vec<String> = Vec::new();
    for item in items {
        for token in item.split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace()) {
            let token = token.trim();
            if token.is_empty() || token.len() > 32 {
                continue;
            }
            if !out.iter().any(|existing| existing.as_str() == token) {
                out.push(token.to_string());
            }
        }
    }
    out
}

pub(crate) fn try_load_benchmark_tokens(config: &PlusConfig) -> Result<Vec<String>, String> {
    let configured = sanitize_benchmark_tokens(config.benchmark_phira_tokens.clone());
    if !configured.is_empty() {
        return Ok(configured);
    }

    let content = match std::fs::read_to_string(BENCH_AUTH_FILE) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("read {BENCH_AUTH_FILE}: {err}")),
    };
    let file = serde_json::from_str::<BenchmarkAuthFile>(&content)
        .map_err(|err| format!("parse {BENCH_AUTH_FILE}: {err}"))?;
    let mut tokens = file.tokens;
    if let Some(token) = file.token {
        tokens.push(token);
    }
    Ok(sanitize_benchmark_tokens(tokens))
}

pub(crate) fn load_benchmark_tokens(config: &PlusConfig) -> Vec<String> {
    match try_load_benchmark_tokens(config) {
        Ok(tokens) => tokens,
        Err(err) => {
            warn!(
                path = BENCH_AUTH_FILE,
                "failed to load benchmark auth file: {err}"
            );
            Vec::new()
        }
    }
}

pub(crate) fn save_benchmark_tokens(tokens: &[String]) -> Result<(), String> {
    std::fs::create_dir_all("data").map_err(|e| format!("create data directory: {e}"))?;
    let file = BenchmarkAuthFile {
        token: None,
        tokens: tokens.to_vec(),
    };
    let payload = serde_json::to_string_pretty(&file)
        .map_err(|e| format!("serialize benchmark auth: {e}"))?;
    std::fs::write(BENCH_AUTH_FILE, payload).map_err(|e| format!("write {BENCH_AUTH_FILE}: {e}"))
}

// ── PlusServerState benchmark execution methods ──────────────────────

use super::config::{Chart, Record};
use super::state::PlusServerState;
use crate::benchmark_report::{BenchmarkMode, BenchmarkReport};
use crate::plugin::PluginEvent;
use std::sync::Arc;

impl PlusServerState {
    fn append_benchmark_report(&self, out: &mut String, report: BenchmarkReport) {
        out.push_str(&report.render_text());
        self.publish_benchmark_completed(&report);
    }

    /// 绑定真实 Phira 账号 token 作为网络压测客户端。
    pub async fn bind_benchmark_tokens(&self, raw_tokens: Vec<String>) -> Result<usize, String> {
        let tokens = sanitize_benchmark_tokens(raw_tokens);
        if tokens.is_empty() {
            return Err(
                "未提供有效 token；可传入空格/逗号分隔的 1 个或多个 Phira token".to_string(),
            );
        }
        save_benchmark_tokens(&tokens)?;
        let count = tokens.len();
        *self.bench_tokens.write().await = tokens;
        Ok(count)
    }

    /// Runtime v2 hybrid benchmark probe.
    ///
    /// Hybrid is explicit and switch-driven. With all switches disabled it is a
    /// dry-run contract check and does not contact Phira. Read probes go through
    /// the unified PhiraRetryClient; write probes are intentionally blocked until
    /// upload-record semantics and safety limits are specified.
    pub async fn run_benchmark_hybrid(&self, config: HybridBenchmarkConfig) -> String {
        let mut out = String::new();
        macro_rules! o { ($($t:tt)*) => { out.push_str(&format!($($t)*)); out.push('\n'); } }

        o!("  ◆ Phira-mp+ Hybrid benchmark probe");
        o!(
            "  │ duration={}s  endpoint={}",
            config.duration_secs,
            config
                .endpoint_override
                .as_deref()
                .unwrap_or("<global phira_api_endpoint>"),
        );
        let switches = config.enabled_switches();
        let mut report = BenchmarkReport::new(
            BenchmarkMode::Hybrid,
            "hybrid Phira probe",
            config.duration_secs,
        );
        if switches.is_empty() {
            report.probes.record_skipped();
            report.add_note(
                "dry-run: no Phira request was sent because all hybrid switches are disabled",
            );
            report.add_note("simulation remains the default pressure path; real Phira probes require explicit switches");
            o!("  │ switches: none");
            o!("  │");
            o!("  ✓ hybrid dry-run complete: no Phira request was sent");
            o!("  │ simulation remains the default pressure path; real Phira probes require explicit switches");
            self.append_benchmark_report(&mut out, report);
            return out;
        }
        o!("  │ switches: {}", switches.join(", "));
        o!("  │");

        if let Err(err) = config.validate() {
            report.add_failure_sample("config", err.clone());
            o!("  ✗ invalid hybrid config: {err}");
            self.append_benchmark_report(&mut out, report);
            return out;
        }

        let endpoint_override = config.endpoint_override.as_deref();
        let tokens = self.bench_tokens.read().await.clone();
        let mut ok = 0usize;
        let mut failed = 0usize;

        if config.authenticate {
            o!("  ├─ authenticate /me");
            if let Some(token) = tokens.first() {
                match self
                    .phira_client
                    .fetch_user_by_token(&self.config.phira_api_endpoint, endpoint_override, token)
                    .await
                {
                    Some((user_id, user_name)) => {
                        ok += 1;
                        report.probes.record_success();
                        o!("  │ ✓ authenticated as {} ({})", user_name, user_id);
                    }
                    None => {
                        failed += 1;
                        report.probes.record_failure();
                        report.add_failure_sample(
                            "authenticate",
                            "fetch_user_by_token returned None".to_string(),
                        );
                        o!("  │ ✗ authenticate failed: returned None");
                    }
                }
            } else {
                failed += 1;
                report.probes.record_skipped();
                report.add_failure_sample("authenticate", "no benchmark token configured");
                o!("  │ ✗ skipped: no benchmark token configured");
                o!("  │   set benchmark_phira_tokens in server_config.yml or run benchmark-auth <token>");
            }
        }

        if let Some(chart_id) = config.chart_lookup {
            o!("  ├─ chart_lookup /chart/{chart_id}");
            match self
                .phira_client
                .get_json::<Chart>(
                    &self.config.phira_api_endpoint,
                    endpoint_override,
                    &format!("/chart/{chart_id}"),
                    None,
                    crate::phira_client::PhiraRetryNoticeTarget::Silent,
                )
                .await
            {
                Ok(chart) => {
                    ok += 1;
                    report.probes.record_success();
                    o!("  │ ✓ chart {}: {}", chart.id, chart.name);
                }
                Err(err) => {
                    failed += 1;
                    report.probes.record_failure();
                    report.add_failure_sample("chart_lookup", err.to_string());
                    o!("  │ ✗ chart lookup failed: {err}");
                }
            }
        }

        if let Some(record_id) = config.record_lookup {
            o!("  ├─ record_lookup /record/{record_id}");
            match self
                .phira_client
                .get_json::<Record>(
                    &self.config.phira_api_endpoint,
                    endpoint_override,
                    &format!("/record/{record_id}"),
                    None,
                    crate::phira_client::PhiraRetryNoticeTarget::Silent,
                )
                .await
            {
                Ok(record) => {
                    ok += 1;
                    report.probes.record_success();
                    o!(
                        "  │ ✓ record {}: player={} score={} acc={:.4}",
                        record.id,
                        record.player,
                        record.score,
                        record.accuracy
                    );
                }
                Err(err) => {
                    failed += 1;
                    report.probes.record_failure();
                    report.add_failure_sample("record_lookup", err.to_string());
                    o!("  │ ✗ record lookup failed: {err}");
                }
            }
        }

        if config.upload_record {
            failed += 1;
            report.probes.record_blocked();
            report.add_failure_sample("upload_record", "hybrid write probes are intentionally disabled until upload semantics are specified");
            o!("  ├─ upload_record");
            o!("  │ ✗ blocked: hybrid write probes are intentionally disabled until upload semantics are specified");
        }

        let stats = self.phira_client.stats();
        o!("  │");
        o!("  └─ hybrid complete: ok={ok} failed={failed}");
        o!("  │ phira_http: requests={} successes={} failures={} retry_attempts={} circuit_open={}",
            stats.requests, stats.successes, stats.failures, stats.retry_attempts, stats.circuit_open_rejections);
        report.add_note(format!(
            "phira_http requests={} successes={} failures={} retry_attempts={} circuit_open={}",
            stats.requests,
            stats.successes,
            stats.failures,
            stats.retry_attempts,
            stats.circuit_open_rejections,
        ));
        self.append_benchmark_report(&mut out, report);
        out
    }

    /// 通过真实 TCP 协议连接本服务端执行压测；不再直接篡改内存状态。
    pub async fn run_benchmark_network(&self, duration_secs: u64, target_rooms: usize) -> String {
        use std::time::Instant;

        struct BenchClient {
            stream: tokio::net::TcpStream,
            room_id: String,
        }

        let tokens = self.bench_tokens.read().await.clone();
        let mut out = String::new();
        macro_rules! o { ($($t:tt)*) => { out.push_str(&format!($($t)*)); out.push('\n'); } }

        o!("  ◆ Phira-mp+ 真实网络压测");
        o!("  │ 目标房间: {target_rooms}  测试时长: {duration_secs}s");
        o!("  │");
        if tokens.is_empty() {
            o!("  ✗ 未配置 Phira 压测账号");
            o!("  │  请配置 benchmark_phira_tokens 或执行 benchmark-auth <token>");
            o!(r#"  │  或直接修改 server_config.yml: benchmark_phira_tokens: ["..."]"#);
            o!("  │  也可以写入 data/benchmark-auth.json: {{\"tokens\":[\"...\"]}}");
            let mut report = BenchmarkReport::new(
                BenchmarkMode::Real,
                "real TCP compatibility benchmark",
                duration_secs,
            )
            .with_target_rooms(target_rooms);
            report.add_failure_sample("config", "no benchmark Phira tokens configured");
            report.add_note("real benchmark is explicit and requires local benchmark tokens; simulation remains the default pressure path");
            self.append_benchmark_report(&mut out, report);
            return out;
        }

        let room_count = target_rooms.max(1);
        let token_slots = tokens.len().max(1);
        if tokens.len() < target_rooms {
            o!(
                "  │ 账号不足：将复用 {} 个 token 分批创建/重建 {} 间房间",
                tokens.len(),
                target_rooms
            );
            o!(
                "  │ 最终只保持最多 {} 个真实客户端在线；创建吞吐仍覆盖目标房间数",
                tokens.len()
            );
            o!("  │");
        }

        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.config.port));
        let started_at = Instant::now();
        let mut clients_by_slot: Vec<Option<BenchClient>> =
            (0..token_slots).map(|_| None).collect();
        let mut created = 0usize;
        let mut rebuilt = 0usize;
        let mut joined = 0usize;
        let mut failures: Vec<String> = Vec::new();

        o!("  ├─ [阶段1] 真实 TCP 连接 + 认证 + 创建/重建房间");
        let phase1 = Instant::now();
        for i in 0..room_count {
            let slot = i % tokens.len();
            if let Some(mut old) = clients_by_slot[slot].take() {
                let _ = bench_leave_room(&mut old.stream).await;
                rebuilt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            let token = &tokens[slot];
            let room_id = format!("bench-{i}");
            match bench_connect_auth(addr, token).await {
                Ok(mut stream) => match bench_create_room(&mut stream, &room_id).await {
                    Ok(()) => {
                        clients_by_slot[slot] = Some(BenchClient { stream, room_id });
                        created += 1;
                    }
                    Err(err) => failures.push(format!("create {room_id}: {err}")),
                },
                Err(err) => failures.push(format!("auth host#{i}: {err}")),
            }
        }
        let mut clients: Vec<BenchClient> = clients_by_slot.into_iter().flatten().collect();
        o!(
            "  │ ✓ 创建/重建 {created} 间, 重建 {rebuilt} 次, 当前保持 {} 个客户端, 耗时 {:.1}s",
            clients.len(),
            phase1.elapsed().as_secs_f64()
        );

        if created == 0 {
            o!("  │");
            o!("  ✗ 没有成功创建任何房间，压测停止");
            for failure in failures.iter().take(8) {
                o!("  │  - {failure}");
            }
            let mut report = BenchmarkReport::new(
                BenchmarkMode::Real,
                "real TCP compatibility benchmark",
                duration_secs,
            )
            .with_target_rooms(target_rooms);
            report.rooms_created = Some(0);
            report.rooms_rebuilt = Some(rebuilt);
            report.failed_operations = Some(failures.len() as u64);
            for failure in failures.iter().take(8) {
                report.add_failure_sample("real_network", failure.clone());
            }
            self.append_benchmark_report(&mut out, report);
            return out;
        }

        o!("  │");
        o!("  ├─ [阶段2] 剩余账号真实 JoinRoom 填充房间");
        let phase2 = Instant::now();
        if tokens.len() > target_rooms {
            for (i, token) in tokens.iter().enumerate().skip(room_count) {
                let room_id = format!("bench-{}", (i - room_count) % created.max(1));
                match bench_connect_auth(addr, token).await {
                    Ok(mut stream) => match bench_join_room(&mut stream, &room_id, false).await {
                        Ok(()) => {
                            clients.push(BenchClient { stream, room_id });
                            joined += 1;
                        }
                        Err(err) => failures.push(format!("join token#{i}: {err}")),
                    },
                    Err(err) => failures.push(format!("auth guest#{i}: {err}")),
                }
            }
        } else {
            o!("  │ 无剩余 token 可填充玩家，已跳过；账号不足时重点测试创建/重建与连接稳定性");
        }
        o!(
            "  │ ✓ 加入 {joined} 人, 活跃客户端 {}, 耗时 {:.1}s",
            clients.len(),
            phase2.elapsed().as_secs_f64()
        );

        o!("  │");
        o!("  ├─ [阶段3] 保持连接并通过 Ping/Pong 测网络链路 {duration_secs}s");
        let phase3 = Instant::now();
        let mut op_count = 0u64;
        let mut failed_ops = 0u64;
        let mut latencies = Vec::new();
        while phase3.elapsed().as_secs() < duration_secs {
            for client in &mut clients {
                let t = Instant::now();
                match bench_ping(&mut client.stream).await {
                    Ok(()) => {
                        op_count += 1;
                        latencies.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                    Err(err) => {
                        failed_ops += 1;
                        if failures.len() < 16 {
                            failures.push(format!("ping {}: {err}", client.room_id));
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let avg_ms = if latencies.is_empty() {
            0.0
        } else {
            latencies.iter().sum::<f64>() / latencies.len() as f64
        };
        let p99_ms = if latencies.len() > 1 {
            let mut sorted = latencies.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            sorted[((sorted.len() - 1) as f64 * 0.99).round() as usize]
        } else {
            avg_ms
        };
        o!("  │ ✓ Ping/Pong {op_count} 次, 失败 {failed_ops} 次, avg={avg_ms:.2}ms p99={p99_ms:.2}ms");

        o!("  │");
        o!("  ├─ [阶段4] 通过协议 LeaveRoom 并清理 bench-* 房间");
        for client in &mut clients {
            let _ = bench_leave_room(&mut client.stream).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        self.rooms
            .write()
            .await
            .retain(|rid, _| !rid.to_string().starts_with("bench-"));
        o!("  │ ✓ 清理完成");

        o!("  │");
        o!("  └─ 压测完成 ({:.1}s)", started_at.elapsed().as_secs_f64());
        o!("");
        let mut report = BenchmarkReport::new(
            BenchmarkMode::Real,
            "real TCP compatibility benchmark",
            duration_secs,
        )
        .with_target_rooms(target_rooms);
        report.active_clients = Some(clients.len());
        report.rooms_created = Some(created);
        report.rooms_rebuilt = Some(rebuilt);
        report.users_joined = Some(joined);
        report.operations = Some(op_count);
        report.failed_operations = Some(failed_ops);
        report.avg_latency_ms = Some(avg_ms);
        report.p99_latency_ms = Some(p99_ms);
        if tokens.len() < target_rooms {
            report.add_note(format!(
                "benchmark tokens were fewer than target rooms; {} tokens were reused for {} target rooms",
                tokens.len(), target_rooms,
            ));
        }
        for failure in failures.iter().take(8) {
            report.add_failure_sample("real_network", failure.clone());
        }
        self.append_benchmark_report(&mut out, report);
        out
    }
}

// ── Benchmark protocol helpers (private, used by run_benchmark_network) ──

async fn bench_send_command(
    stream: &mut tokio::net::TcpStream,
    payload: &phira_mp_common::ClientCommand,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let mut buffer = Vec::new();
    phira_mp_common::encode_packet(payload, &mut buffer)
        .map_err(|e| format!("encode packet failed: {e}"))?;
    let mut len_buf = [0u8; 5];
    let mut x = buffer.len() as u32;
    let mut n = 0usize;
    loop {
        len_buf[n] = (x & 0x7f) as u8;
        n += 1;
        x >>= 7;
        if x == 0 {
            break;
        }
        len_buf[n - 1] |= 0x80;
    }
    stream
        .write_all(&len_buf[..n])
        .await
        .map_err(|e| format!("write length: {e}"))?;
    stream
        .write_all(&buffer)
        .await
        .map_err(|e| format!("write payload: {e}"))?;
    stream.flush().await.map_err(|e| format!("flush: {e}"))
}

async fn bench_recv_command(
    stream: &mut tokio::net::TcpStream,
) -> Result<phira_mp_common::ServerCommand, String> {
    use tokio::io::AsyncReadExt;
    let mut len = 0u32;
    let mut pos = 0;
    loop {
        let byte = stream
            .read_u8()
            .await
            .map_err(|e| format!("read length: {e}"))?;
        len |= ((byte & 0x7f) as u32) << pos;
        pos += 7;
        if byte & 0x80 == 0 {
            break;
        }
        if pos > 32 {
            return Err("invalid packet length".to_string());
        }
    }
    if len > 2 * 1024 * 1024 {
        return Err("packet too large".to_string());
    }
    let mut buffer = vec![0u8; len as usize];
    stream
        .read_exact(&mut buffer)
        .await
        .map_err(|e| format!("read payload: {e}"))?;
    phira_mp_common::decode_packet(&buffer).map_err(|e| format!("decode packet: {e}"))
}

async fn bench_connect_auth(
    addr: std::net::SocketAddr,
    token: &str,
) -> Result<tokio::net::TcpStream, String> {
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| "connect timeout".to_string())?
    .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_nodelay(true)
        .map_err(|e| format!("set_nodelay: {e}"))?;
    stream
        .write_u8(1)
        .await
        .map_err(|e| format!("write protocol version: {e}"))?;
    bench_send_command(
        &mut stream,
        &phira_mp_common::ClientCommand::Authenticate {
            token: token
                .to_string()
                .try_into()
                .map_err(|e| format!("invalid token: {e}"))?,
        },
    )
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            match bench_recv_command(&mut stream).await? {
                phira_mp_common::ServerCommand::Authenticate(Ok(_)) => return Ok(()),
                phira_mp_common::ServerCommand::Authenticate(Err(err)) => {
                    return Err(format!("authenticate rejected: {err}"))
                }
                phira_mp_common::ServerCommand::Message(_) => {}
                other => trace!(?other, "benchmark ignored packet while authenticating"),
            }
        }
    })
    .await
    .map_err(|_| "authenticate timeout".to_string())??;
    Ok(stream)
}

async fn bench_create_room(
    stream: &mut tokio::net::TcpStream,
    room_id: &str,
) -> Result<(), String> {
    bench_send_command(
        stream,
        &phira_mp_common::ClientCommand::CreateRoom {
            id: room_id
                .to_string()
                .try_into()
                .map_err(|e| format!("invalid room id: {e}"))?,
        },
    )
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::CreateRoom(Ok(())) => return Ok(()),
                phira_mp_common::ServerCommand::CreateRoom(Err(err)) => {
                    return Err(format!("create room rejected: {err}"))
                }
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while creating room"),
            }
        }
    })
    .await
    .map_err(|_| "create room timeout".to_string())?
}

async fn bench_join_room(
    stream: &mut tokio::net::TcpStream,
    room_id: &str,
    monitor: bool,
) -> Result<(), String> {
    bench_send_command(
        stream,
        &phira_mp_common::ClientCommand::JoinRoom {
            id: room_id
                .to_string()
                .try_into()
                .map_err(|e| format!("invalid room id: {e}"))?,
            monitor,
        },
    )
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::JoinRoom(Ok(_)) => return Ok(()),
                phira_mp_common::ServerCommand::JoinRoom(Err(err)) => {
                    return Err(format!("join room rejected: {err}"))
                }
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while joining room"),
            }
        }
    })
    .await
    .map_err(|_| "join room timeout".to_string())?
}

async fn bench_ping(stream: &mut tokio::net::TcpStream) -> Result<(), String> {
    bench_send_command(stream, &phira_mp_common::ClientCommand::Ping).await?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::Pong => return Ok(()),
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while waiting pong"),
            }
        }
    })
    .await
    .map_err(|_| "pong timeout".to_string())?
}

async fn bench_leave_room(stream: &mut tokio::net::TcpStream) -> Result<(), String> {
    bench_send_command(stream, &phira_mp_common::ClientCommand::LeaveRoom).await?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::LeaveRoom(_) => return Ok(()),
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| "leave timeout".to_string())?
}
