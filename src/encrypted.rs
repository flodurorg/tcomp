use crate::crypto::{self, InputReceiver, Sender, OUTPUT, SNAPSHOT};
use crate::relay::{self, Params, Socket};
use crate::screen::{clamp_size, decode_into, dump_bytes};
use crate::Event;
use anyhow::{ensure, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::Message;

struct Screen {
    vt: avt::Vt,
    carry: Vec<u8>,
    cols: u16,
    rows: u16,
    title: Option<String>,
    cwd: Option<String>,
    exit: Option<i32>,
}

impl Screen {
    fn new(params: &Params) -> Self {
        let (cols, rows) = clamp_size(params.cols, params.rows);
        Self {
            vt: avt::Vt::builder()
                .size(cols as usize, rows as usize)
                .scrollback_limit(2000)
                .build(),
            carry: Vec::new(),
            cols,
            rows,
            title: None,
            cwd: params.cwd.clone(),
            exit: None,
        }
    }

    fn update(&mut self, event: Event) -> Option<Vec<u8>> {
        match event {
            Event::Output(bytes) => {
                self.carry.extend_from_slice(&bytes);
                decode_into(&mut self.vt, &mut self.carry);
                Some(bytes)
            }
            Event::Resize { cols, rows } => {
                (self.cols, self.rows) = clamp_size(cols, rows);
                self.vt.resize(self.cols as usize, self.rows as usize);
                None
            }
            Event::Metadata(metadata) => {
                if let Some(title) = metadata.title {
                    self.title = Some(title);
                }
                if let Some(cwd) = metadata.cwd {
                    self.cwd = Some(cwd);
                }
                None
            }
            Event::Exit { code } => {
                self.exit = Some(code);
                None
            }
        }
    }

    fn snapshot(
        &self,
        params: &Params,
        epoch: &str,
        sender: &mut Sender,
        budget: usize,
        checkpoint: bool,
    ) -> Result<Vec<u8>> {
        let payload = proto::EncryptedPayload::Snapshot {
            cols: self.cols,
            rows: self.rows,
            name: params.name.clone(),
            cmd: params.cmd.clone(),
            title: self.title.clone(),
            cwd: self.cwd.clone(),
            input: params.input,
            epoch: epoch.into(),
            screen: String::from_utf8(dump_bytes(&self.vt).to_vec())?,
            carry: self.carry.clone(),
            exit: self.exit,
            checkpoint,
        };
        let frame = sender.seal(SNAPSHOT, &serde_json::to_vec(&payload)?)?;
        ensure!(frame.len() <= budget, "encrypted snapshot exceeds the relay history budget; increase TCOMP_HISTORY_BYTES on the relay");
        Ok(frame)
    }
}

pub async fn run(
    url: &str,
    params: &Params,
    events: &mut UnboundedReceiver<Event>,
    writes: Option<&std::sync::mpsc::Sender<Vec<u8>>>,
) -> Result<()> {
    let key = params.key.as_ref().unwrap();
    let mut screen = Screen::new(params);
    let mut session = None;
    let mut sender = None;
    let mut backoff = Duration::from_millis(500);
    loop {
        let (cols, rows) = (screen.cols, screen.rows);
        let resume = session.clone();
        let connect = tokio::time::timeout(
            Duration::from_secs(15),
            relay::connect(url, params, &resume, cols, rows),
        );
        tokio::pin!(connect);
        let connection = loop {
            tokio::select! {
                result = &mut connect => break result,
                Some(event) = events.recv() => { screen.update(event); }
            }
        };
        match connection {
            Ok(Ok((mut socket, ack))) => {
                if session.as_deref() != Some(&ack.session) {
                    sender = Some(Sender::new(key, &ack.session, false)?);
                    let link = format!(
                        "{}#key={}",
                        ack.url.split('#').next().unwrap_or(&ack.url),
                        key.fragment()
                    );
                    crate::term::note_with_url(crate::term::Note::Good, "watch at", Some(&link));
                    tokio::spawn(async {
                        tokio::time::sleep(crate::term::LINK_LINGER).await;
                        let _ = crate::term::end_linger();
                    });
                }
                session = Some(ack.session.clone());
                let budget = ack
                    .history_bytes
                    .unwrap_or(0)
                    .min(proto::MAX_ENCRYPTED_FRAME);
                ensure!(
                    budget > crypto::HEADER_LEN + 16,
                    "relay has no encrypted replay capacity"
                );
                let receiver = InputReceiver::new()?;
                let finished = pump(
                    &mut socket,
                    events,
                    writes,
                    params,
                    &ack.session,
                    &mut screen,
                    sender.as_mut().unwrap(),
                    receiver,
                    budget,
                )
                .await?;
                if finished {
                    let _ = socket.close(None).await;
                    return Ok(());
                }
                backoff = Duration::from_millis(500);
            }
            Ok(Err(error)) if error.is::<relay::Rejected>() => return Err(error),
            _ => {}
        }
        let pause = tokio::time::sleep(backoff);
        tokio::pin!(pause);
        loop {
            tokio::select! {
                _ = &mut pause => break,
                Some(event) = events.recv() => { screen.update(event); }
            }
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
        if events.is_closed() && events.is_empty() && screen.exit.is_none() {
            return Ok(());
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn pump(
    socket: &mut Socket,
    events: &mut UnboundedReceiver<Event>,
    writes: Option<&std::sync::mpsc::Sender<Vec<u8>>>,
    params: &Params,
    session: &str,
    screen: &mut Screen,
    sender: &mut Sender,
    mut receiver: InputReceiver,
    budget: usize,
) -> Result<bool> {
    let snapshot = screen.snapshot(params, receiver.epoch(), sender, budget, false)?;
    let mut history_len = snapshot.len();
    if socket.send(Message::Binary(snapshot.into())).await.is_err() {
        return Ok(false);
    }
    if screen.exit.is_some() {
        return finish(socket).await;
    }
    let mut checkpoint = tokio::time::interval(Duration::from_secs(2));
    checkpoint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    checkpoint.tick().await;
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    ping.tick().await;
    let mut dirty = false;
    loop {
        let frame = tokio::select! {
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Binary(frame))) if params.input => {
                        if let Ok(bytes) = receiver.receive(params.key.as_ref().unwrap(), session, &frame) {
                            if let Some(writes) = writes {
                                if !bytes.is_empty() && writes.send(bytes).is_err() { return Ok(true); }
                            }
                        }
                    }
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return Ok(false),
                    _ => {}
                }
                continue;
            }
            _ = ping.tick() => {
                if socket.send(Message::Ping(Default::default())).await.is_err() { return Ok(false); }
                continue;
            }
            _ = checkpoint.tick(), if dirty => {
                dirty = false;
                let frame = screen.snapshot(params, receiver.epoch(), sender, budget, true)?;
                history_len = frame.len();
                frame
            }
            event = events.recv() => {
                let Some(event) = event else { return Ok(true); };
                let checkpoint = matches!(event, Event::Metadata(_) | Event::Exit { .. });
                if let Some(bytes) = screen.update(event) {
                    dirty = true;
                    let payload = proto::EncryptedPayload::Output { bytes };
                    let plaintext = serde_json::to_vec(&payload)?;
                    let frame_len = plaintext.len() + crypto::HEADER_LEN + 16;
                    if history_len.saturating_add(frame_len) <= budget {
                        history_len += frame_len;
                        sender.seal(OUTPUT, &plaintext)?
                    } else {
                        dirty = false;
                        let frame = screen.snapshot(params, receiver.epoch(), sender, budget, false)?;
                        history_len = frame.len();
                        frame
                    }
                } else {
                    dirty = false;
                    let frame = screen.snapshot(params, receiver.epoch(), sender, budget, checkpoint)?;
                    history_len = frame.len();
                    frame
                }
            }
        };
        if socket.send(Message::Binary(frame.into())).await.is_err() {
            return Ok(false);
        }
        if screen.exit.is_some() {
            return finish(socket).await;
        }
    }
}

async fn finish(socket: &mut Socket) -> Result<bool> {
    let control = serde_json::to_string(&proto::Producer::Exit { code: 0 })?;
    Ok(socket.send(Message::Text(control.into())).await.is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params {
            relay: "http://localhost".into(),
            name: "private-name".into(),
            cmd: "private-command".into(),
            token: None,
            cols: 80,
            rows: 24,
            input: true,
            cwd: Some("private-directory".into()),
            key: Some(crypto::Key::generate().unwrap()),
        }
    }

    fn decrypt(params: &Params, frame: &[u8]) -> proto::EncryptedPayload {
        serde_json::from_slice(
            &crypto::open(params.key.as_ref().unwrap(), "test-session", frame, false)
                .unwrap()
                .1,
        )
        .unwrap()
    }

    #[test]
    fn snapshots_preserve_resize_unicode_carry_and_metadata() {
        let params = params();
        let mut screen = Screen::new(&params);
        screen.update(Event::Resize {
            cols: 100,
            rows: 30,
        });
        screen.update(Event::Output(
            b"\x1b[?1049h\x1b[2J\x1b[Hsecret\xe2\x82".to_vec(),
        ));
        screen.update(Event::Metadata(crate::modes::Seen {
            title: Some("private-title".into()),
            cwd: Some("/private".into()),
        }));
        let mut sender = Sender::new(params.key.as_ref().unwrap(), "test-session", false).unwrap();
        let frame = screen
            .snapshot(
                &params,
                "epoch",
                &mut sender,
                proto::MAX_ENCRYPTED_FRAME,
                false,
            )
            .unwrap();
        let proto::EncryptedPayload::Snapshot {
            cols,
            rows,
            title,
            cwd,
            screen,
            carry,
            ..
        } = decrypt(&params, &frame)
        else {
            panic!("snapshot")
        };
        assert_eq!((cols, rows), (100, 30));
        assert_eq!(title.as_deref(), Some("private-title"));
        assert_eq!(cwd.as_deref(), Some("/private"));
        assert_eq!(carry, b"\xe2\x82");
        let mut replay = avt::Vt::new(100, 30);
        replay.feed_str(&screen);
        let mut carry = carry;
        carry.push(0xac);
        decode_into(&mut replay, &mut carry);
        assert!(replay.view().any(|line| line.text().contains("secret€")));
    }

    #[test]
    fn oversized_snapshots_fail_instead_of_truncating() {
        let params = params();
        let screen = Screen::new(&params);
        let mut sender = Sender::new(params.key.as_ref().unwrap(), "test-session", false).unwrap();
        assert!(screen
            .snapshot(&params, "epoch", &mut sender, 64, false)
            .is_err());
    }

    #[tokio::test]
    async fn only_redundant_snapshots_are_checkpoints() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let params = params();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = async {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            socket.next().await.unwrap().unwrap();
            let ack = serde_json::json!({ "session": "test-session", "url": "http://localhost/s/test-session", "encryption": 2, "history_bytes": 16384 });
            socket
                .send(Message::Text(ack.to_string().into()))
                .await
                .unwrap();
            assert!(matches!(
                next_payload(&params, &mut socket).await,
                proto::EncryptedPayload::Snapshot {
                    checkpoint: false,
                    ..
                }
            ));

            events_tx
                .send(Event::Output(b"live-output".to_vec()))
                .unwrap();
            assert!(matches!(next_payload(&params, &mut socket).await,
                proto::EncryptedPayload::Output { bytes } if bytes == b"live-output"));
            assert!(matches!(next_payload(&params, &mut socket).await,
                proto::EncryptedPayload::Snapshot { checkpoint: true, screen, .. } if screen.contains("live-output")));

            events_tx
                .send(Event::Metadata(crate::modes::Seen {
                    title: Some("new-title".into()),
                    cwd: None,
                }))
                .unwrap();
            assert!(matches!(next_payload(&params, &mut socket).await,
                proto::EncryptedPayload::Snapshot { checkpoint: true, title, .. } if title.as_deref() == Some("new-title")));

            events_tx
                .send(Event::Resize {
                    cols: 100,
                    rows: 30,
                })
                .unwrap();
            assert!(matches!(
                next_payload(&params, &mut socket).await,
                proto::EncryptedPayload::Snapshot {
                    checkpoint: false,
                    cols: 100,
                    rows: 30,
                    ..
                }
            ));

            let mut bytes = vec![b'\r'; 16384];
            bytes.extend_from_slice(b"budget-output");
            events_tx.send(Event::Output(bytes)).unwrap();
            assert!(matches!(next_payload(&params, &mut socket).await,
                proto::EncryptedPayload::Snapshot { checkpoint: false, screen, .. } if screen.contains("budget-output")));

            events_tx.send(Event::Exit { code: 7 }).unwrap();
            assert!(matches!(
                next_payload(&params, &mut socket).await,
                proto::EncryptedPayload::Snapshot {
                    checkpoint: true,
                    exit: Some(7),
                    ..
                }
            ));
            let control = socket.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(matches!(
                serde_json::from_str(&control),
                Ok(proto::Producer::Exit { .. })
            ));
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            let (result, ()) = tokio::join!(run(&url, &params, &mut events_rx, None), server);
            result.unwrap();
        })
        .await
        .unwrap();
    }

    async fn next_payload(
        params: &Params,
        socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> proto::EncryptedPayload {
        loop {
            if let Message::Binary(bytes) = socket.next().await.unwrap().unwrap() {
                return decrypt(params, &bytes);
            }
        }
    }

    #[tokio::test]
    async fn reconnect_preserves_output_and_rejects_old_and_replayed_input() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let params = params();
        let key = params.key.as_ref().unwrap();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (writes_tx, writes_rx) = std::sync::mpsc::channel();
        let server = async {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            socket.next().await.unwrap().unwrap();
            let ack = serde_json::json!({ "session": "test-session", "url": "http://localhost/s/test-session", "encryption": 2, "history_bytes": 524288 });
            socket
                .send(Message::Text(ack.to_string().into()))
                .await
                .unwrap();
            let first = socket.next().await.unwrap().unwrap().into_data();
            let proto::EncryptedPayload::Snapshot {
                epoch,
                checkpoint: false,
                ..
            } = decrypt(&params, &first)
            else {
                panic!("snapshot")
            };
            let mut input = Sender::new(key, "test-session", true).unwrap();
            let old = input
                .seal(
                    crypto::INPUT,
                    &serde_json::to_vec(&proto::EncryptedPayload::Input {
                        epoch,
                        bytes: b"old".to_vec(),
                    })
                    .unwrap(),
                )
                .unwrap();
            socket.close(None).await.unwrap();
            drop(socket);
            events_tx
                .send(Event::Output(b"during-disconnect".to_vec()))
                .unwrap();
            events_tx
                .send(Event::Resize {
                    cols: 100,
                    rows: 30,
                })
                .unwrap();
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let hello: proto::Hello =
                serde_json::from_str(&socket.next().await.unwrap().unwrap().into_text().unwrap())
                    .unwrap();
            assert_eq!(hello.resume.as_deref(), Some("test-session"));
            socket
                .send(Message::Text(ack.to_string().into()))
                .await
                .unwrap();
            let next = socket.next().await.unwrap().unwrap().into_data();
            assert_eq!(
                crypto::header(&first).unwrap().writer,
                crypto::header(&next).unwrap().writer
            );
            assert!(
                crypto::header(&next).unwrap().sequence > crypto::header(&first).unwrap().sequence
            );
            let proto::EncryptedPayload::Snapshot {
                epoch,
                screen,
                cols,
                rows,
                checkpoint: false,
                ..
            } = decrypt(&params, &next)
            else {
                panic!("snapshot")
            };
            assert!(screen.contains("during-disconnect"));
            assert_eq!((cols, rows), (100, 30));
            socket.send(Message::Binary(old.into())).await.unwrap();
            socket
                .send(Message::Binary(b"plaintext".to_vec().into()))
                .await
                .unwrap();
            let valid = input
                .seal(
                    crypto::INPUT,
                    &serde_json::to_vec(&proto::EncryptedPayload::Input {
                        epoch,
                        bytes: b"accepted-once".to_vec(),
                    })
                    .unwrap(),
                )
                .unwrap();
            let mut bad = valid.clone();
            *bad.last_mut().unwrap() ^= 1;
            socket.send(Message::Binary(bad.into())).await.unwrap();
            socket
                .send(Message::Binary(valid.clone().into()))
                .await
                .unwrap();
            socket.send(Message::Binary(valid.into())).await.unwrap();
            socket
                .send(Message::Ping(Default::default()))
                .await
                .unwrap();
            loop {
                if matches!(socket.next().await.unwrap().unwrap(), Message::Pong(_)) {
                    break;
                }
            }
            events_tx.send(Event::Exit { code: 7 }).unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if let Message::Text(text) = message {
                    if matches!(
                        serde_json::from_str(&text),
                        Ok(proto::Producer::Exit { .. })
                    ) {
                        break;
                    }
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            let (result, ()) =
                tokio::join!(run(&url, &params, &mut events_rx, Some(&writes_tx)), server);
            result.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(writes_rx.try_recv().unwrap(), b"accepted-once");
        assert!(writes_rx.try_recv().is_err());
    }
}
