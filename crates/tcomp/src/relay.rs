use crate::Event;
use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
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
            eprint!("tcomp: {error}\r\n");
            drain(&mut events).await;
            return;
        }
    };

    let mut session: Option<String> = None;
    let mut backoff = BACKOFF_MIN;
    let mut cols = params.cols;
    let mut rows = params.rows;

    loop {
        match connect(&url, &params, &session, cols, rows).await {
            Ok((mut socket, ack)) => {
                if session.is_none() {
                    eprint!("tcomp: watch at {}\r\n", ack.url);
                }
                session = Some(ack.session);
                backoff = BACKOFF_MIN;
                match pump(
                    &mut socket,
                    &mut events,
                    &mut cols,
                    &mut rows,
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

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(
    url: &str,
    params: &Params,
    session: &Option<String>,
    cols: u16,
    rows: u16,
) -> Result<(Socket, proto::HelloAck)> {
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await?;
    let hello = proto::Hello {
        v: proto::VERSION,
        token: params.token.clone(),
        name: params.name.clone(),
        cmd: params.cmd.clone(),
        cols,
        rows,
        input: params.input,
        resume: session.clone(),
    };
    socket
        .send(Message::Text(serde_json::to_string(&hello)?.into()))
        .await?;

    match socket.next().await {
        Some(Ok(Message::Text(text))) => {
            let ack: proto::HelloAck = serde_json::from_str(&text)?;
            Ok((socket, ack))
        }
        Some(Ok(_)) => Err(anyhow!("relay sent an unexpected frame")),
        Some(Err(error)) => Err(error.into()),
        None => Err(anyhow!("relay closed the connection")),
    }
}

async fn pump(
    socket: &mut Socket,
    events: &mut UnboundedReceiver<Event>,
    cols: &mut u16,
    rows: &mut u16,
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
        eprint!("tcomp: relay unavailable ({error}); retrying in background\r\n");
    }
}

async fn drain(events: &mut UnboundedReceiver<Event>) {
    while events.recv().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::produce_url;

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
