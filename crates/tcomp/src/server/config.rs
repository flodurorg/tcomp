use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub public_url: String,
    pub web_dir: String,
    pub ended_ttl: Duration,
    pub stale_ttl: Duration,
    pub scrollback: usize,
    pub history_bytes: usize,
    /// Reserved for the auth seam; see auth.rs.
    #[allow(dead_code)]
    pub token: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            bind: env_or("TCOMP_BIND", "0.0.0.0:8080"),
            public_url: env_or("TCOMP_PUBLIC_URL", "http://localhost:8080")
                .trim_end_matches('/')
                .to_string(),
            web_dir: env_or("TCOMP_WEB_DIR", "web"),
            ended_ttl: Duration::from_secs(env_num("TCOMP_ENDED_TTL", 60)),
            stale_ttl: Duration::from_secs(env_num("TCOMP_STALE_TTL", 4 * 60 * 60)),
            scrollback: env_num("TCOMP_SCROLLBACK", 2000) as usize,
            history_bytes: env_num("TCOMP_HISTORY_BYTES", 512 * 1024) as usize,
            token: std::env::var("TCOMP_TOKEN").ok().filter(|t| !t.is_empty()),
        }
    }

    pub fn session_url(&self, id: &str) -> String {
        format!("{}/s/{}", self.public_url, id)
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_num(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
