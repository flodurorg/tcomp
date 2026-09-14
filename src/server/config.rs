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
    pub token: Option<String>,
    pub token_in_links: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            bind: env_or("TCOMP_BIND", "0.0.0.0:8080"),
            public_url: normalize_public_url(&env_or("TCOMP_PUBLIC_URL", "")),
            web_dir: env_or("TCOMP_WEB_DIR", "web"),
            ended_ttl: Duration::from_secs(env_num("TCOMP_ENDED_TTL", 60)),
            stale_ttl: Duration::from_secs(env_num("TCOMP_STALE_TTL", 4 * 60 * 60)),
            scrollback: env_num("TCOMP_SCROLLBACK", 2000) as usize,
            history_bytes: env_num("TCOMP_HISTORY_BYTES", 512 * 1024) as usize,
            token: std::env::var("TCOMP_TOKEN").ok().filter(|t| !t.is_empty()),
            token_in_links: false,
        }
    }

    pub fn session_url(&self, id: &str) -> String {
        match self.token.as_deref().filter(|_| self.token_in_links) {
            Some(token) => format!("{}/s/{}?token={}", self.public_url, id, token),
            None => format!("{}/s/{}", self.public_url, id),
        }
    }
}

pub fn normalize_public_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
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

#[cfg(test)]
mod tests {
    use super::Config;

    fn config(token: Option<&str>, token_in_links: bool) -> Config {
        Config {
            public_url: "http://relay.test".into(),
            token: token.map(str::to_string),
            token_in_links,
            ..Config::from_env()
        }
    }

    #[test]
    fn a_minted_token_rides_along_in_the_watch_link() {
        assert_eq!(
            config(Some("s3cret"), true).session_url("abc"),
            "http://relay.test/s/abc?token=s3cret"
        );
    }

    #[test]
    fn a_shared_relay_never_prints_its_token() {
        assert_eq!(
            config(Some("s3cret"), false).session_url("abc"),
            "http://relay.test/s/abc"
        );
        assert_eq!(
            config(None, true).session_url("abc"),
            "http://relay.test/s/abc"
        );
    }
}
