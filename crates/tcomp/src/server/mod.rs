pub mod auth;
pub mod config;
pub mod produce;
pub mod session;
pub mod view;

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

/// Start the relay server. Resolves once the server stops (signal or error).
/// `on_ready` is called with the bound address just before accepting connections.
pub async fn serve(config: Config, on_ready: impl FnOnce(&str)) -> anyhow::Result<()> {
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
    let bound = listener.local_addr()?.to_string();

    // If public_url was left blank (standalone mode), derive it from the bound address.
    let config = if config.public_url.is_empty() {
        let mut c = (*config).clone();
        c.public_url = format!("http://{bound}");
        Arc::new(c)
    } else {
        config
    };

    tracing::info!(bind = %bound, public_url = %config.public_url, "tcomp relay listening");
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
