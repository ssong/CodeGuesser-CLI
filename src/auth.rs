use std::process::Command;

pub struct DetectedToken {
    pub token: String,
    pub source: &'static str,
}

/// Look for a GitHub token in the local environment: env vars first (same
/// precedence gh itself uses), then the gh CLI's stored login. Returns None
/// if neither exists — onboarding decides what to do about that.
pub fn detect_token() -> Option<DetectedToken> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(var) {
            let token = value.trim().to_string();
            if !token.is_empty() {
                return Some(DetectedToken {
                    token,
                    source: "GH_TOKEN / GITHUB_TOKEN env var",
                });
            }
        }
    }

    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if output.status.success() {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !token.is_empty() {
            return Some(DetectedToken {
                token,
                source: "gh CLI login",
            });
        }
    }
    None
}
