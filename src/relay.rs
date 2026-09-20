use crate::Event;
use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;

const PING_INTERVAL: Duration = Duration::from_secs(20);
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

pub struct Params {
    pub relay: String,
    pub name: String,
    pub cmd: String,
    pub token: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub input: bool,
    pub cwd: Option<String>,
    pub key: Option<crate::crypto::Key>,
}

pub fn produce_url(base: &str) -> Result<String> {
    let base = base.trim_end_matches('/');
    let url = match base.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}"),
        Some(("http", rest)) => format!("ws://{rest}"),
        Some(("wss", rest)) => format!("wss://{rest}"),
        Some(("ws", rest)) => format!("ws://{rest}"),
        Some((scheme, _)) => return Err(anyhow!("unsupported relay scheme: {scheme}")),
        None => format!("wss://{base}"),
    };
    Ok(format!("{url}/ws/produce"))
}

pub async fn run(
    params: Params,
    mut events: UnboundedReceiver<Event>,
    writes: Option<std::sync::mpsc::Sender<Vec<u8>>>,
) {
    let url = match produce_url(&params.relay) {
        Ok(url) => url,
        Err(error) => {
            crate::term::note(crate::term::Note::Warn, &error.to_string());
            drain(&mut events).await;
            return;
        }
    };

    if params.key.is_some() {
        if let Err(error) = crate::encrypted::run(&url, &params, &mut events, writes.as_ref()).await
        {
            crate::term::note(
                crate::term::Note::Warn,
                &format!("encrypted relay stopped: {error}"),
            );
            drain(&mut events).await;
        }
        return;
    }

    let mut session: Option<String> = None;
    let mut backoff = BACKOFF_MIN;
    let mut cols = params.cols;
    let mut rows = params.rows;
    let mut title: Option<String> = None;
    let mut cwd = params.cwd.clone();

    loop {
        match connect(&url, &params, &session, cols, rows).await {
            Ok((mut socket, ack)) => {
                if session.as_deref() != Some(ack.session.as_str()) {
                    if session.is_some() {
                        crate::term::note(
                            crate::term::Note::Warn,
                            "relay lost the old session — reconnected under a new URL",
                        );
                    }
                    crate::term::note_with_url(crate::term::Note::Good, "watch at", Some(&ack.url));
                    tokio::spawn(async {
                        tokio::time::sleep(crate::term::LINK_LINGER).await;
                        let _ = crate::term::end_linger();
                    });
                }
                let resumed = session.as_deref() == Some(ack.session.as_str());
                session = Some(ack.session);
                backoff = BACKOFF_MIN;
                if resumed {
                    if let Some(text) = &title {
                        if socket
                            .send(control(&proto::Producer::Title { text: text.clone() }))
                            .await
                            .is_err()
                        {
                            continue;
                        }
                    }
                    if let Some(path) = &cwd {
                        if socket
                            .send(control(&proto::Producer::Cwd { path: path.clone() }))
                            .await
                            .is_err()
                        {
                            continue;
                        }
                    }
                }
                match pump(
                    &mut socket,
                    &mut events,
                    &mut cols,
                    &mut rows,
                    &mut title,
                    &mut cwd,
                    writes.as_ref(),
                )
                .await
                {
                    Outcome::Finished => {
                        let _ = socket.close(None).await;
                        return;
                    }
                    Outcome::Disconnected => {}
                }
            }
            Err(error) => {
                if let Some(Rejected(reason)) = error.downcast_ref::<Rejected>() {
                    crate::term::note(
                        crate::term::Note::Warn,
                        &format!("relay refused this session: {reason}"),
                    );
                    drain(&mut events).await;
                    return;
                }
                tracing_eprint(&error, session.is_none());
            }
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

enum Outcome {
    Finished,
    Disconnected,
}

pub(crate) type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub(crate) async fn connect(
    url: &str,
    params: &Params,
    session: &Option<String>,
    cols: u16,
    rows: u16,
) -> Result<(Socket, proto::HelloAck)> {
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await?;
    let hello = proto::Hello {
        v: if params.key.is_some() {
            proto::ENCRYPTED_VERSION
        } else {
            proto::VERSION
        },
        token: params.token.clone(),
        name: if params.key.is_some() {
            "Encrypted session".into()
        } else {
            params.name.clone()
        },
        cmd: if params.key.is_some() {
            String::new()
        } else {
            params.cmd.clone()
        },
        cols,
        rows,
        input: params.input,
        resume: session.clone(),
        cwd: if params.key.is_some() {
            None
        } else {
            params.cwd.clone()
        },
    };
    socket
        .send(Message::Text(serde_json::to_string(&hello)?.into()))
        .await?;

    match socket.next().await {
        Some(Ok(Message::Text(text))) => {
            let ack: proto::HelloAck = serde_json::from_str(&text)?;
            if params.key.is_some() && ack.encryption != Some(proto::ENCRYPTED_VERSION) {
                return Err(Rejected("relay does not support end-to-end encryption".into()).into());
            }
            Ok((socket, ack))
        }
        Some(Ok(Message::Close(Some(frame)))) if frame.code == CloseCode::Policy => {
            Err(Rejected(frame.reason.to_string()).into())
        }
        Some(Ok(_)) => Err(anyhow!("relay sent an unexpected frame")),
        Some(Err(error)) => Err(error.into()),
        None => Err(anyhow!("relay closed the connection")),
    }
}

/// Turned away on purpose; `run` gives up rather than reconnecting forever.
#[derive(Debug)]
pub(crate) struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

async fn pump(
    socket: &mut Socket,
    events: &mut UnboundedReceiver<Event>,
    cols: &mut u16,
    rows: &mut u16,
    title: &mut Option<String>,
    cwd: &mut Option<String>,
    writes: Option<&std::sync::mpsc::Sender<Vec<u8>>>,
) -> Outcome {
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;

    loop {
        let message = tokio::select! {
            _ = ping.tick() => Message::Ping(Default::default()),
            incoming = socket.next() => match incoming {
                Some(Ok(Message::Close(_))) | None => return Outcome::Disconnected,
                Some(Err(_)) => return Outcome::Disconnected,
                Some(Ok(Message::Binary(bytes))) => {
                    if let Some(writes) = writes {
                        if !bytes.is_empty() && writes.send(bytes.to_vec()).is_err() {
                            return Outcome::Finished;
                        }
                    }
                    continue;
                }
                Some(Ok(_)) => continue,
            },
            event = events.recv() => match event {
                Some(Event::Output(bytes)) => Message::Binary(bytes.into()),
                Some(Event::Resize { cols: c, rows: r }) => {
                    *cols = c;
                    *rows = r;
                    control(&proto::Producer::Resize { cols: c, rows: r })
                }
                Some(Event::Metadata(metadata)) => {
                    if let Some(text) = metadata.title {
                        *title = Some(text.clone());
                        if socket
                            .send(control(&proto::Producer::Title { text }))
                            .await
                            .is_err()
                        {
                            return Outcome::Disconnected;
                        }
                    }
                    if let Some(path) = metadata.cwd {
                        *cwd = Some(path.clone());
                        if socket
                            .send(control(&proto::Producer::Cwd { path }))
                            .await
                            .is_err()
                        {
                            return Outcome::Disconnected;
                        }
                    }
                    continue;
                }
                Some(Event::Exit { code }) => {
                    let _ = socket.send(control(&proto::Producer::Exit { code })).await;
                    let _ = socket.flush().await;
                    return Outcome::Finished;
                }
                None => return Outcome::Finished,
            },
        };

        if socket.send(message).await.is_err() {
            return Outcome::Disconnected;
        }
    }
}

fn control(frame: &proto::Producer) -> Message {
    Message::Text(serde_json::to_string(frame).unwrap_or_default().into())
}

fn tracing_eprint(error: &anyhow::Error, first_attempt: bool) {
    if first_attempt {
        crate::term::note(
            crate::term::Note::Warn,
            &format!("relay unavailable ({error}); retrying in background"),
        );
    }
}

async fn drain(events: &mut UnboundedReceiver<Event>) {
    while events.recv().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn encrypted_hello_hides_metadata_and_rejects_unnegotiated_ack() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let hello = socket.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(!hello.contains("private"));
            let hello: proto::Hello = serde_json::from_str(&hello).unwrap();
            assert_eq!(hello.v, proto::ENCRYPTED_VERSION);
            assert_eq!(hello.name, "Encrypted session");
            assert!(hello.cmd.is_empty());
            assert!(hello.cwd.is_none());
            socket
                .send(Message::Text(
                    r#"{"session":"s","url":"http://localhost/s/s"}"#.into(),
                ))
                .await
                .unwrap();
            if let Ok(Some(Ok(message))) =
                tokio::time::timeout(Duration::from_secs(1), socket.next()).await
            {
                assert!(!matches!(message, Message::Binary(_)));
            }
        });
        let params = Params {
            relay: url.clone(),
            name: "private-name".into(),
            cmd: "private-command".into(),
            token: None,
            cols: 80,
            rows: 24,
            input: true,
            cwd: Some("private-cwd".into()),
            key: Some(crate::crypto::Key::generate().unwrap()),
        };
        let result = connect(&url, &params, &None, 80, 24).await;
        assert!(matches!(result, Err(error) if error.is::<Rejected>()));
        server.await.unwrap();
    }

    #[test]
    fn https_becomes_wss() {
        assert_eq!(
            produce_url("https://relay.example.com").unwrap(),
            "wss://relay.example.com/ws/produce"
        );
    }

    #[test]
    fn http_becomes_ws() {
        assert_eq!(
            produce_url("http://127.0.0.1:8080").unwrap(),
            "ws://127.0.0.1:8080/ws/produce"
        );
    }

    #[test]
    fn trailing_slash_does_not_double_up() {
        assert_eq!(
            produce_url("https://relay.example.com/").unwrap(),
            "wss://relay.example.com/ws/produce"
        );
    }

    #[test]
    fn a_bare_host_defaults_to_tls() {
        assert_eq!(
            produce_url("relay.example.com").unwrap(),
            "wss://relay.example.com/ws/produce"
        );
    }

    #[test]
    fn ws_urls_are_passed_through() {
        assert_eq!(
            produce_url("wss://relay.example.com").unwrap(),
            "wss://relay.example.com/ws/produce"
        );
    }

    #[test]
    fn unknown_schemes_are_rejected() {
        assert!(produce_url("ftp://relay.example.com").is_err());
    }
}
