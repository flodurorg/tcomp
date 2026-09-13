use crate::server::auth::{authorize, Access};
use crate::server::session::{Frame, Join, Session};
use crate::server::App;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

pub async fn upgrade(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if authorize(&app.config, &headers, Access::View { session: &id })
        .await
        .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(session) = app.store.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let may_write = authorize(&app.config, &headers, Access::Write { session: &id })
        .await
        .is_ok();
    ws.on_upgrade(move |socket| handle(socket, session, may_write))
}

async fn handle(socket: WebSocket, session: Arc<Session>, may_write: bool) {
    let (mut sink, mut stream) = socket.split();

    let upstream = session.clone();
    tokio::spawn(async move {
        while let Some(Ok(message)) = stream.next().await {
            if let Message::Binary(bytes) = message {
                if may_write && !bytes.is_empty() {
                    upstream.send_input(bytes);
                }
            }
        }
    });

    let Join {
        init,
        payload,
        mut rx,
    } = session.join();

    if send_ctrl(&mut sink, &init).await.is_err() {
        return;
    }
    for chunk in payload {
        if sink.send(Message::Binary(chunk)).await.is_err() {
            return;
        }
    }

    loop {
        let result = match rx.recv().await {
            Ok(Frame::Data(bytes)) => sink.send(Message::Binary(bytes)).await,
            Ok(Frame::Ctrl(control)) => send_ctrl(&mut sink, &control).await,
            Err(RecvError::Lagged(dropped)) => {
                tracing::debug!(dropped, "viewer lagged, resyncing from dump");
                sink.send(Message::Binary(session.dump())).await
            }
            Err(RecvError::Closed) => break,
        };
        if result.is_err() {
            break;
        }
    }
}

async fn send_ctrl<S>(sink: &mut S, frame: &proto::Server) -> Result<(), axum::Error>
where
    S: SinkExt<Message, Error = axum::Error> + Unpin,
{
    let json = serde_json::to_string(frame).unwrap_or_default();
    sink.send(Message::Text(json.into())).await
}
