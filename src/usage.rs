use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageCounter {
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStats {
    #[serde(default)]
    pub total: UsageCounter,
    #[serde(default)]
    pub by_day: BTreeMap<String, UsageCounter>,
    #[serde(default)]
    pub by_model: BTreeMap<String, UsageCounter>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub model_key: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub fn load(path: &Path) -> Result<UsageStats> {
    if !path.exists() {
        return Ok(UsageStats::default());
    }

    let raw = fs::read_to_string(path)
        .with_context(|| format!("读取用量文件失败: {}", path.display()))?;
    let stats = serde_json::from_str::<UsageStats>(&raw)
        .with_context(|| format!("解析用量文件失败: {}", path.display()))?;
    Ok(stats)
}

pub fn save(path: &Path, stats: &UsageStats) -> Result<()> {
    let raw = serde_json::to_string_pretty(stats)?;
    fs::write(path, format!("{raw}\n"))
        .with_context(|| format!("写入用量文件失败: {}", path.display()))?;
    Ok(())
}

pub fn record(path: &Path, event: &UsageEvent) -> Result<()> {
    let mut stats = load(path).unwrap_or_default();

    add_counter(&mut stats.total, event);

    let day_key = Local::now().format("%Y-%m-%d").to_string();
    let day = stats.by_day.entry(day_key).or_default();
    add_counter(day, event);

    let model = stats.by_model.entry(event.model_key.clone()).or_default();
    add_counter(model, event);

    stats.updated_at = Some(Local::now().to_rfc3339());
    save(path, &stats)?;
    Ok(())
}

fn add_counter(counter: &mut UsageCounter, event: &UsageEvent) {
    counter.requests += 1;
    counter.input_tokens += event.input_tokens;
    counter.output_tokens += event.output_tokens;
}
