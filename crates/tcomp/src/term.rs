use anyhow::Result;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

static RAW_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn size() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => (80, 24),
    }
}

pub fn restore() {
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
