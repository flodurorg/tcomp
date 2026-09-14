use anyhow::{bail, Context, Result};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub public_url: String,
    pub web_dir: String,
    pub ended_ttl: Duration,
    pub stale_ttl: Duration,
    pub producer_timeout: Duration,
    pub scrollback: usize,
    pub history_bytes: usize,
    pub token: Option<String>,
    pub token_in_links: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            bind: env_or("TCOMP_BIND", "0.0.0.0:8080"),
            public_url: normalize_public_url(&env_or("TCOMP_PUBLIC_URL", "")),
            web_dir: web_dir_from_env(),
            ended_ttl: Duration::from_secs(env_num("TCOMP_ENDED_TTL", 60)),
            stale_ttl: Duration::from_secs(env_num("TCOMP_STALE_TTL", 4 * 60 * 60)),
            producer_timeout: Duration::from_secs(env_num("TCOMP_PRODUCER_TIMEOUT", 60).max(3)),
            scrollback: env_num("TCOMP_SCROLLBACK", 2000) as usize,
            history_bytes: env_num("TCOMP_HISTORY_BYTES", 512 * 1024) as usize,
            token: token_from_env()?,
            token_in_links: false,
        })
    }

    pub fn producer_ping(&self) -> Duration {
        self.producer_timeout / 3
    }

    pub fn session_url(&self, id: &str) -> String {
        match self.token.as_deref().filter(|_| self.token_in_links) {
            Some(token) => format!("{}/s/{}?token={}", self.public_url, id, token),
            None => format!("{}/s/{}", self.public_url, id),
        }
    }
}

pub fn web_dir_from_env() -> String {
    env_or("TCOMP_WEB_DIR", "web")
}

pub fn token_from_env() -> Result<Option<String>> {
    resolve_token(
        std::env::var("TCOMP_TOKEN").ok(),
        std::env::var("TCOMP_TOKEN_FILE").ok(),
    )
}

/// `--token` and `--token-file` are two ways to say the same thing, so taking
/// both is a mistake rather than a precedence question.
pub fn resolve_token(inline: Option<String>, path: Option<String>) -> Result<Option<String>> {
    let inline = inline.filter(|t| !t.is_empty());
    let path = path.filter(|p| !p.is_empty());
    match (inline, path) {
        (Some(_), Some(_)) => bail!("pass a token or a token file, not both"),
        (Some(token), None) => Ok(Some(token)),
        (None, Some(path)) => read_token_file(&path).map(Some),
        (None, None) => Ok(None),
    }
}

pub fn read_token_file(path: &str) -> Result<String> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading token file {path}"))?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        bail!("token file {path} holds no token");
    }
    Ok(token)
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
    use super::{read_token_file, resolve_token, Config, Duration};

    fn token_file(contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tcomp-token-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn config(token: Option<&str>, token_in_links: bool) -> Config {
        Config {
            public_url: "http://relay.test".into(),
            token: token.map(str::to_string),
            token_in_links,
            ..Config::from_env().unwrap()
        }
    }

    #[test]
    fn a_live_producer_gets_two_pings_before_it_counts_as_gone() {
        let config = Config {
            producer_timeout: Duration::from_secs(60),
            ..Config::from_env().unwrap()
        };
        assert!(config.producer_ping() * 2 < config.producer_timeout);
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

    #[test]
    fn a_token_file_stands_in_for_an_inline_token() {
        let path = token_file("s3cret");
        let resolved = resolve_token(None, Some(path.to_string_lossy().into_owned())).unwrap();
        assert_eq!(resolved.as_deref(), Some("s3cret"));
    }

    #[test]
    fn the_trailing_newline_a_file_picks_up_is_not_part_of_the_token() {
        let path = token_file("s3cret\n");
        assert_eq!(read_token_file(&path.to_string_lossy()).unwrap(), "s3cret");
    }

    #[test]
    fn an_empty_file_is_a_missing_token_rather_than_an_open_relay() {
        let path = token_file("   \n");
        assert!(read_token_file(&path.to_string_lossy()).is_err());
    }

    #[test]
    fn an_unreadable_token_file_is_never_silently_skipped() {
        let missing = std::env::temp_dir().join("tcomp-token-does-not-exist");
        let _ = std::fs::remove_file(&missing);
        assert!(resolve_token(None, Some(missing.to_string_lossy().into_owned())).is_err());
    }

    #[test]
    fn naming_both_a_token_and_a_token_file_is_refused() {
        let path = token_file("s3cret");
        assert!(resolve_token(
            Some("inline".into()),
            Some(path.to_string_lossy().into_owned())
        )
        .is_err());
    }

    #[test]
    fn an_empty_value_counts_as_unset_on_either_side() {
        assert_eq!(resolve_token(Some(String::new()), None).unwrap(), None);
        assert_eq!(resolve_token(None, Some(String::new())).unwrap(), None);
        assert_eq!(
            resolve_token(Some("s3cret".into()), Some(String::new())).unwrap(),
            Some("s3cret".into())
        );
    }
}
