pub mod auth;
pub mod config;
pub mod produce;
pub mod session;
pub mod view;

use anyhow::Context;
use auth::{authorize, Access};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Json, Router};
use config::Config;
use serde::Deserialize;
use session::Store;
use std::sync::Arc;
use std::time::Duration;
use tower_http::services::ServeDir;

fn public_url_for(addr: std::net::SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        format!("http://localhost:{}", addr.port())
    } else {
        format!("http://{addr}")
    }
}

#[derive(Clone)]
pub struct App {
    pub config: Arc<Config>,
    pub store: Store,
}

/// Start the relay server. Resolves once the server stops (signal or error).
/// `on_ready` is called with the bound address just before accepting connections.
pub async fn serve(mut config: Config, on_ready: impl FnOnce(&str)) -> anyhow::Result<()> {
    if let Some(token) = config.token.as_deref() {
        anyhow::ensure!(
            auth::is_well_formed(token),
            "TCOMP_TOKEN must be URL-safe: letters, digits and -._~ only"
        );
    }

    // Bind first: standalone may ask for port 0 and needs the bound address to
    // build public_url, which the router captures via App state.
    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .with_context(|| format!("cannot bind {}", config.bind))?;
    let addr = listener.local_addr()?;
    let bound = addr.to_string();
    if config.public_url.is_empty() {
        config.public_url = public_url_for(addr);
    }

    let config = Arc::new(config);
    let app = App {
        config: config.clone(),
        store: Store::default(),
    };

    {
        let app = app.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(15));
            loop {
                ticker.tick().await;
                let reaped = app.store.reap(&app.config);
                if reaped > 0 {
                    tracing::info!(reaped, "expired sessions removed");
                }
            }
        });
    }

    let router = Router::new()
        .route("/", get(index))
        .route("/healthz", get(|| async { "ok" }))
        .route("/login", get(login_page).post(login))
        .route("/api/sessions", get(list_sessions))
        .route("/s/{id}", get(session_page))
        .route("/ws/produce", get(produce::upgrade))
        .route("/ws/view/{id}", get(view::upgrade))
        .nest_service(
            "/static",
            ServeDir::new(format!("{}/static", config.web_dir)),
        )
        .with_state(app);

    tracing::info!(
        bind = %bound,
        public_url = %config.public_url,
        auth = config.token.is_some(),
        "tcomp relay listening"
    );
    if config.token.is_none() {
        tracing::warn!(
            "no TCOMP_TOKEN set: anyone who can reach this relay can watch every session"
        );
    }
    on_ready(&bound);

    const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
    let served = axum::serve(listener, router).with_graceful_shutdown(terminated());
    tokio::select! {
        result = served => result?,
        _ = async { terminated().await; tokio::time::sleep(SHUTDOWN_GRACE).await } => {
            tracing::warn!("sockets still open after {SHUTDOWN_GRACE:?}, exiting anyway");
        }
    }
    tracing::info!("tcomp relay stopped");
    Ok(())
}

async fn terminated() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => return std::future::pending().await,
    };
    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(_) => return std::future::pending().await,
    };
    tokio::select! {
        _ = terminate.recv() => {}
        _ = interrupt.recv() => {}
    }
}

async fn index(
    State(app): State<App>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    if let Some(denied) = admit(&app, &headers, Access::Index, &query, "/").await {
        return denied;
    }
    page(&app, "index.html").await
}

async fn session_page(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    let access = Access::View { session: &id };
    let back = safe_next(&format!("/s/{id}"));
    if let Some(denied) = admit(&app, &headers, access, &query, &back).await {
        return denied;
    }
    if app.store.get(&id).is_none() {
        return (StatusCode::NOT_FOUND, "no such session").into_response();
    }
    page(&app, "session.html").await
}

async fn list_sessions(State(app): State<App>, headers: HeaderMap) -> Response {
    if let Err(rejection) = authorize(&app.config, &headers, Access::Index).await {
        return (
            rejection.0,
            Json(serde_json::json!({ "error": rejection.1 })),
        )
            .into_response();
    }
    Json(app.store.list(&app.config)).into_response()
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

#[derive(Deserialize)]
struct LoginForm {
    token: String,
    #[serde(default)]
    next: String,
}

/// Gates a page a browser navigates to; `Some` is the response to send instead.
/// A `?token=` that works becomes a cookie and redirects to the clean URL.
async fn admit(
    app: &App,
    headers: &HeaderMap,
    access: Access<'_>,
    query: &TokenQuery,
    path: &str,
) -> Option<Response> {
    if let Some(token) = query.token.as_deref() {
        if auth::accepts(&app.config, token) {
            return Some(authenticated(app, headers, token, path));
        }
    }
    match authorize(&app.config, headers, access).await {
        Ok(_) => None,
        Err(rejection) => Some(sign_in(app, rejection.0).await),
    }
}

async fn login_page(State(app): State<App>, headers: HeaderMap) -> Response {
    if authorize(&app.config, &headers, Access::Index)
        .await
        .is_ok()
    {
        return Redirect::to("/").into_response();
    }
    sign_in(&app, StatusCode::OK).await
}

async fn login(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let token = form.token.trim();
    if auth::accepts(&app.config, token) {
        return authenticated(&app, &headers, token, &safe_next(&form.next));
    }
    tracing::warn!("failed sign-in");
    Redirect::to(&format!(
        "/login?bad=1&next={}",
        utf8_percent_encode(&safe_next(&form.next))
    ))
    .into_response()
}

fn utf8_percent_encode(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn authenticated(app: &App, headers: &HeaderMap, token: &str, path: &str) -> Response {
    let cookie = auth::set_cookie(&app.config, headers, token);
    match HeaderValue::from_str(&cookie) {
        Ok(cookie) => ([(header::SET_COOKIE, cookie)], Redirect::to(path)).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot build the session cookie");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn sign_in(app: &App, status: StatusCode) -> Response {
    match tokio::fs::read_to_string(format!("{}/login.html", app.config.web_dir)).await {
        Ok(body) => (status, Html(body)).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot read the sign-in page");
            (status, "a token is required").into_response()
        }
    }
}

/// Same-origin absolute paths only, so `next` cannot leave this relay.
fn safe_next(next: &str) -> String {
    let next = next.trim();
    let local = next.starts_with('/')
        && !next.starts_with("//")
        && !next.contains('\\')
        && !next.bytes().any(|b| b < 0x20 || b == 0x7f);
    if local {
        next.to_string()
    } else {
        "/".to_string()
    }
}

async fn page(app: &App, name: &str) -> Response {
    match tokio::fs::read_to_string(format!("{}/{}", app.config.web_dir, name)).await {
        Ok(body) => Html(body).into_response(),
        Err(error) => {
            tracing::error!(%error, name, "cannot read page");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod redirect_tests {
    use super::{safe_next, utf8_percent_encode};

    #[test]
    fn a_local_path_is_kept() {
        assert_eq!(safe_next("/"), "/");
        assert_eq!(safe_next("/s/abc123"), "/s/abc123");
        assert_eq!(safe_next("  /s/abc123  "), "/s/abc123");
    }

    #[test]
    fn anywhere_off_this_relay_falls_back_to_the_index() {
        for hostile in [
            "//evil.example.com",
            "https://evil.example.com",
            "http://evil.example.com",
            "/\\evil.example.com",
            "evil.example.com",
            "",
        ] {
            assert_eq!(safe_next(hostile), "/", "{hostile:?} escaped the relay");
        }
    }

    #[test]
    fn a_header_break_cannot_be_smuggled_through_next() {
        assert_eq!(safe_next("/s/a\r\nSet-Cookie: x=y"), "/");
        assert_eq!(safe_next("/s/a\nLocation: http://evil"), "/");
    }

    #[test]
    fn encoding_protects_the_query_it_lands_in() {
        assert_eq!(utf8_percent_encode("/s/abc"), "/s/abc");
        assert_eq!(utf8_percent_encode("/s/a b"), "/s/a%20b");
        assert_eq!(utf8_percent_encode("/s/a&b=c"), "/s/a%26b%3Dc");
    }
}

#[cfg(test)]
mod public_url_tests {
    use super::public_url_for;
    use crate::server::config::normalize_public_url;

    #[test]
    fn wildcard_bind_becomes_localhost() {
        assert_eq!(
            public_url_for("0.0.0.0:8080".parse().unwrap()),
            "http://localhost:8080"
        );
        assert_eq!(
            public_url_for("[::]:8080".parse().unwrap()),
            "http://localhost:8080"
        );
    }

    #[test]
    fn specific_address_is_used_verbatim() {
        assert_eq!(
            public_url_for("100.101.102.103:41157".parse().unwrap()),
            "http://100.101.102.103:41157"
        );
        assert_eq!(
            public_url_for("[fd7a::1]:8080".parse().unwrap()),
            "http://[fd7a::1]:8080"
        );
    }

    #[test]
    fn public_url_loses_only_its_trailing_slash() {
        assert_eq!(normalize_public_url(""), "");
        assert_eq!(
            normalize_public_url(" https://box.tail1234.ts.net/ "),
            "https://box.tail1234.ts.net"
        );
        assert_eq!(
            normalize_public_url("https://box.tail1234.ts.net/tcomp"),
            "https://box.tail1234.ts.net/tcomp"
        );
    }
}
