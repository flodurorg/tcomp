use crate::server::auth::{authorize, Access};
use crate::server::session::Session;
use crate::server::App;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use bytes::Bytes;
use futures_util::stream::{SplitStream, StreamExt};
use futures_util::SinkExt;
use std::sync::Arc;

pub async fn upgrade(State(app): State<App>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle(app, headers, socket))
}

async fn handle(app: App, headers: HeaderMap, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();

    let Some(hello) = read_hello(&mut stream).await else {
        return;
    };

    let access = Access::Produce {
        token: hello.token.as_deref(),
    };
    if let Err(rejection) = authorize(&app.config, &headers, access).await {
        tracing::warn!(reason = rejection.1, "producer rejected");
        let _ = sink.send(Message::Close(None)).await;
        return;
    }

    let (session, epoch) = attach(&app, &hello);
    let ack = proto::HelloAck {
        session: session.id.clone(),
        url: app.config.session_url(&session.id),
    };
    if sink
        .send(Message::Text(
            serde_json::to_string(&ack).unwrap_or_default().into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    tracing::info!(
        session = %session.id,
        name = %hello.name,
        cmd = %hello.cmd,
        input = hello.input,
        "producer attached"
    );

    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    session.attach_input(epoch, input_tx);
    let downstream = tokio::spawn(async move {
        while let Some(bytes) = input_rx.recv().await {
            if sink.send(Message::Binary(bytes)).await.is_err() {
                break;
            }
        }
    });

    let clean_exit = pump(&session, &mut stream).await;

    downstream.abort();
    session.detach_input(epoch);
    if clean_exit {
        tracing::info!(session = %session.id, "session ended");
    } else {
        session.mark_disconnected(epoch);
        tracing::info!(session = %session.id, "producer lost, session is stale");
    }
}

async fn pump(session: &Arc<Session>, stream: &mut SplitStream<WebSocket>) -> bool {
    let mut clean_exit = false;
    while let Some(Ok(message)) = stream.next().await {
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
    clean_exit
}

fn attach(app: &App, hello: &proto::Hello) -> (Arc<Session>, u64) {
    if let Some(existing) = hello.resume.as_ref().and_then(|id| app.store.get(id)) {
        let epoch = existing.resume(hello.cols, hello.rows, hello.input);
        tracing::info!(session = %existing.id, "producer resumed");
        return (existing, epoch);
    }
    let session = Arc::new(Session::new(
        new_id(),
        hello.name.clone(),
        hello.cmd.clone(),
        hello.cols,
        hello.rows,
        hello.input,
        &app.config,
    ));
    app.store.insert(session.clone());
    let epoch = session.epoch();
    (session, epoch)
}

async fn read_hello(stream: &mut SplitStream<WebSocket>) -> Option<proto::Hello> {
    match stream.next().await {
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
