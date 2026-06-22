//! 数据库后端
//!
//! PostgreSQL 实现（--features postgres）和内存实现（开发回退）。

use phira_mp_plus_server_api::{DatabaseHandle, DbResult};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ── 内存数据库（开发回退，数据不持久化） ──

pub struct JsonDatabase {
    stores: Arc<Mutex<HashMap<String, Vec<HashMap<String, Value>>>>>,
    _path: Option<PathBuf>,
}

impl JsonDatabase {
    pub fn new_memory() -> Self {
        Self { stores: Arc::new(Mutex::new(HashMap::new())), _path: None }
    }

    pub fn into_handle(self) -> DatabaseHandle {
        let stores = self.stores.clone();
        DatabaseHandle::new(move |sql, _params| {
            let mut stores = stores.lock().map_err(|e| e.to_string())?;
            let upper = sql.trim().to_uppercase();

            if upper.contains("CREATE TABLE") {
                // 解析表名并创建空集合
                let table = sql.split(|c: char| c == '(' || c == ' ' || c == ';')
                    .filter(|s| !s.is_empty() && !["CREATE", "TABLE", "IF", "NOT", "EXISTS"].contains(&s.trim()))
                    .next().unwrap_or("t");
                stores.entry(table.to_string()).or_insert_with(Vec::new);
                Ok(DbResult { rows: vec![], columns: vec![], rows_affected: 0 })
            } else if upper.contains("INSERT") {
                let table = sql.split_whitespace().nth(2).unwrap_or("t");
                let phira_id = _params.first().and_then(|v| v.as_i64()).unwrap_or(0);
                let rows = stores.entry(table.to_string()).or_insert_with(Vec::new);
                if let Some(existing) = rows.iter_mut().find(|r| r.get("phira_id").and_then(|v| v.as_i64()) == Some(phira_id)) {
                    existing.insert("last_seen".into(), Value::String("now".into()));
                    if let Some(n) = existing.get("play_count").and_then(|v| v.as_i64()) {
                        existing.insert("play_count".into(), Value::Number((n + 1).into()));
                    }
                } else {
                    rows.push(HashMap::from([
                        ("phira_id".into(), Value::Number(phira_id.into())),
                        ("first_seen".into(), Value::String("now".into())),
                        ("last_seen".into(), Value::String("now".into())),
                        ("play_count".into(), Value::Number(1.into())),
                    ]));
                }
                Ok(DbResult { rows: vec![], columns: vec![], rows_affected: 1 })
            } else if upper.contains("COUNT") {
                let table = sql.split_whitespace().nth(3).unwrap_or("t");
                let rows = stores.get(table).map(|r| r.len()).unwrap_or(0);
                Ok(DbResult {
                    rows: vec![vec![Value::Number(rows.into())]],
                    columns: vec!["cnt".into()],
                    rows_affected: rows as u64,
                })
            } else if upper.contains("SELECT") {
                let table = sql.split_whitespace().nth(3).unwrap_or("t");
                let store = stores.get(table).cloned().unwrap_or_default();
                // 排序
                let mut data = store;
                data.sort_by(|a, b| b.get("last_seen").and_then(|v| v.as_str()).unwrap_or("")
                    .cmp(a.get("last_seen").and_then(|v| v.as_str()).unwrap_or("")));
                // LIMIT / OFFSET
                let page_size = 20usize;
                let rows: Vec<Vec<Value>> = data.iter().take(page_size).map(|r| {
                    vec![
                        r.get("phira_id").cloned().unwrap_or(Value::Null),
                        r.get("first_seen").cloned().unwrap_or(Value::Null),
                        r.get("last_seen").cloned().unwrap_or(Value::Null),
                        r.get("play_count").cloned().unwrap_or(Value::Null),
                    ]
                }).collect();
                Ok(DbResult {
                    rows,
                    columns: vec!["phira_id".into(), "first_seen".into(), "last_seen".into(), "play_count".into()],
                    rows_affected: data.len() as u64,
                })
            } else {
                Ok(DbResult { rows: vec![], columns: vec![], rows_affected: 0 })
            }
        })
    }
}

// ── PostgreSQL（--features postgres） ──

#[cfg(feature = "postgres")]
pub struct PgDatabase {
    pool: Arc<sqlx::PgPool>,
}

#[cfg(feature = "postgres")]
impl PgDatabase {
    pub async fn new(database_url: &str) -> Result<Self, String> {
        use sqlx::Executor;
        let pool = sqlx::PgPool::connect(database_url)
            .await.map_err(|e| format!("PostgreSQL connect: {e}"))?;
        pool.execute("CREATE SCHEMA IF NOT EXISTS data")
            .await.map_err(|e| format!("create schema: {e}"))?;
        Ok(Self { pool: Arc::new(pool) })
    }

    pub fn into_handle(self) -> DatabaseHandle {
        let pool = self.pool;
        DatabaseHandle::new(move |sql, _params| {
            let handle = tokio::runtime::Handle::current();
            let pool = pool.clone();
            let sql = sql.to_string();
            handle.block_on(async move {
                use sqlx::Row;
                let upper = sql.trim().to_uppercase();
                let is_select = upper.starts_with("SELECT") || upper.starts_with("WITH");
                if is_select {
                    let rows = sqlx::query(&sql).fetch_all(&*pool).await
                        .map_err(|e| format!("query: {e}"))?;
                    let columns: Vec<String> = if rows.is_empty() { vec![] } else {
                        (0..rows[0].len()).map(|i| rows[0].column(i).name().to_string()).collect()
                    };
                    let data: Vec<Vec<Value>> = rows.iter().map(|row| {
                        (0..columns.len()).map(|i| {
                            row.try_get::<i64, _>(i).ok().map(|v| Value::Number(v.into()))
                                .or_else(|| row.try_get::<f64, _>(i).ok().map(Value::from))
                                .or_else(|| row.try_get::<String, _>(i).ok().map(Value::String))
                                .or_else(|| row.try_get::<bool, _>(i).ok().map(Value::Bool))
                                .unwrap_or(Value::Null)
                        }).collect()
                    }).collect();
                    Ok(DbResult { rows: data, columns, rows_affected: rows.len() as u64 })
                } else {
                    let r = sqlx::query(&sql).execute(&*pool).await
                        .map_err(|e| format!("exec: {e}"))?;
                    Ok(DbResult { rows: vec![], columns: vec![], rows_affected: r.rows_affected() })
                }
            })
        })
    }
}
