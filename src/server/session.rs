use crate::server::config::Config;
use bytes::Bytes;
use dashmap::DashMap;
use proto::{SessionInfo, Status};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;

const BROADCAST_CAPACITY: usize = 2048;
const MAX_COLS: u16 = 1000;
const MAX_ROWS: u16 = 500;
const PREVIEW_LINES: usize = 24;
const RESET: &[u8] = b"\x1bc";

#[derive(Clone, Debug)]
pub enum Frame {
    Data(Bytes),
    Ctrl(proto::Server),
}

pub struct Session {
    pub id: String,
    pub tx: broadcast::Sender<Frame>,
    state: Mutex<State>,
}

struct State {
    name: String,
    cmd: String,
    cols: u16,
    rows: u16,
    status: Status,
    started_at: SystemTime,
    updated_at: SystemTime,
    terminal_at: Option<SystemTime>,
    exit_code: Option<i32>,
    vt: avt::Vt,
    history: VecDeque<Bytes>,
    history_len: usize,
    history_cap: usize,
    carry: Vec<u8>,
    epoch: u64,
    allow_input: bool,
    input: Option<UnboundedSender<Bytes>>,
    banner: Option<String>,
}

pub struct Join {
    pub init: proto::Server,
    pub banner: Option<proto::Server>,
    pub payload: Vec<Bytes>,
    pub rx: broadcast::Receiver<Frame>,
}

impl Session {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn new(
        id: String,
        name: String,
        cmd: String,
        cols: u16,
        rows: u16,
        allow_input: bool,
        config: &Config,
    ) -> Self {
        let (cols, rows) = clamp_size(cols, rows);
        let vt = avt::Vt::builder()
            .size(cols as usize, rows as usize)
            .scrollback_limit(config.scrollback)
            .build();
        let now = SystemTime::now();
        Self {
            id,
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
            state: Mutex::new(State {
                name,
                cmd,
                cols,
                rows,
                status: Status::Live,
                started_at: now,
                updated_at: now,
                terminal_at: None,
                exit_code: None,
                vt,
                history: VecDeque::new(),
                history_len: 0,
                history_cap: config.history_bytes,
                carry: Vec::new(),
                epoch: 0,
                allow_input,
                input: None,
                banner: None,
            }),
        }
    }

    pub fn feed(&self, chunk: Bytes) {
        let mut state = self.lock();
        let mut carry = std::mem::take(&mut state.carry);
        carry.extend_from_slice(&chunk);
        decode_into(&mut state.vt, &mut carry);
        if carry.len() > 8 {
            carry.clear();
        }
        state.carry = carry;
        state.updated_at = SystemTime::now();

        state.history_len += chunk.len();
        state.history.push_back(chunk.clone());
        while state.history_len > state.history_cap && state.history.len() > 1 {
            if let Some(front) = state.history.pop_front() {
                state.history_len -= front.len();
            }
        }

        let _ = self.tx.send(Frame::Data(chunk));
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let (cols, rows) = clamp_size(cols, rows);
        let mut state = self.lock();
        if state.cols == cols && state.rows == rows {
            return;
        }
        state.vt.resize(cols as usize, rows as usize);
        state.cols = cols;
        state.rows = rows;
        state.updated_at = SystemTime::now();
        let dump = dump_bytes(&state.vt);
        let _ = self
            .tx
            .send(Frame::Ctrl(proto::Server::Resize { cols, rows }));
        let _ = self.tx.send(Frame::Data(dump));
    }

    pub fn join(&self) -> Join {
        let state = self.lock();
        let init = proto::Server::Init {
            cols: state.cols,
            rows: state.rows,
            status: state.status,
            name: state.name.clone(),
            cmd: state.cmd.clone(),
            input: state.allow_input,
        };
        let banner = state
            .banner
            .clone()
            .map(|text| proto::Server::Banner { text });
        let payload = if state.status == Status::Live || state.history.is_empty() {
            vec![dump_bytes(&state.vt)]
        } else {
            let mut out = Vec::with_capacity(state.history.len() + 1);
            out.push(Bytes::from_static(RESET));
            out.extend(state.history.iter().cloned());
            out
        };
        let rx = self.tx.subscribe();
        Join {
            init,
            banner,
            payload,
            rx,
        }
    }

    /// Set the session banner and push it to viewers already connected.
    pub fn set_banner(&self, text: String) {
        self.lock().banner = Some(text.clone());
        let _ = self.tx.send(Frame::Ctrl(proto::Server::Banner { text }));
    }

    pub fn dump(&self) -> Bytes {
        let state = self.lock();
        dump_bytes(&state.vt)
    }

    pub fn mark_exit(&self, code: i32) {
        let mut state = self.lock();
        state.status = Status::Ended;
        state.input = None;
        state.exit_code = Some(code);
        state.terminal_at = Some(SystemTime::now());
        state.updated_at = SystemTime::now();
        let _ = self
            .tx
            .send(Frame::Ctrl(proto::Server::Exit { code: Some(code) }));
    }

    pub fn mark_disconnected(&self, epoch: u64) {
        let mut state = self.lock();
        if state.epoch != epoch || state.status != Status::Live {
            return;
        }
        state.input = None;
        state.status = Status::Stale;
        state.terminal_at = Some(SystemTime::now());
        let _ = self.tx.send(Frame::Ctrl(proto::Server::Status {
            status: Status::Stale,
        }));
    }

    pub fn resume(&self, cols: u16, rows: u16, allow_input: bool) -> u64 {
        let mut state = self.lock();
        state.epoch += 1;
        state.allow_input = allow_input;
        state.input = None;
        state.status = Status::Live;
        state.terminal_at = None;
        state.exit_code = None;
        state.updated_at = SystemTime::now();
        state.carry.clear();
        let epoch = state.epoch;
        drop(state);
        let _ = self.tx.send(Frame::Ctrl(proto::Server::Status {
            status: Status::Live,
        }));
        self.resize(cols, rows);
        epoch
    }

    pub fn allows_input(&self) -> bool {
        self.lock().allow_input
    }

    pub fn attach_input(&self, epoch: u64, tx: UnboundedSender<Bytes>) {
        let mut state = self.lock();
        if state.epoch == epoch {
            state.input = Some(tx);
        }
    }

    pub fn detach_input(&self, epoch: u64) {
        let mut state = self.lock();
        if state.epoch == epoch {
            state.input = None;
        }
    }

    pub fn send_input(&self, bytes: Bytes) -> bool {
        let state = self.lock();
        if !state.allow_input || state.status != Status::Live {
            return false;
        }
        match state.input.as_ref() {
            Some(tx) => tx.send(bytes).is_ok(),
            None => false,
        }
    }

    pub fn epoch(&self) -> u64 {
        self.lock().epoch
    }

    pub fn status(&self) -> Status {
        self.lock().status
    }

    pub fn expires_in(&self, config: &Config) -> Option<Duration> {
        let state = self.lock();
        let ttl = match state.status {
            Status::Live => return None,
            Status::Ended => config.ended_ttl,
            Status::Stale => config.stale_ttl,
        };
        let deadline = state.terminal_at? + ttl;
        deadline.duration_since(SystemTime::now()).ok()
    }

    pub fn info(&self, config: &Config) -> SessionInfo {
        let expires_in = self.expires_in(config).map(|d| d.as_secs());
        let state = self.lock();
        SessionInfo {
            id: self.id.clone(),
            name: state.name.clone(),
            cmd: state.cmd.clone(),
            cols: state.cols,
            rows: state.rows,
            status: state.status,
            input: state.allow_input,
            started_at: unix(state.started_at),
            updated_at: unix(state.updated_at),
            expires_in,
            preview: preview(&state.vt),
        }
    }
}

fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(1, MAX_COLS), rows.clamp(1, MAX_ROWS))
}

fn preview(vt: &avt::Vt) -> String {
    let mut lines: Vec<String> = vt
        .view()
        .map(|line| {
            let cleaned: String = line
                .text()
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            cleaned.trim_end().to_string()
        })
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(PREVIEW_LINES);
    lines[start..].join("\n")
}

fn dump_bytes(vt: &avt::Vt) -> Bytes {
    let mut out = Vec::from(RESET);
    out.extend_from_slice(vt.dump().as_bytes());
    Bytes::from(out)
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn decode_into(vt: &mut avt::Vt, carry: &mut Vec<u8>) {
    loop {
        match std::str::from_utf8(carry) {
            Ok(text) => {
                if !text.is_empty() {
                    vt.feed_str(text);
                }
                carry.clear();
                return;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    vt.feed_str(std::str::from_utf8(&carry[..valid]).unwrap());
                }
                match error.error_len() {
                    Some(bad) => {
                        vt.feed_str("\u{fffd}");
                        carry.drain(..valid + bad);
                    }
                    None => {
                        carry.drain(..valid);
                        return;
                    }
                }
            }
        }
    }
}

#[derive(Clone, Default)]
pub struct Store {
    sessions: Arc<DashMap<String, Arc<Session>>>,
}

impl Store {
    pub fn insert(&self, session: Arc<Session>) {
        self.sessions.insert(session.id.clone(), session);
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.get(id).map(|s| s.clone())
    }

    pub fn list(&self, config: &Config) -> Vec<SessionInfo> {
        let mut out: Vec<SessionInfo> = self
            .sessions
            .iter()
            .map(|entry| entry.value().info(config))
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        out
    }

    pub fn reap(&self, config: &Config) -> usize {
        let expired: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| {
                entry.value().status().is_terminal() && entry.value().expires_in(config).is_none()
            })
            .map(|entry| entry.key().clone())
            .collect();
        for id in &expired {
            self.sessions.remove(id);
        }
        expired.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            bind: "127.0.0.1:0".into(),
            public_url: "http://relay.test".into(),
            web_dir: "web".into(),
            ended_ttl: Duration::from_secs(60),
            stale_ttl: Duration::from_secs(4 * 60 * 60),
            producer_timeout: Duration::from_secs(60),
            scrollback: 100,
            history_bytes: 64,
            token: None,
            token_in_links: false,
        }
    }

    fn session() -> Session {
        Session::new(
            "s1".into(),
            "box".into(),
            "sh".into(),
            20,
            5,
            false,
            &config(),
        )
    }

    fn screen(session: &Session) -> String {
        preview(&session.lock().vt)
    }

    #[test]
    fn utf8_sequence_split_across_chunks_is_not_corrupted() {
        let session = session();
        let text = "日本語".as_bytes().to_vec();
        for byte in text {
            session.feed(Bytes::from(vec![byte]));
        }
        assert_eq!(screen(&session), "日本語");
    }

    #[test]
    fn invalid_utf8_does_not_poison_later_output() {
        let session = session();
        session.feed(Bytes::from_static(b"ab\xffcd"));
        let screen = screen(&session);
        assert!(screen.starts_with("ab"), "got {screen:?}");
        assert!(screen.ends_with("cd"), "got {screen:?}");
    }

    #[test]
    fn sequence_split_across_two_chunks_is_reassembled() {
        let session = session();
        session.feed(Bytes::from_static(b"\xe6\x97"));
        session.feed(Bytes::from_static(b"\xa5 ok"));
        assert_eq!(screen(&session), "日 ok");
    }

    #[test]
    fn viewers_receive_original_bytes_verbatim() {
        let session = session();
        let mut rx = session.tx.subscribe();
        let raw = Bytes::from_static(b"\x1b[32mgreen\x1b[0m");
        session.feed(raw.clone());
        match rx.try_recv().unwrap() {
            Frame::Data(bytes) => assert_eq!(bytes, raw),
            other => panic!("expected data, got {other:?}"),
        }
    }

    #[test]
    fn zero_and_absurd_dimensions_are_clamped() {
        assert_eq!(clamp_size(0, 0), (1, 1));
        assert_eq!(clamp_size(u16::MAX, u16::MAX), (MAX_COLS, MAX_ROWS));
        assert_eq!(clamp_size(80, 24), (80, 24));
    }

    #[test]
    fn session_survives_zero_rows_from_producer() {
        let session = Session::new(
            "z".into(),
            "box".into(),
            "sh".into(),
            0,
            0,
            false,
            &config(),
        );
        session.feed(Bytes::from_static(b"hi"));
        session.resize(0, 0);
        assert_eq!(session.status(), Status::Live);
    }

    #[test]
    fn history_ring_is_bounded() {
        let session = session();
        for _ in 0..50 {
            session.feed(Bytes::from_static(b"0123456789"));
        }
        let state = session.lock();
        assert!(
            state.history_len <= state.history_cap + 10,
            "{}",
            state.history_len
        );
    }

    #[test]
    fn clean_exit_is_ended_and_unclean_drop_is_stale() {
        let ended = session();
        ended.mark_exit(0);
        assert_eq!(ended.status(), Status::Ended);

        let dropped = session();
        dropped.mark_disconnected(dropped.epoch());
        assert_eq!(dropped.status(), Status::Stale);
    }

    #[test]
    fn exit_frame_wins_over_a_later_socket_drop() {
        let session = session();
        let epoch = session.epoch();
        session.mark_exit(3);
        session.mark_disconnected(epoch);
        assert_eq!(session.status(), Status::Ended);
    }

    #[test]
    fn stale_session_outlives_an_ended_one() {
        let config = config();
        let ended = session();
        ended.mark_exit(0);
        let stale = session();
        stale.mark_disconnected(stale.epoch());

        let ended_ttl = ended.expires_in(&config).unwrap();
        let stale_ttl = stale.expires_in(&config).unwrap();
        assert!(ended_ttl <= config.ended_ttl);
        assert!(stale_ttl > ended_ttl);
    }

    #[test]
    fn live_session_never_expires() {
        assert!(session().expires_in(&config()).is_none());
    }

    #[test]
    fn resume_revives_a_stale_session_without_losing_history() {
        let session = session();
        session.feed(Bytes::from_static(b"before"));
        session.mark_disconnected(session.epoch());
        assert_eq!(session.status(), Status::Stale);

        let epoch = session.resume(20, 5, false);
        assert_eq!(session.status(), Status::Live);
        assert!(screen(&session).contains("before"));

        session.mark_disconnected(epoch - 1);
        assert_eq!(session.status(), Status::Live);
    }

    #[test]
    fn stale_viewers_get_history_and_live_viewers_get_a_dump() {
        let live = session();
        live.feed(Bytes::from_static(b"hello"));
        let payload = live.join().payload;
        assert_eq!(payload.len(), 1, "live join should be a single dump");

        let stale = session();
        stale.feed(Bytes::from_static(b"hello"));
        stale.mark_disconnected(stale.epoch());
        assert!(
            stale.join().payload.len() > 1,
            "stale join should replay history"
        );
    }

    #[test]
    fn preview_reflects_the_alternate_screen() {
        let session = session();
        session.feed(Bytes::from_static(b"primary text"));
        session.feed(Bytes::from_static(b"\x1b[?1049h\x1b[HFULLSCREEN APP"));
        assert!(screen(&session).contains("FULLSCREEN APP"));
    }

    #[test]
    fn preview_is_not_padded_to_the_terminal_width() {
        let session = session();
        session.feed(Bytes::from_static(b"hi"));
        assert_eq!(screen(&session), "hi");
    }

    #[test]
    fn a_lagging_viewer_can_resync_from_an_accurate_dump() {
        let session = session();
        let mut rx = session.tx.subscribe();

        for i in 0..(BROADCAST_CAPACITY + 100) {
            session.feed(Bytes::from(format!("\r\nline {i}")));
        }

        assert!(
            matches!(
                rx.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
            ),
            "viewer should have lagged once the channel overflowed"
        );

        let dump = String::from_utf8(session.dump().to_vec()).unwrap();
        let mut replay = avt::Vt::new(20, 5);
        replay.feed_str(&dump);
        let replayed: Vec<String> = replay.view().map(|line| line.text()).collect();
        let expected = format!("line {}", BROADCAST_CAPACITY + 99);
        assert!(
            replayed.iter().any(|line| line.contains(&expected)),
            "resync dump should show the current screen, got {replayed:?}"
        );
    }

    fn interactive() -> Session {
        Session::new(
            "i1".into(),
            "box".into(),
            "sh".into(),
            20,
            5,
            true,
            &config(),
        )
    }

    #[test]
    fn input_is_refused_unless_the_host_opted_in() {
        let session = session();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        session.attach_input(session.epoch(), tx);

        assert!(!session.send_input(Bytes::from_static(b"rm -rf /")));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn input_reaches_an_attached_producer_when_allowed() {
        let session = interactive();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        session.attach_input(session.epoch(), tx);

        assert!(session.send_input(Bytes::from_static(b"ls\r")));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"ls\r"));
    }

    #[test]
    fn input_is_dropped_once_the_producer_is_gone() {
        let session = interactive();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let epoch = session.epoch();
        session.attach_input(epoch, tx);

        session.mark_disconnected(epoch);
        assert!(!session.send_input(Bytes::from_static(b"whoami")));

        let ended = interactive();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        ended.attach_input(ended.epoch(), tx);
        ended.mark_exit(0);
        assert!(!ended.send_input(Bytes::from_static(b"whoami")));
    }

    #[test]
    fn a_stale_producers_input_channel_does_not_outlive_a_resume() {
        let session = interactive();
        let (old_tx, mut old_rx) = tokio::sync::mpsc::unbounded_channel();
        let old_epoch = session.epoch();
        session.attach_input(old_epoch, old_tx);
        session.mark_disconnected(old_epoch);

        let new_epoch = session.resume(20, 5, true);
        session.attach_input(old_epoch, tokio::sync::mpsc::unbounded_channel().0);
        assert!(!session.send_input(Bytes::from_static(b"stale")));

        let (new_tx, mut new_rx) = tokio::sync::mpsc::unbounded_channel();
        session.attach_input(new_epoch, new_tx);
        assert!(session.send_input(Bytes::from_static(b"fresh")));
        assert_eq!(new_rx.try_recv().unwrap(), Bytes::from_static(b"fresh"));
        assert!(old_rx.try_recv().is_err());
    }

    #[test]
    fn resume_can_revoke_input() {
        let session = interactive();
        assert!(session.allows_input());
        session.mark_disconnected(session.epoch());

        let epoch = session.resume(20, 5, false);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        session.attach_input(epoch, tx);
        assert!(!session.allows_input());
        assert!(!session.send_input(Bytes::from_static(b"nope")));
    }

    #[test]
    fn reaper_removes_expired_sessions_only() {
        let mut config = config();
        config.ended_ttl = Duration::ZERO;
        let store = Store::default();

        let live = Arc::new(session());
        let gone = Arc::new(Session::new(
            "s2".into(),
            "b".into(),
            "sh".into(),
            20,
            5,
            false,
            &config,
        ));
        gone.mark_exit(0);
        store.insert(live.clone());
        store.insert(gone.clone());

        assert_eq!(store.reap(&config), 1);
        assert!(store.get("s1").is_some());
        assert!(store.get("s2").is_none());
    }
}
