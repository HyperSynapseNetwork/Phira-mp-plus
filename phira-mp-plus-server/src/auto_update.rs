//! 自动更新：检查 GitHub Release、下载新版本、替换可执行文件并尝试重启。
//!
//! 默认关闭，需在配置 `auto_update.enabled: true` 显式开启。开启后由
//! `run_checker` 在启动时与按间隔检查；有新版本且满足空闲条件才自动应用。
//! 检查失败只记 warn（静默降级），不影响服务器启动与运行。

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::server::config::AutoUpdateConfig;
use crate::server::PlusServerState;

/// 已成功应用过的 Release tag（进程级），避免同一版本反复下载替换。
static LAST_APPLIED_TAG: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 更新完成标记文件：替换二进制后写入目标版本号；新进程启动时据此输出
/// "更新完成"一次性提示（校验一致后删除）。
const UPDATED_VERSION_MARKER: &str = "data/update/updated-version";

/// 启动时调用：检测更新完成标记。
///
/// - 标记版本与当前 `CARGO_PKG_VERSION` 一致 → 返回"更新完成"提示，调用方
///   用 `tracing::info` 输出，随后删除标记（一次性，不重复）。
/// - 标记版本不一致（旧版残留/下载失败后的残留）→ 记 warn 并清除。
pub fn check_updated_version_notice() -> Option<String> {
    let content = std::fs::read_to_string(UPDATED_VERSION_MARKER).ok()?;
    let _ = std::fs::remove_file(UPDATED_VERSION_MARKER);
    let marker_version = content.trim();
    if marker_version == env!("CARGO_PKG_VERSION") {
        Some(format!("更新完成：已更新到 v{marker_version}"))
    } else {
        warn!(
            target: "auto_update",
            marker = marker_version,
            current = env!("CARGO_PKG_VERSION"),
            "更新标记版本与当前版本不一致，已清除"
        );
        None
    }
}

/// 版本对比结果信息。
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_tag: String,
    pub release_url: String,
    pub published_at: String,
    /// 发布资产列表（apply 内部用于挑选当前平台资产）。
    pub(crate) assets: Vec<ReleaseAsset>,
}

/// GitHub Release 资产。
#[derive(Debug, Clone)]
pub(crate) struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
}

/// 解析版本号：去掉 `v` 前缀，按 `.` 拆分为数字段。
/// 支持任意段数（如 `0.4.190`、`v0.5.2091`）；任何一段非纯数字返回 `None`
/// （调用方视为无更新，不崩溃）。
pub fn parse_version(tag: &str) -> Option<Vec<u64>> {
    let s = tag.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = Vec::new();
    for seg in s.split('.') {
        match seg.parse::<u64>() {
            Ok(n) => parts.push(n),
            Err(_) => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts)
}

/// 按数字段逐段比较（非字典序）：`latest` 大于 `current` 才为真。
/// 段数不足时按 0 补齐（`0.5` == `0.5.0`）。
pub fn is_newer(latest: &[u64], current: &[u64]) -> bool {
    let max_len = latest.len().max(current.len());
    for i in 0..max_len {
        let l = latest.get(i).copied().unwrap_or(0);
        let c = current.get(i).copied().unwrap_or(0);
        if l != c {
            return l > c;
        }
    }
    false
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    published_at: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// 查询 GitHub 最新 Release 并与当前版本（CARGO_PKG_VERSION）对比。
pub async fn check(repo: &str) -> Result<UpdateInfo> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let resp = client
        .get(&url)
        // GitHub API 要求显式 User-Agent，否则返回 403。
        .header(
            reqwest::header::USER_AGENT,
            format!("phira-mp-plus-server/{}", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("GitHub API 返回 {}（{url}）", resp.status());
    }
    let release: GithubRelease = resp.json().await?;

    let current_tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    let latest = parse_version(&release.tag_name);
    let current = parse_version(&current_tag);
    let update_available = match (latest.as_deref(), current.as_deref()) {
        (Some(latest), Some(current)) => is_newer(latest, current),
        // 无法解析的 tag 视为无更新，仅告警，绝不崩溃。
        (None, _) => {
            warn!(
                tag = %release.tag_name,
                "无法解析最新 Release 版本号，跳过更新判定"
            );
            false
        }
        (Some(_), None) => {
            warn!(
                current = %current_tag,
                "无法解析当前版本号，跳过更新判定"
            );
            false
        }
    };

    Ok(UpdateInfo {
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        latest_version: release.tag_name.clone(),
        update_available,
        release_tag: release.tag_name,
        release_url: release.html_url,
        published_at: release.published_at,
        assets: release
            .assets
            .into_iter()
            .map(|a| ReleaseAsset {
                name: a.name,
                download_url: a.browser_download_url,
            })
            .collect(),
    })
}

/// 选择匹配当前平台的发布资产；找不到特定平台资产时回退到通用名
/// `phira-mp-plus-server`。
fn pick_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let needles: &[&str] = &["linux-glibc", "x86_64-glibc", "amd64-linux"];
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    let needles: &[&str] = &["linux-arm64-glibc", "aarch64-glibc", "arm64-linux"];
    #[cfg(not(any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux")
    )))]
    let needles: &[&str] = &["linux"];
    assets
        .iter()
        .find(|a| needles.iter().any(|n| a.name.contains(n)))
        .or_else(|| {
            assets
                .iter()
                .find(|a| a.name.contains("phira-mp-plus-server"))
        })
}

/// 下载发布资产到内存（大文件直接读入内存，用于替换自身）。
async fn download_asset(download_url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;
    let resp = client
        .get(download_url)
        .header(
            reqwest::header::USER_AGENT,
            format!("phira-mp-plus-server/{}", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("下载更新包失败: {}", resp.status());
    }
    let bytes = resp.bytes().await?.to_vec();
    if bytes.is_empty() {
        bail!("下载的更新包为空");
    }
    Ok(bytes)
}

/// 替换自身可执行文件。
///
/// Linux 下运行中的二进制可被覆盖（unlink+rename）。优先原子 rename；
/// 跨文件系统（EXDEV，如 data/ 为挂载卷而可执行文件在容器镜像层）时
/// 回退为复制到目标目录后再次 rename，保证替换原子性。
fn replace_executable(tmp_path: &str, target: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        match std::fs::rename(tmp_path, target) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                let target_dir = target
                    .parent()
                    .ok_or_else(|| anyhow!("可执行文件目录不可用"))?;
                let fallback = target_dir.join(format!(".pmp-update-{}", std::process::id()));
                std::fs::copy(tmp_path, &fallback)
                    .map_err(|e| anyhow!("复制更新文件失败（{}）: {e}", fallback.display()))?;
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(0o755))?;
                }
                std::fs::rename(&fallback, target)
                    .map_err(|e| anyhow!("替换可执行文件失败（{}）: {e}", target.display()))
            }
            Err(e) => Err(anyhow!("替换可执行文件失败（{}）: {e}", target.display())),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (tmp_path, target);
        Err(anyhow!("自动更新仅支持 Linux/Unix"))
    }
}

/// 手动/自动触发更新流程。
///
/// - 先查最新版本；无新版返回"已是最新"。
/// - `force=false` 时检查在线玩家（id>0）与空闲时长（min_idle_minutes）。
/// - 下载匹配平台的资产 → 写到 `data/update/` → 替换当前可执行文件 →
///   以相同参数 spawn 新进程接管。
pub async fn apply(state: &Arc<PlusServerState>, force: bool) -> Result<String> {
    let repo = state.live_config.read().await.auto_update.github_repo.clone();
    let info = check(&repo).await?;
    if !info.update_available {
        return Ok(format!("已是最新版本（v{}）", info.current_version));
    }
    if !force {
        // 同一版本已应用过（本次进程内），等待重启生效即可，避免反复下载替换。
        // std MutexGuard 不能跨 await 存活（!Send），故用块作用域提前释放。
        {
            let last = LAST_APPLIED_TAG.lock().unwrap_or_else(|e| e.into_inner());
            if last.as_deref() == Some(info.release_tag.as_str()) {
                return Ok(format!("v{} 已应用，等待服务重启生效", info.latest_version));
            }
        }
        // 有在线玩家或最近下线未满 min_idle_minutes 时拒绝。
        let players_online = {
            let users = state.users.read().await;
            users.values().filter(|u| u.id > 0).count()
        };
        if players_online > 0 {
            return Ok(format!("有玩家在线（{players_online} 人），暂不更新"));
        }
        let (idle_minutes, offline_elapsed) = {
            let lc = state.live_config.read().await;
            let idle_minutes = lc.auto_update.min_idle_minutes;
            let elapsed = state.last_all_offline_at.lock().await.elapsed();
            (idle_minutes, elapsed)
        };
        let idle = Duration::from_secs(idle_minutes * 60);
        if offline_elapsed < idle {
            let remain = (idle - offline_elapsed).as_secs();
            return Ok(format!(
                "最近下线未满 {idle_minutes} 分钟（还需约 {remain} 秒），暂不更新"
            ));
        }
    }

    let asset = pick_asset(&info.assets)
        .ok_or_else(|| anyhow!("未找到匹配当前平台的发布资产"))?;

    let bytes = download_asset(&asset.download_url).await?;

    std::fs::create_dir_all("data/update")
        .map_err(|e| anyhow!("创建 data/update 失败: {e}"))?;
    let tmp_path = format!("data/update/{}", asset.name);
    std::fs::write(&tmp_path, &bytes)
        .map_err(|e| anyhow!("写入更新文件失败（{tmp_path}）: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| anyhow!("设置更新文件权限失败: {e}"))?;
    }

    let current_exe = std::env::current_exe()
        .map_err(|e| anyhow!("无法定位当前可执行文件: {e}"))?;
    replace_executable(&tmp_path, &current_exe)?;

    // 写入更新完成标记：重启后新进程据此输出"更新完成"提示（一次性）。
    // 写入失败不阻塞更新，仅丢失提示。
    let _ = std::fs::write(
        UPDATED_VERSION_MARKER,
        info.latest_version.trim_start_matches('v'),
    );

    // 以相同参数 spawn 新进程接管。旧进程保持运行，待服务管理器（systemd /
    // Docker）重启后即使用已替换的新二进制。
    let args: Vec<String> = std::env::args().skip(1).collect();
    match tokio::process::Command::new(&current_exe).args(&args).spawn() {
        Ok(_child) => info!(target: "auto_update", "已启动新版本进程"),
        Err(e) => warn!(target: "auto_update", "启动新版本进程失败: {e}"),
    }

    *LAST_APPLIED_TAG.lock().unwrap_or_else(|e| e.into_inner()) = Some(info.release_tag.clone());

    Ok(format!(
        "更新完成（v{} → {}），正在重启",
        info.current_version, info.latest_version
    ))
}

/// 后台检查器：启动时检查一次，之后按配置间隔定期检查。
///
/// `enabled`/`check_interval_secs` 每次循环重新读取 `live_config`，
/// 支持 `update auto on|off` 与 `config reload` 运行时切换。
pub async fn run_checker(state: Arc<PlusServerState>, config: AutoUpdateConfig) {
    check_once(&state, config.enabled).await;
    loop {
        let interval_secs = {
            let lc = state.live_config.read().await;
            lc.auto_update.check_interval_secs.max(60)
        };
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        let enabled = state.live_config.read().await.auto_update.enabled;
        check_once(&state, enabled).await;
    }
}

async fn check_once(state: &Arc<PlusServerState>, enabled: bool) {
    let repo = state.live_config.read().await.auto_update.github_repo.clone();
    match check(&repo).await {
        Ok(info) if info.update_available => {
            if enabled {
                info!(
                    target: "auto_update",
                    "检测到新版本 v{} → {}，尝试自动更新",
                    info.current_version,
                    info.latest_version
                );
                match apply(state, false).await {
                    Ok(msg) => info!(target: "auto_update", "{msg}"),
                    Err(e) => warn!(target: "auto_update", "自动更新失败: {e}"),
                }
            } else {
                info!(
                    target: "auto_update",
                    "发现新版本 v{} → {}，可执行 `update apply` 更新",
                    info.current_version,
                    info.latest_version
                );
            }
        }
        Ok(_) => debug!(target: "auto_update", "已是最新版本"),
        Err(e) => warn!(target: "auto_update", "检查更新失败（静默降级）: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_newer, parse_version};

    #[test]
    fn parse_handles_v_prefix_and_variable_segments() {
        assert_eq!(parse_version("v0.5.2091"), Some(vec![0, 5, 2091]));
        assert_eq!(parse_version("0.4.190"), Some(vec![0, 4, 190]));
        assert_eq!(parse_version("v0.5.2091.1"), Some(vec![0, 5, 2091, 1]));
        assert_eq!(parse_version("v1"), Some(vec![1]));
    }

    #[test]
    fn parse_rejects_malformed_tags() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("v"), None);
        assert_eq!(parse_version("release-0.5.1"), None);
        assert_eq!(parse_version("0.5.beta"), None);
        assert_eq!(parse_version("v0..1"), None);
    }

    #[test]
    fn newer_compares_numerically_not_lexicographically() {
        // 字典序 "2091" < "2092" 在此也成立，关键用例是 10 vs 9：
        assert!(is_newer(&[0, 5, 10], &[0, 5, 9]));
        // 段数不等时按 0 补齐：
        assert!(is_newer(&[0, 5, 1], &[0, 5]));
        assert!(!is_newer(&[0, 5], &[0, 5, 1]));
        assert!(!is_newer(&[0, 5, 1], &[0, 5, 1]));
        assert!(!is_newer(&[0, 4, 190], &[0, 5, 2091]));
    }
}
