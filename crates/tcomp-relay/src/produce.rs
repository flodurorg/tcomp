use crate::auth::{authorize, Access};
use crate::session::Session;
use crate::App;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use std::sync::Arc;

pub async fn upgrade(State(app): State<App>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle(app, headers, socket))
}

async fn handle(app: App, headers: HeaderMap, mut socket: WebSocket) {
    let Some(hello) = read_hello(&mut socket).await else {
        return;
    };

    let access = Access::Produce {
        token: hello.token.as_deref(),
    };
    if let Err(rejection) = authorize(&app.config, &headers, access).await {
        tracing::warn!(reason = rejection.1, "producer rejected");
        let _ = socket.send(Message::Close(None)).await;
        return;
    }

    let (session, epoch) = attach(&app, &hello);
    let ack = proto::HelloAck {
        session: session.id.clone(),
        url: app.config.session_url(&session.id),
    };
    if socket
        .send(Message::Text(
            serde_json::to_string(&ack).unwrap_or_default().into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    tracing::info!(session = %session.id, name = %hello.name, cmd = %hello.cmd, "producer attached");

    let mut clean_exit = false;
    while let Some(Ok(message)) = socket.recv().await {
        match message {
            Message::Binary(bytes) => session.feed(bytes),
            Message::Text(text) => match serde_json::from_str::<proto::Producer>(&text) {
                Ok(proto::Producer::Resize { cols, rows }) => session.resize(cols, rows),
                Ok(proto::Producer::Exit { code }) => {
                    session.mark_exit(code);
                    clean_exit = true;
                }
                Err(error) => tracing::debug!(%error, "bad producer control frame"),
            },
            Message::Close(_) => break,
            _ => {}
        }
    }

    if !clean_exit {
        session.mark_disconnected(epoch);
        tracing::info!(session = %session.id, "producer lost, session is stale");
    } else {
        tracing::info!(session = %session.id, "session ended");
    }
}

fn attach(app: &App, hello: &proto::Hello) -> (Arc<Session>, u64) {
    if let Some(existing) = hello.resume.as_ref().and_then(|id| app.store.get(id)) {
        let epoch = existing.resume(hello.cols, hello.rows);
        tracing::info!(session = %existing.id, "producer resumed");
        return (existing, epoch);
    }
    let session = Arc::new(Session::new(
        new_id(),
        hello.name.clone(),
        hello.cmd.clone(),
        hello.cols,
        hello.rows,
        &app.config,
    ));
    app.store.insert(session.clone());
    let epoch = session.epoch();
    (session, epoch)
}

async fn read_hello(socket: &mut WebSocket) -> Option<proto::Hello> {
    match socket.recv().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<proto::Hello>(&text) {
            Ok(hello) if hello.v == proto::VERSION => Some(hello),
            Ok(hello) => {
                tracing::warn!(version = hello.v, "unsupported producer protocol version");
                None
            }
            Err(error) => {
                tracing::warn!(%error, "malformed hello");
                None
            }
        },
        other => {
            tracing::warn!(frame = ?other, "producer did not open with a hello frame");
            None
        }
    }
}

fn new_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}
