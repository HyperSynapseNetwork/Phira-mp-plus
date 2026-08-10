//! 进程内基准测试 harness。
//!
//! 直接调用线上 `PlusServerState` 内部 API 生成负载（不跑 phira 线协议）：
//! - 会话 = 虚拟 User（负数 id，`id > 0` 才计入真实玩家，天然从线上隔离）；
//! - 游玩房间 = `create_empty_room → set_chart → add_user → start_room` 推到
//!   Playing，房间按生命周期轮换（close + recreate）重置内存并产生真实负载；
//! - 触控/判定经 `room_commands.add_touches/add_judges`（telemetry 通道）制造 CPU 负载。
//!
//! 支持两种模式：Fixed（维持会话/游玩房间上限）+ Ramp（加压直到 CPU/RAM 触顶）。
//! 实时进度经 `CliStatus` 上报（TUI 状态矩形 + 进度条），x 键取消。

use super::mode::{BenchmarkMode, ModeParams};
use super::report::{BenchmarkReport, RampReached};
use super::sampler::{read_rss_bytes, CpuSampler};
use crate::cli_status::CliStatus;
use crate::l10n::Language;
use crate::plugin::TouchEventPoint;
use crate::server::PlusServerState;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 每 tick 时长。
const TICK: Duration = Duration::from_millis(100);
/// Ramp 模式房间数硬上限（防失控）。
const RAMP_MAX_ROOMS: usize = 5000;
/// 每房间成员数（从会话池抽取）。
const MEMBERS_PER_ROOM: usize = 2;
/// 房间生命周期（到点 close + recreate 重置内存）。
const ROOM_LIFETIME: Duration = Duration::from_secs(30);
/// 每房间每 tick 触控点批大小。
const TOUCHES_PER_ROOM_PER_TICK: u32 = 20;
/// 每房间选曲 ID（benchmark 统一用，无 Phira API 校验）。
const BENCH_CHART_ID: i32 = 114_514;
/// 负数会话 id 基址（避开真实玩家 id>0 与远程玩家的小负数范围）。
const SESSION_ID_BASE: i32 = -100_000_000;

/// 会话池中一个虚拟会话。
struct SessionEntry {
    user_id: i32,
    name: String,
}

/// 一个游玩房间。
struct RoomEntry {
    room_id: String,
    /// 房间成员（取第一个用于 telemetry）。
    member_ids: Vec<i32>,
    created_at: Instant,
}

/// 进程内基准测试 harness。
pub struct BenchmarkHarness {
    state: Arc<PlusServerState>,
    params: ModeParams,
    status: Arc<CliStatus>,

    sessions: Vec<SessionEntry>,
    rooms: Vec<RoomEntry>,
    next_room_idx: u32,
    next_user_idx: u32,
    /// 全局成员序号：每个房间从池里取独立的成员（房间 i → sessions[2i], sessions[2i+1]）。
    next_member_idx: usize,
    started: Instant,

    // 指标累加
    total_commands: u64,
    errors: u64,
    peak_sessions: u32,
    peak_rooms: u32,
    peak_cpu: f64,
    peak_ram: u64,
    session_sum: u64,
    room_sum: u64,
    cpu_sum: f64,
    ram_sum: u64,
    sample_count: u64,
    ramp_reached: Option<RampReached>,
    aborted: bool,
    abort_reason: Option<String>,
    /// 最近一次 add_room 错误（暴露到 TUI 状态，便于诊断卡住原因）。
    last_error: Option<String>,

    sampler: CpuSampler,
}

impl BenchmarkHarness {
    pub fn new(
        state: Arc<PlusServerState>,
        params: ModeParams,
        status: Arc<CliStatus>,
    ) -> Self {
        Self {
            state,
            params,
            status,
            sessions: Vec::new(),
            rooms: Vec::new(),
            next_room_idx: 0,
            next_user_idx: 0,
            next_member_idx: 0,
            started: Instant::now(),
            total_commands: 0,
            errors: 0,
            peak_sessions: 0,
            peak_rooms: 0,
            peak_cpu: 0.0,
            peak_ram: 0,
            session_sum: 0,
            room_sum: 0,
            cpu_sum: 0.0,
            ram_sum: 0,
            sample_count: 0,
            ramp_reached: None,
            aborted: false,
            abort_reason: None,
            last_error: None,
            sampler: CpuSampler::new(),
        }
    }

    // ── 会话 ─────────────────────────────────────────────────────

    /// 创建一个虚拟会话（负数 id 的 User 注册进 state.users）。
    async fn add_session(&mut self) -> i32 {
        let id = SESSION_ID_BASE - self.next_user_idx as i32;
        self.next_user_idx += 1;
        let name = format!("bench-{}", self.next_user_idx);
        let user = crate::session_lifecycle::User::new(
            id,
            name.clone(),
            Language::default(),
            Arc::clone(&self.state),
            None,
        );
        self.state.users.write().await.insert(id, Arc::new(user));
        self.sessions.push(SessionEntry {
            user_id: id,
            name,
        });
        id
    }

    /// 取会话池第 `index` 个成员：不存在则创建，保证每个房间拿到独立成员
    /// （会话数无上限，随房间需要自动增长）。
    async fn ensure_member(&mut self, index: usize) -> i32 {
        while self.sessions.len() <= index {
            self.add_session().await;
        }
        self.sessions[index].user_id
    }

    // ── 房间 ─────────────────────────────────────────────────────

    /// 创建一个房间并推到 Playing 状态。
    ///
    /// start_room 失败时关闭房间并返回 Err，保证只有真正进入 Playing 的
    /// 房间才计入"游玩房间"计数。
    async fn add_room(&mut self) -> Result<(), String> {
        let idx = self.next_room_idx;
        self.next_room_idx += 1;
        let room_id = format!("bench-{idx}");
        self.state.create_empty_room(&room_id, None, false).await?;

        let mut member_ids = Vec::with_capacity(MEMBERS_PER_ROOM);
        for _ in 0..MEMBERS_PER_ROOM {
            let uid = self.ensure_member(self.next_member_idx).await;
            self.next_member_idx += 1;
            let name = self
                .sessions
                .iter()
                .find(|s| s.user_id == uid)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| uid.to_string());
            let deadline = Instant::now() + Duration::from_secs(30);
            if let Err(e) = self
                .state
                .room_commands
                .add_user(&self.state, &room_id, uid, &name, false, deadline, None)
                .await
            {
                self.errors += 1;
                tracing::warn!(room = &room_id, user = uid, %e, "bench add_user failed");
            }
            member_ids.push(uid);
        }

        if let Err(e) = self
            .state
            .room_commands
            .set_chart(
                &self.state,
                &room_id,
                BENCH_CHART_ID,
                "bench",
                member_ids.first().copied().unwrap_or(SESSION_ID_BASE),
                None,
                None,
            )
            .await
        {
            self.errors += 1;
            tracing::warn!(room = &room_id, %e, "bench set_chart failed");
        }
        // 大谱面时长：避免 lifecycle 维护在压测中自动结束 Playing。
        let _ = self
            .state
            .room_commands
            .set_chart_duration(&self.state, &room_id, Some(9_000_000.0))
            .await;

        if let Err(e) = self
            .state
            .room_commands
            .start_room(&self.state, &room_id)
            .await
        {
            self.errors += 1;
            tracing::warn!(room = &room_id, %e, "bench start_room failed");
            self.close_room(&room_id).await;
            return Err(format!("start_room failed: {e}"));
        }

        self.total_commands += 1;
        self.rooms.push(RoomEntry {
            room_id,
            member_ids,
            created_at: Instant::now(),
        });
        Ok(())
    }

    /// 关闭一个房间（teardown / 轮换）。
    async fn close_room(&self, room_id: &str) {
        if let Err(e) = self.state.room_commands.close_room(&self.state, room_id).await {
            tracing::warn!(room = room_id, %e, "bench close_room failed");
        }
    }

    // ── 负载泵送 ─────────────────────────────────────────────────

    /// 给每个游玩房间发一批触控（CPU 负载）。
    async fn pump_touches(&mut self) {
        let touches: Vec<TouchEventPoint> = (0..TOUCHES_PER_ROOM_PER_TICK)
            .map(|i| TouchEventPoint {
                time: i as f32 * 0.016,
                finger: 0,
                x: 0.5,
                y: 0.5,
            })
            .collect();
        // 先收集目标（避免借住 &self.rooms 时再可变借 self 计数）。
        let targets: Vec<(String, i32)> = self
            .rooms
            .iter()
            .filter_map(|room| {
                room.member_ids
                    .first()
                    .map(|member| (room.room_id.clone(), *member))
            })
            .collect();
        let mut ok = 0u64;
        let mut err = 0u64;
        for (room_id, member) in targets {
            match self
                .state
                .room_commands
                .add_touches(&room_id, member, &touches)
                .await
            {
                Ok(_) => ok += 1,
                Err(e) => {
                    // telemetry 通道拥塞等——计错误但继续。
                    err += 1;
                    tracing::trace!(room = &room_id, %e, "bench add_touches failed");
                }
            }
        }
        self.total_commands += ok;
        self.errors += err;
    }

    /// 轮换超龄房间：close + recreate，重置内存并产生真实生命周期负载。
    async fn recycle_rooms(&mut self) {
        let now = Instant::now();
        let mut to_recycle = Vec::new();
        for (i, room) in self.rooms.iter().enumerate() {
            if now.duration_since(room.created_at) >= ROOM_LIFETIME {
                to_recycle.push(i);
            }
        }
        // 逆序回收避免索引漂移。
        for i in to_recycle.into_iter().rev() {
            let room = self.rooms.remove(i);
            self.close_room(&room.room_id).await;
            if let Err(e) = self.add_room().await {
                self.errors += 1;
                self.last_error = Some(e.clone());
                tracing::warn!(%e, "bench recycle add_room failed");
            }
        }
    }

    // ── 负载管理 ─────────────────────────────────────────────────

    async fn manage_fixed(&mut self) {
        while self.rooms.len() < self.params.max_playing_rooms as usize {
            if let Err(e) = self.add_room().await {
                self.errors += 1;
                self.last_error = Some(e.clone());
                tracing::warn!(%e, "bench add_room failed");
                break;
            }
        }
    }

    async fn manage_ramp(&mut self, cpu: f64, ram: u64) {
        let at_cap = (self.params.max_cpu_pct > 0.0 && cpu >= self.params.max_cpu_pct)
            || (self.params.max_ram_bytes > 0 && ram >= self.params.max_ram_bytes);
        if at_cap {
            if self.ramp_reached.is_none() {
                self.ramp_reached = Some(RampReached {
                    cpu_pct: cpu,
                    ram_bytes: ram,
                    sessions: self.sessions.len() as u32,
                    playing_rooms: self.rooms.len() as u32,
                    commands_per_sec: self.current_rate(),
                });
            }
            return; // 触顶：维持当前负载
        }
        // 未触顶：几何加速加压（房间越多，每 tick 加得越多），成员自动创建会话。
        let step = (self.rooms.len() / 8 + 1).min(RAMP_MAX_ROOMS);
        for _ in 0..step {
            if self.rooms.len() >= RAMP_MAX_ROOMS {
                break;
            }
            if let Err(e) = self.add_room().await {
                self.errors += 1;
                self.last_error = Some(e.clone());
                tracing::warn!(%e, "bench ramp add_room failed");
                break;
            }
        }
    }

    fn current_rate(&self) -> f64 {
        let elapsed = self.started.elapsed().as_secs_f64().max(1.0);
        self.total_commands as f64 / elapsed
    }

    // ── 采样 / 状态 ──────────────────────────────────────────────

    fn record_sample(&mut self, cpu: f64, ram: u64) {
        self.sample_count += 1;
        self.session_sum += self.sessions.len() as u64;
        self.room_sum += self.rooms.len() as u64;
        self.cpu_sum += cpu;
        self.ram_sum += ram;
        self.peak_sessions = self.peak_sessions.max(self.sessions.len() as u32);
        self.peak_rooms = self.peak_rooms.max(self.rooms.len() as u32);
        self.peak_cpu = self.peak_cpu.max(cpu);
        self.peak_ram = self.peak_ram.max(ram);
    }

    // ── 主循环 ───────────────────────────────────────────────────

    /// 运行 harness，返回报告。
    pub async fn run(&mut self) -> BenchmarkReport {
        let started = self.started;
        let deadline = self.params.duration.map(|d| started + d);

        loop {
            let cpu = self.sampler.sample_pct();
            let ram = read_rss_bytes();
            self.record_sample(cpu, ram);

            match self.params.mode {
                BenchmarkMode::Fixed => self.manage_fixed().await,
                BenchmarkMode::Ramp => self.manage_ramp(cpu, ram).await,
            }
            self.pump_touches().await;
            self.recycle_rooms().await;

            let elapsed = started.elapsed();
            let progress = match self.params.duration {
                Some(d) if !d.is_zero() => Some((elapsed.as_secs(), d.as_secs())),
                _ => None,
            };
            let mut status_text = format!(
                "会话 {} 游玩房间 {} CPU {:.1}% RAM {}MB 速率 {:.0} cmd/s",
                self.sessions.len(),
                self.rooms.len(),
                cpu,
                ram / 1024 / 1024,
                self.current_rate(),
            );
            if let Some(err) = &self.last_error {
                status_text.push_str(&format!(" 错误: {err}"));
            }
            self.status.update(status_text, progress);

            let cancelled = self.status.is_cancelled();
            let timed_out = deadline.map(|d| Instant::now() >= d).unwrap_or(false);
            if cancelled || timed_out {
                self.aborted = cancelled;
                self.abort_reason = if cancelled {
                    Some("x 键取消".to_string())
                } else {
                    None
                };
                break;
            }
            tokio::time::sleep(TICK).await;
        }

        self.teardown().await;
        let environment = crate::benchmark::environment::EnvironmentSnapshot::capture().await;
        self.build_report(started, environment)
    }

    // ── 收尾 ─────────────────────────────────────────────────────

    async fn teardown(&mut self) {
        let rooms: Vec<String> = self.rooms.drain(..).map(|r| r.room_id).collect();
        for id in rooms {
            self.close_room(&id).await;
        }
        let users: Vec<i32> = self.sessions.drain(..).map(|s| s.user_id).collect();
        if !users.is_empty() {
            let mut guard = self.state.users.write().await;
            for id in users {
                guard.remove(&id);
            }
        }
    }

    fn build_report(
        &mut self,
        started: Instant,
        environment: crate::benchmark::environment::EnvironmentSnapshot,
    ) -> BenchmarkReport {
        let duration_secs = started.elapsed().as_secs().max(1);
        let mut report = BenchmarkReport::new(
            format!("Phira-mp+ benchmark ({})", self.params.mode.as_str()),
            environment,
            self.params.clone(),
        );
        report.summary.duration_secs = duration_secs;
        report.summary.total_commands = self.total_commands;
        report.summary.avg_commands_per_sec = self.total_commands as f64 / duration_secs as f64;
        report.summary.peak_commands_per_sec = self.current_rate();
        report.peak_sessions = self.peak_sessions;
        report.avg_sessions = if self.sample_count == 0 {
            0.0
        } else {
            self.session_sum as f64 / self.sample_count as f64
        };
        report.peak_playing_rooms = self.peak_rooms;
        report.avg_playing_rooms = if self.sample_count == 0 {
            0.0
        } else {
            self.room_sum as f64 / self.sample_count as f64
        };
        report.cpu_avg_pct = if self.sample_count == 0 {
            0.0
        } else {
            self.cpu_sum / self.sample_count as f64
        };
        report.cpu_peak_pct = self.peak_cpu;
        report.ram_avg_bytes = if self.sample_count == 0 {
            0
        } else {
            self.ram_sum / self.sample_count
        };
        report.ram_peak_bytes = self.peak_ram;
        report.ramp = self.ramp_reached.clone();
        report.errors_total = self.errors;
        report.aborted = self.aborted;
        report.abort_reason = self.abort_reason.clone();
        report.mark_finished();
        report
    }
}
