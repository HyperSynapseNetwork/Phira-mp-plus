//! Server benchmark types and helpers.
//!
//! Extracted from the original `server.rs`.

use super::config::{normalize_phira_api_endpoint, PlusConfig};
use serde::{Deserialize, Serialize};
use tracing::warn;

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
