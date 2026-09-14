use anyhow::Result;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

static RAW_ACTIVE: AtomicBool = AtomicBool::new(false);

pub const NOTE_HOLD: Duration = Duration::from_millis(500);
pub const LINK_LINGER: Duration = Duration::from_secs(1);

const ALT_SCREENS: [&[u8]; 3] = [b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];

enum Gate {
    Waiting(Vec<u8>),
    Linger {
        until: Instant,
        held: Vec<u8>,
        blocked: bool,
    },
    Open,
}

struct Output {
    gate: Gate,
    at_line_start: bool,
}

static OUTPUT: Mutex<Output> = Mutex::new(Output {
    gate: Gate::Waiting(Vec::new()),
    at_line_start: true,
});

fn output() -> MutexGuard<'static, Output> {
    OUTPUT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_out(bytes: &[u8], at_line_start: &mut bool) -> std::io::Result<()> {
    use std::io::Write;
    if bytes.is_empty() {
        return Ok(());
    }
    let mut stdout = std::io::stdout();
    stdout.write_all(bytes)?;
    stdout.flush()?;
    *at_line_start = bytes.last() == Some(&b'\n');
    Ok(())
}

fn alt_screen_at(chunk: &[u8]) -> Option<usize> {
    ALT_SCREENS
        .iter()
        .filter_map(|enter| {
            chunk
                .windows(enter.len())
                .position(|window| window == *enter)
        })
        .min()
}

fn alt_prefix_len(chunk: &[u8]) -> usize {
    (1..ALT_SCREENS
        .iter()
        .map(|enter| enter.len())
        .max()
        .unwrap_or_default())
        .rev()
        .find(|&len| {
            len <= chunk.len()
                && ALT_SCREENS
                    .iter()
                    .any(|enter| enter.starts_with(&chunk[chunk.len() - len..]))
        })
        .unwrap_or_default()
}

impl Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        write_out(bytes, &mut self.at_line_start)
    }

    fn open(&mut self) -> std::io::Result<()> {
        let held = match std::mem::replace(&mut self.gate, Gate::Open) {
            Gate::Waiting(held) | Gate::Linger { held, .. } => held,
            Gate::Open => return Ok(()),
        };
        self.write(&held)
    }

    fn linger_chunk(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        let mut held = match &mut self.gate {
            Gate::Linger { held, blocked, .. } => {
                if *blocked {
                    held.extend_from_slice(chunk);
                    return Ok(());
                }
                std::mem::take(held)
            }
            _ => unreachable!(),
        };
        held.extend_from_slice(chunk);

        if let Some(at) = alt_screen_at(&held) {
            self.write(&held[..at])?;
            if let Gate::Linger {
                held: pending,
                blocked,
                ..
            } = &mut self.gate
            {
                pending.extend_from_slice(&held[at..]);
                *blocked = true;
            }
            return Ok(());
        }

        let split = held.len() - alt_prefix_len(&held);
        self.write(&held[..split])?;
        if let Gate::Linger { held: pending, .. } = &mut self.gate {
            pending.extend_from_slice(&held[split..]);
        }
        Ok(())
    }

    fn start_linger(&mut self) -> std::io::Result<()> {
        let now = Instant::now();
        let held = match std::mem::replace(&mut self.gate, Gate::Open) {
            Gate::Waiting(held) => held,
            Gate::Linger { held, blocked, .. } => {
                self.gate = Gate::Linger {
                    until: now + LINK_LINGER,
                    held,
                    blocked,
                };
                return Ok(());
            }
            Gate::Open => Vec::new(),
        };
        self.gate = Gate::Linger {
            until: now + LINK_LINGER,
            held: Vec::new(),
            blocked: false,
        };
        self.linger_chunk(&held)
    }
}

pub fn size() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => (80, 24),
    }
}

pub fn child_output(chunk: &[u8]) -> std::io::Result<()> {
    let mut output = output();
    match &mut output.gate {
        Gate::Waiting(held) => {
            held.extend_from_slice(chunk);
            Ok(())
        }
        Gate::Linger { .. } => output.linger_chunk(chunk),
        Gate::Open => output.write(chunk),
    }
}

pub fn give_up_waiting() -> std::io::Result<()> {
    let mut output = output();
    if matches!(output.gate, Gate::Waiting(_)) {
        output.open()?;
    }
    Ok(())
}

pub fn end_linger() -> std::io::Result<()> {
    let mut output = output();
    if let Gate::Linger { until, .. } = &output.gate {
        if *until > Instant::now() {
            return Ok(());
        }
        output.open()?;
    }
    Ok(())
}

pub fn release_output() {
    let _ = output().open();
}

/// True when stderr is a terminal that is likely to understand SGR sequences.
fn styled() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
}

/// Print a tcomp status line to stderr — dim label, coloured body.
/// `\r\n` because the local terminal is in raw mode while a session runs.
pub fn note(kind: Note, body: &str) {
    note_with_url(kind, body, None)
}

/// Same as [`note`], with a trailing URL that gets underlined when styling is on.
pub fn note_with_url(kind: Note, body: &str, url: Option<&str>) {
    use std::io::Write;
    let mut output = output();
    let line = render_note(kind, body, url, styled(), output.at_line_start);
    let mut err = std::io::stderr();
    let _ = err.write_all(line.as_bytes());
    let _ = err.flush();
    output.at_line_start = true;
    if url.is_some() {
        let _ = output.start_linger();
    }
}

/// The same line [`note`] prints, styled for replay into a remote viewer.
pub fn styled_line(kind: Note, body: &str) -> String {
    render_note(kind, body, None, true, false)
}

fn render_note(
    kind: Note,
    body: &str,
    url: Option<&str>,
    styled: bool,
    at_line_start: bool,
) -> String {
    let lead = if at_line_start { "" } else { "\r\n" };
    if !styled {
        return match url {
            Some(url) => format!("{lead}tcomp: {body} {url}\r\n"),
            None => format!("{lead}tcomp: {body}\r\n"),
        };
    }
    let (colour, glyph) = match kind {
        Note::Info => ("36", "●"), // cyan
        Note::Good => ("32", "▶"), // green
        Note::Warn => ("33", "▲"), // yellow
    };
    let tail = match url {
        Some(url) => format!(" \x1b[4;36m{url}\x1b[0m"),
        None => String::new(),
    };
    format!("{lead}\x1b[2mtcomp\x1b[0m \x1b[{colour}m{glyph}\x1b[0m {body}{tail}\r\n")
}

#[derive(Clone, Copy)]
pub enum Note {
    /// Neutral status; no caller yet, kept so the three levels stay symmetric.
    #[allow(dead_code)]
    Info,
    Good,
    Warn,
}

pub fn restore() {
    release_output();
    if RAW_ACTIVE.swap(false, Ordering::SeqCst) {
        use std::io::Write;
        let cleanup = crate::modes::cleanup();
        let mut stdout = std::io::stdout();
        if !cleanup.is_empty() {
            let _ = stdout.write_all(&cleanup);
            let _ = stdout.flush();
        }
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

pub struct RawGuard;

impl RawGuard {
    pub fn new() -> Result<Self> {
        if std::io::stdin().is_terminal() {
            crossterm::terminal::enable_raw_mode()?;
            RAW_ACTIVE.store(true, Ordering::SeqCst);
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
        }
        Ok(Self)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_output_carries_no_escapes() {
        let line = render_note(Note::Good, "watch at", Some("http://x/s/1"), false, true);
        assert_eq!(line, "tcomp: watch at http://x/s/1\r\n");
        assert!(!line.contains('\x1b'));
    }

    #[test]
    fn styled_output_underlines_the_url_and_resets() {
        let line = render_note(Note::Good, "watch at", Some("http://x/s/1"), true, true);
        assert!(line.starts_with("\x1b[2mtcomp\x1b[0m \x1b[32m▶\x1b[0m watch at "));
        assert!(line.contains("\x1b[4;36mhttp://x/s/1\x1b[0m"));
        assert!(line.ends_with("\r\n"));
    }

    #[test]
    fn every_line_ends_with_crlf_for_raw_mode() {
        for styled in [true, false] {
            for kind in [Note::Info, Note::Good, Note::Warn] {
                assert!(render_note(kind, "x", None, styled, true).ends_with("\r\n"));
            }
        }
    }

    #[test]
    fn a_note_never_lands_on_a_half_written_prompt() {
        for styled in [true, false] {
            let line = render_note(Note::Good, "watch at", Some("http://x/s/1"), styled, false);
            assert!(line.starts_with("\r\n"));
        }
    }

    #[test]
    fn a_viewer_line_always_breaks_before_itself() {
        assert!(styled_line(Note::Warn, "bash exited (0)").starts_with("\r\n"));
    }

    #[test]
    fn finds_alternate_screen_sequences() {
        assert_eq!(alt_screen_at(b"\x1b[?1049h"), Some(0));
        assert_eq!(alt_screen_at(b"hi\x1b[?1047h"), Some(2));
        assert_eq!(alt_screen_at(b"hi\x1b[?47hbye"), Some(2));
        assert_eq!(alt_screen_at(b"\x1b[?2004h[~] $ "), None);
    }

    #[test]
    fn retains_only_a_possible_alternate_screen_prefix() {
        assert_eq!(alt_prefix_len(b"plain text"), 0);
        assert_eq!(alt_prefix_len(b"x\x1b"), 1);
        assert_eq!(alt_prefix_len(b"x\x1b[?10"), 5);
        assert_eq!(alt_prefix_len(b"\x1b[?1049h"), 0);
    }

    #[test]
    fn link_linger_holds_a_full_screen_switch() {
        let mut output = Output {
            gate: Gate::Waiting(b"\x1b[?1049happ".to_vec()),
            at_line_start: true,
        };
        output.start_linger().unwrap();
        match &output.gate {
            Gate::Linger {
                held,
                blocked: true,
                ..
            } => assert_eq!(held, b"\x1b[?1049happ"),
            _ => panic!("alternate screen should have been withheld"),
        }
    }

    #[test]
    fn link_linger_handles_a_split_screen_switch() {
        let mut output = Output {
            gate: Gate::Linger {
                until: Instant::now() + LINK_LINGER,
                held: b"\x1b[?10".to_vec(),
                blocked: false,
            },
            at_line_start: true,
        };
        output.linger_chunk(b"49happ").unwrap();
        match &output.gate {
            Gate::Linger {
                held,
                blocked: true,
                ..
            } => assert_eq!(held, b"\x1b[?1049happ"),
            _ => panic!("alternate screen should have been withheld"),
        }
    }

    #[test]
    fn a_new_link_extends_the_linger() {
        let first = Instant::now();
        let mut output = Output {
            gate: Gate::Linger {
                until: first,
                held: Vec::new(),
                blocked: false,
            },
            at_line_start: true,
        };
        output.start_linger().unwrap();
        match output.gate {
            Gate::Linger { until, .. } => assert!(until > first),
            _ => panic!("linger should remain active"),
        }
    }
}
