use anyhow::Result;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

static RAW_ACTIVE: AtomicBool = AtomicBool::new(false);

struct Output {
    held: Option<Vec<u8>>,
    at_line_start: bool,
}

static OUTPUT: Mutex<Output> = Mutex::new(Output {
    held: Some(Vec::new()),
    at_line_start: true,
});

fn output() -> MutexGuard<'static, Output> {
    OUTPUT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Output {
    fn emit(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        if bytes.is_empty() {
            return Ok(());
        }
        let mut stdout = std::io::stdout();
        stdout.write_all(bytes)?;
        stdout.flush()?;
        self.at_line_start = bytes.last() == Some(&b'\n');
        Ok(())
    }

    fn flush_held(&mut self) {
        if let Some(held) = self.held.take() {
            let _ = self.emit(&held);
        }
    }
}

pub fn size() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => (80, 24),
    }
}

/// Child output, withheld until the first note lands so the two never share a line.
pub fn child_output(chunk: &[u8]) -> std::io::Result<()> {
    let mut output = output();
    if let Some(held) = output.held.as_mut() {
        held.extend_from_slice(chunk);
        return Ok(());
    }
    output.emit(chunk)
}

/// Give up waiting for a note and let child output through.
pub fn release_output() {
    output().flush_held();
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
    output.flush_held();
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
    fn released_output_is_written_once() {
        let mut output = Output {
            held: Some(b"prompt".to_vec()),
            at_line_start: true,
        };
        output.flush_held();
        assert!(output.held.is_none());
        output.flush_held();
    }
}
