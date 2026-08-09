//! CLI 运行期状态：命令执行时禁用输入框 + 输入框上方显示状态矩形。
//!
//! 长任务命令（如 benchmark）锁定状态 → TUI 渲染状态矩形并禁用输入，
//! 按取消热键（默认 x）请求取消；命令完成/取消后解锁恢复输入。
//! 该句柄挂在 `PlusServerState.cli_status`，内置命令与插件命令（经
//! `CliCommand.handler` 传入）都可使用。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

/// TUI 每帧渲染用的状态快照。
#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    /// 状态矩形标题（如 "benchmark"）
    pub title: String,
    /// 状态矩形正文（命令自定义文字）
    pub text: String,
    /// 进度条 (current, total)，None 表示不确定进度
    pub progress: Option<(u64, u64)>,
    /// 取消热键（如 'x'）
    pub cancel_hotkey: char,
}

/// 锁定期内的活动状态。
struct ActiveStatus {
    title: String,
    text: String,
    progress: Option<(u64, u64)>,
    cancel_hotkey: char,
}

/// 共享运行期状态句柄。
pub struct CliStatus {
    inner: RwLock<Option<ActiveStatus>>,
    cancel_requested: AtomicBool,
}

impl CliStatus {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(None),
            cancel_requested: AtomicBool::new(false),
        }
    }

    /// 锁定输入并显示状态矩形。
    pub fn lock(
        &self,
        title: impl Into<String>,
        text: impl Into<String>,
        cancel_hotkey: char,
    ) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        self.cancel_requested.store(false, Ordering::Relaxed);
        *guard = Some(ActiveStatus {
            title: title.into(),
            text: text.into(),
            progress: None,
            cancel_hotkey,
        });
    }

    /// 更新状态文字与进度（未锁定时 no-op）。
    pub fn update(&self, text: impl Into<String>, progress: Option<(u64, u64)>) {
        if let Ok(mut guard) = self.inner.write() {
            if let Some(active) = guard.as_mut() {
                active.text = text.into();
                active.progress = progress;
            }
        }
    }

    /// 输入是否被锁定（命令运行中）。
    pub fn is_locked(&self) -> bool {
        self.inner.read().map(|g| g.is_some()).unwrap_or(false)
    }

    /// 当前状态快照（未锁定时 None）。
    pub fn snapshot(&self) -> Option<StatusSnapshot> {
        self.inner
            .read()
            .ok()?
            .as_ref()
            .map(|a| StatusSnapshot {
                title: a.title.clone(),
                text: a.text.clone(),
                progress: a.progress,
                cancel_hotkey: a.cancel_hotkey,
            })
    }

    /// 请求取消（TUI 按热键时调用）。
    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::Relaxed);
    }

    /// 是否已请求取消。
    pub fn is_cancelled(&self) -> bool {
        self.cancel_requested.load(Ordering::Relaxed)
    }

    /// 解锁恢复输入。
    pub fn unlock(&self) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = None;
        self.cancel_requested.store(false, Ordering::Relaxed);
    }
}

impl Default for CliStatus {
    fn default() -> Self {
        Self::new()
    }
}

/// 运行期守卫：Drop 时自动解锁。async handler 用它保证任意退出路径
/// （含 panic）都恢复输入，避免输入框永久卡死。
pub struct CliStatusGuard {
    status: Arc<CliStatus>,
}

impl CliStatusGuard {
    pub fn new(
        status: &Arc<CliStatus>,
        title: impl Into<String>,
        text: impl Into<String>,
        cancel_hotkey: char,
    ) -> Self {
        status.lock(title, text, cancel_hotkey);
        Self {
            status: Arc::clone(status),
        }
    }
}

impl Drop for CliStatusGuard {
    fn drop(&mut self) {
        self.status.unlock();
    }
}
