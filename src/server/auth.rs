use crate::server::config::Config;
use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::http::{HeaderMap, StatusCode};

pub const COOKIE_NAME: &str = "tcomp_token";

const COOKIE_MAX_AGE: u32 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone)]
pub enum Identity {
    Anonymous,
    Holder,
}

#[derive(Debug)]
pub struct Rejection(pub StatusCode, pub &'static str);

pub enum Access<'a> {
    Index,
    Produce { token: Option<&'a str> },
    View { session: &'a str },
    Write { session: &'a str },
}

impl Access<'_> {
    fn scope(&self) -> &str {
        match self {
            Access::Index => "index",
            Access::Produce { .. } => "produce",
            Access::View { session } | Access::Write { session } => session,
        }
    }
}

pub async fn authorize(
    config: &Config,
    headers: &HeaderMap,
    access: Access<'_>,
) -> Result<Identity, Rejection> {
    let Some(expected) = config.token.as_deref() else {
        return Ok(Identity::Anonymous);
    };
    let presented = match access {
        Access::Produce { token } => token,
        Access::Index | Access::View { .. } | Access::Write { .. } => {
            bearer(headers).or_else(|| cookie(headers, COOKIE_NAME))
        }
    };
    match presented {
        Some(token) if secret_eq(token, expected) => Ok(Identity::Holder),
        presented => {
            tracing::debug!(
                scope = access.scope(),
                presented = presented.is_some(),
                "access denied"
            );
            Err(Rejection(
                StatusCode::UNAUTHORIZED,
                if presented.is_some() {
                    "token is not valid"
                } else {
                    "token required"
                },
            ))
        }
    }
}

/// False whenever the relay is open, so a stray `?token=` never mints a cookie.
pub fn accepts(config: &Config, token: &str) -> bool {
    config
        .token
        .as_deref()
        .is_some_and(|expected| secret_eq(token, expected))
}

pub fn set_cookie(config: &Config, headers: &HeaderMap, token: &str) -> String {
    format!(
        "{COOKIE_NAME}={token}; Path=/; Max-Age={COOKIE_MAX_AGE}; HttpOnly; SameSite=Lax{}",
        if over_https(config, headers) {
            "; Secure"
        } else {
            ""
        }
    )
}

/// Checked at startup, which is why cookie and link builders can interpolate.
pub fn is_well_formed(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

fn over_https(config: &Config, headers: &HeaderMap) -> bool {
    match headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
    {
        Some(value) => value
            .split(',')
            .next()
            .is_some_and(|p| p.trim().eq_ignore_ascii_case("https")),
        None => config.public_url.starts_with("https://"),
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|raw| raw.split(';'))
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

/// Compared without an early exit, so a wrong token leaks no timing signal.
fn secret_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0, |differs, (x, y)| differs | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn config(token: Option<&str>) -> Config {
        Config {
            bind: "127.0.0.1:0".into(),
            public_url: "http://relay.test".into(),
            web_dir: "web".into(),
            ended_ttl: Duration::from_secs(60),
            stale_ttl: Duration::from_secs(4 * 60 * 60),
            producer_timeout: Duration::from_secs(60),
            scrollback: 100,
            history_bytes: 64,
            token: token.map(str::to_string),
            token_in_links: false,
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    async fn allowed(config: &Config, headers: &HeaderMap, access: Access<'_>) -> bool {
        authorize(config, headers, access).await.is_ok()
    }

    #[tokio::test]
    async fn an_untokened_relay_lets_everyone_in() {
        let open = config(None);
        let none = headers(&[]);
        assert!(allowed(&open, &none, Access::Index).await);
        assert!(allowed(&open, &none, Access::View { session: "s1" }).await);
        assert!(allowed(&open, &none, Access::Write { session: "s1" }).await);
        assert!(allowed(&open, &none, Access::Produce { token: None }).await);
    }

    #[tokio::test]
    async fn a_tokened_relay_turns_away_a_browser_with_no_cookie() {
        let guarded = config(Some("s3cret"));
        let none = headers(&[]);
        assert!(!allowed(&guarded, &none, Access::Index).await);
        assert!(!allowed(&guarded, &none, Access::View { session: "s1" }).await);
        assert!(!allowed(&guarded, &none, Access::Write { session: "s1" }).await);
    }

    #[tokio::test]
    async fn the_cookie_admits_a_browser() {
        let guarded = config(Some("s3cret"));
        let cookie = headers(&[("cookie", "tcomp_token=s3cret")]);
        assert!(allowed(&guarded, &cookie, Access::Index).await);
        assert!(allowed(&guarded, &cookie, Access::View { session: "s1" }).await);
        assert!(allowed(&guarded, &cookie, Access::Write { session: "s1" }).await);
    }

    #[tokio::test]
    async fn a_wrong_cookie_is_turned_away() {
        let guarded = config(Some("s3cret"));
        let cookie = headers(&[("cookie", "tcomp_token=guess")]);
        assert!(!allowed(&guarded, &cookie, Access::Index).await);
    }

    #[tokio::test]
    async fn the_cookie_is_found_among_others() {
        let guarded = config(Some("s3cret"));
        for raw in [
            "theme=dark; tcomp_token=s3cret",
            "tcomp_token=s3cret; theme=dark",
            "a=1;tcomp_token=s3cret;b=2",
        ] {
            assert!(
                allowed(&guarded, &headers(&[("cookie", raw)]), Access::Index).await,
                "missed the token in {raw:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_lookalike_cookie_name_does_not_admit() {
        let guarded = config(Some("s3cret"));
        let cookie = headers(&[("cookie", "xtcomp_token=s3cret")]);
        assert!(!allowed(&guarded, &cookie, Access::Index).await);
    }

    #[tokio::test]
    async fn a_bearer_header_admits_a_script() {
        let guarded = config(Some("s3cret"));
        assert!(
            allowed(
                &guarded,
                &headers(&[("authorization", "Bearer s3cret")]),
                Access::Index
            )
            .await
        );
        assert!(
            allowed(
                &guarded,
                &headers(&[("authorization", "bearer s3cret")]),
                Access::Index
            )
            .await
        );
        assert!(
            !allowed(
                &guarded,
                &headers(&[("authorization", "Basic s3cret")]),
                Access::Index
            )
            .await
        );
    }

    #[tokio::test]
    async fn a_producer_is_judged_on_its_hello_not_its_cookies() {
        let guarded = config(Some("s3cret"));
        let cookie = headers(&[("cookie", "tcomp_token=s3cret")]);
        assert!(!allowed(&guarded, &cookie, Access::Produce { token: None }).await);
        assert!(
            allowed(
                &guarded,
                &headers(&[]),
                Access::Produce {
                    token: Some("s3cret")
                }
            )
            .await
        );
    }

    #[tokio::test]
    async fn rejections_are_401_so_the_viewer_can_offer_a_sign_in() {
        let guarded = config(Some("s3cret"));
        let rejection = authorize(&guarded, &headers(&[]), Access::Index)
            .await
            .unwrap_err();
        assert_eq!(rejection.0, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn a_link_token_is_only_accepted_against_a_tokened_relay() {
        assert!(accepts(&config(Some("s3cret")), "s3cret"));
        assert!(!accepts(&config(Some("s3cret")), "guess"));
        assert!(!accepts(&config(None), "s3cret"));
        assert!(!accepts(&config(None), ""));
    }

    #[test]
    fn secrets_compare_by_value_and_length() {
        assert!(secret_eq("s3cret", "s3cret"));
        assert!(!secret_eq("s3cret", "s3cre"));
        assert!(!secret_eq("s3cret", "s3crets"));
        assert!(!secret_eq("s3cret", "S3cret"));
        assert!(secret_eq("", ""));
    }

    #[test]
    fn the_cookie_is_locked_down() {
        let cookie = set_cookie(&config(Some("s3cret")), &headers(&[]), "s3cret");
        assert!(cookie.starts_with("tcomp_token=s3cret;"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Path=/"));
    }

    #[test]
    fn the_cookie_is_marked_secure_only_where_tls_reaches() {
        let plain = config(Some("s3cret"));
        let mut tls = config(Some("s3cret"));
        tls.public_url = "https://relay.test".into();

        assert!(!set_cookie(&plain, &headers(&[]), "s3cret").contains("Secure"));
        assert!(set_cookie(&tls, &headers(&[]), "s3cret").contains("Secure"));

        let proxied = headers(&[("x-forwarded-proto", "https")]);
        assert!(set_cookie(&plain, &proxied, "s3cret").contains("Secure"));
        let chained = headers(&[("x-forwarded-proto", "https, http")]);
        assert!(set_cookie(&plain, &chained, "s3cret").contains("Secure"));
        let plain_proxy = headers(&[("x-forwarded-proto", "http")]);
        assert!(!set_cookie(&tls, &plain_proxy, "s3cret").contains("Secure"));
    }

    #[test]
    fn tokens_that_would_not_survive_a_cookie_or_a_url_are_refused() {
        assert!(is_well_formed("abc123"));
        assert!(is_well_formed("a-b_c.d~e"));
        assert!(!is_well_formed(""));
        assert!(!is_well_formed("has space"));
        assert!(!is_well_formed("semi;colon"));
        assert!(!is_well_formed("amper&sand"));
        assert!(!is_well_formed("new\nline"));
        assert!(!is_well_formed("equals=sign"));
    }
}
