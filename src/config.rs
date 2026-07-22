use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Player choices persisted between runs.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// GitHub org to pull snippets from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    /// Personal access token, only stored here if the player opted in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("codeguesser")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load() -> Config {
    fs::read_to_string(config_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(config: &Config) -> std::io::Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    let path = config_path();
    let json = serde_json::to_string_pretty(config).expect("config serializes");
    fs::write(&path, json + "\n")?;
    // the config may hold a token — keep it owner-only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
