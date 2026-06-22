//! PostgreSQL 数据库后端
//!
//! 使用 sqlx 连接 PostgreSQL，通过 DatabaseHandle 提供给插件使用。

use phira_mp_plus_server_api::{DatabaseHandle, DbResult};
use serde_json::Value;
use sqlx::{Column, Row};
use std::sync::Arc;

pub struct PgDatabase {
    pool: Arc<sqlx::PgPool>,
}

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
                let upper = sql.trim().to_uppercase();
                let is_select = upper.starts_with("SELECT") || upper.starts_with("WITH") || sql.contains("COUNT(*)");

                if is_select {
                    let rows = sqlx::query(&sql).fetch_all(&*pool).await
                        .map_err(|e| format!("query error: {e}"))?;
                    let columns: Vec<String> = if rows.is_empty() {
                        vec![]
                    } else {
                        (0..rows[0].len()).map(|i| rows[0].column(i).name().to_string()).collect()
                    };
                    let result_rows: Vec<Vec<Value>> = rows.iter().map(|row| {
                        (0..columns.len()).map(|i| {
                            row.try_get::<i64, _>(i).ok().map(|v| Value::Number(v.into()))
                                .or_else(|| row.try_get::<f64, _>(i).ok().map(Value::from))
                                .or_else(|| row.try_get::<String, _>(i).ok().map(Value::String))
                                .or_else(|| row.try_get::<bool, _>(i).ok().map(Value::Bool))
                                .or_else(|| row.try_get::<Option<String>, _>(i).ok().flatten().map(Value::String))
                                .unwrap_or(Value::Null)
                        }).collect()
                    }).collect();
                    Ok(DbResult { rows: result_rows, columns, rows_affected: rows.len() as u64 })
                } else {
                    let result = sqlx::query(&sql).execute(&*pool).await
                        .map_err(|e| format!("execute error: {e}"))?;
                    Ok(DbResult { rows: vec![], columns: vec![], rows_affected: result.rows_affected() })
                }
            })
        })
    }
}
