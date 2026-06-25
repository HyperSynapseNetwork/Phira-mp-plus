//! Phira-mp+ 数据库模块（PostgreSQL）
//!
//! 可选依赖，启用 `postgres` feature 时编译。
//! 未配置 `database_url` 时自动回退 JSON 文件存储。

use anyhow::Result;

/// 游玩时间记录
#[derive(Debug, Clone)]
pub struct PlaytimeRow {
    pub total_secs: i64,
    pub session_start: Option<i64>,
}

/// 数据库管理器
pub enum DbManager {
    /// PostgreSQL 已连接
    #[cfg(feature = "postgres")]
    Pg(sqlx::PgPool),
    /// 未配置数据库（回退 JSON）
    None,
}

impl DbManager {
    /// 根据配置初始化数据库连接（自动创建数据库）
    pub async fn new(database_url: Option<&str>) -> Self {
        match database_url {
            Some(url) if !url.is_empty() => {
                #[cfg(feature = "postgres")]
                {
                    // 先尝试直接连接
                    match sqlx::PgPool::connect(url).await {
                        Ok(pool) => {
                            tracing::info!("PostgreSQL 已连接");
                            if let Err(e) = init_tables(&pool).await {
                                tracing::warn!("数据库建表失败: {e:?}，回退 JSON");
                                return Self::None;
                            }
                            return Self::Pg(pool);
                        }
                        Err(e) => {
                            let err_str = e.to_string();
                            // 数据库不存在 → 自动创建
                            if err_str.contains("does not exist") || err_str.contains("database") && err_str.contains("not found") {
                                tracing::info!("数据库不存在，尝试自动创建...");
                                // 连接到默认 postgres 数据库来创建目标数据库
                                let default_url = url.rsplitn(2, '/').collect::<Vec<_>>();
                                let base = default_url.get(1).copied().unwrap_or("postgres://postgres:postgres@localhost:5432");
                                let admin_url = format!("{}/postgres", base);
                                if let Ok(admin_pool) = sqlx::PgPool::connect(&admin_url).await {
                                    let db_name = default_url.first().copied().unwrap_or("phira_mp_plus");
                                    let _ = sqlx::query(&format!("CREATE DATABASE \"{}\"", db_name))
                                        .execute(&admin_pool).await;
                                    admin_pool.close().await;
                                    tracing::info!("数据库创建完成，重新连接...");
                                    // 重新连接到目标数据库
                                    if let Ok(new_pool) = sqlx::PgPool::connect(url).await {
                                        if let Err(e) = init_tables(&new_pool).await {
                                            tracing::warn!("数据库建表失败: {e:?}，回退 JSON");
                                            return Self::None;
                                        }
                                        tracing::info!("PostgreSQL 已连接（自动建库）");
                                        return Self::Pg(new_pool);
                                    }
                                }
                            }
                            tracing::warn!("PostgreSQL 连接失败: {e:?}，回退 JSON");
                        }
                    }
                }
                #[cfg(not(feature = "postgres"))]
                tracing::warn!("未启用 postgres feature，回退 JSON");
                Self::None
            }
            _ => Self::None,
        }
    }

    /// 标记用户上线（设置 session_start）
    pub fn set_online_sync(&self, user_id: i32) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let _ = sqlx::query(
                    "INSERT INTO playtime (user_id, total_secs, session_start) VALUES ($1, 0, $2)
                     ON CONFLICT (user_id) DO UPDATE SET session_start = $2"
                )
                .bind(user_id).bind(now)
                .execute(&pool).await;
            });
        }
    }

    /// 标记用户离线（累加本次会话时间）
    pub fn set_offline_sync(&self, user_id: i32) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let _ = sqlx::query(
                    "UPDATE playtime SET total_secs = total_secs + $1 - session_start, session_start = NULL
                     WHERE user_id = $2 AND session_start IS NOT NULL"
                )
                .bind(now).bind(user_id)
                .execute(&pool).await;
            });
        }
    }

    /// 是否使用 PostgreSQL
    pub fn is_active(&self) -> bool {
        false
    }
}

/// 创建数据库表
#[cfg(feature = "postgres")]
async fn init_tables(pool: &sqlx::PgPool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS playtime (
            user_id INTEGER PRIMARY KEY,
            total_secs BIGINT NOT NULL DEFAULT 0,
            session_start BIGINT
        )"
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS room_history (
            id SERIAL PRIMARY KEY,
            user_id INTEGER NOT NULL,
            room_id TEXT NOT NULL,
            room_uuid TEXT NOT NULL,
            joined_at BIGINT NOT NULL
        )"
    )
    .execute(pool)
    .await?;

    tracing::info!("数据库表已就绪");
    Ok(())
}

