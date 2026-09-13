mod auth;
mod config;
mod produce;
mod session;
mod view;

use auth::{authorize, Access};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use config::Config;
use session::Store;
use std::sync::Arc;
use std::time::Duration;
use tower_http::services::ServeDir;

#[derive(Clone)]
pub struct App {
    pub config: Arc<Config>,
    pub store: Store,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tcomp_relay=info,tower_http=warn".into()),
        )
        .init();

    let config = Arc::new(Config::from_env());
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
        .route("/api/sessions", get(list_sessions))
        .route("/s/{id}", get(session_page))
        .route("/ws/produce", get(produce::upgrade))
        .route("/ws/view/{id}", get(view::upgrade))
        .nest_service(
            "/static",
            ServeDir::new(format!("{}/static", config.web_dir)),
        )
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(bind = %config.bind, public_url = %config.public_url, "tcomp-relay listening");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn index(State(app): State<App>, headers: HeaderMap) -> Response {
    if authorize(&app.config, &headers, Access::Index)
        .await
        .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    page(&app, "index.html").await
}

async fn session_page(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if authorize(&app.config, &headers, Access::View { session: &id })
        .await
        .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if app.store.get(&id).is_none() {
        return (StatusCode::NOT_FOUND, "no such session").into_response();
    }
    page(&app, "session.html").await
}

async fn list_sessions(State(app): State<App>, headers: HeaderMap) -> Response {
    if authorize(&app.config, &headers, Access::Index)
        .await
        .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    Json(app.store.list(&app.config)).into_response()
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
