use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Lifetime stats. Field names stay camelCase for compatibility with the
/// stats.json written by earlier versions.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersistedStats {
    pub high_score: u64,
    pub best_streak: u32,
    pub games_played: u64,
    pub total_rounds: u64,
    pub total_correct: u64,
}

/// One session's tally.
#[derive(Debug, Default)]
pub struct SessionStats {
    pub score: u64,
    pub rounds: u64,
    pub correct: u64,
    pub streak: u32,
    pub best_streak: u32,
    /// Per-round outcomes in play order (true = correct).
    pub history: Vec<bool>,
}

fn stats_path() -> PathBuf {
    crate::config::config_dir().join("stats.json")
}

pub fn load() -> PersistedStats {
    fs::read_to_string(stats_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(stats: &PersistedStats) {
    // losing a high score is sad, but not fatal
    let _ = fs::create_dir_all(crate::config::config_dir());
    let json = serde_json::to_string_pretty(stats).expect("stats serialize");
    let _ = fs::write(stats_path(), json + "\n");
}
