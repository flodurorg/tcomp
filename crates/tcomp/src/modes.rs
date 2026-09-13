use std::sync::atomic::{AtomicU32, Ordering};

const ALT_1049: u32 = 1 << 0;
const ALT_1047: u32 = 1 << 1;
const ALT_47: u32 = 1 << 2;
const BRACKETED: u32 = 1 << 3;
const MOUSE_1000: u32 = 1 << 4;
const MOUSE_1002: u32 = 1 << 5;
const MOUSE_1003: u32 = 1 << 6;
const MOUSE_1005: u32 = 1 << 7;
const MOUSE_1006: u32 = 1 << 8;
const MOUSE_1015: u32 = 1 << 9;
const CURSOR_HIDDEN: u32 = 1 << 10;
const MODIFY_KEYS: u32 = 1 << 11;
const NO_AUTOWRAP: u32 = 1 << 12;

static ACTIVE: AtomicU32 = AtomicU32::new(0);
static KITTY_DEPTH: AtomicU32 = AtomicU32::new(0);

fn set(bit: u32, on: bool) {
    if on {
        ACTIVE.fetch_or(bit, Ordering::SeqCst);
    } else {
        ACTIVE.fetch_and(!bit, Ordering::SeqCst);
    }
}

/// Consumes complete escape sequences from `buf`, leaving any partial tail.
pub fn observe(buf: &mut Vec<u8>, chunk: &[u8]) {
    buf.extend_from_slice(chunk);
    let mut at = 0;
    while at < buf.len() {
        if buf[at] != 0x1b {
            at += 1;
            continue;
        }
        match sequence_len(&buf[at..]) {
            Some(len) => {
                apply(&buf[at..at + len]);
                at += len;
            }
            None => break,
        }
    }
    buf.drain(..at);
    if buf.len() > 128 {
        buf.clear();
    }
}

fn sequence_len(bytes: &[u8]) -> Option<usize> {
    match bytes.get(1)? {
        b'[' => {
            let mut i = 2;
            while i < bytes.len() && (0x20..=0x3f).contains(&bytes[i]) {
                i += 1;
            }
            let final_byte = *bytes.get(i)?;
            if (0x40..=0x7e).contains(&final_byte) {
                Some(i + 1)
            } else {
                None
            }
        }
        b']' => {
            let mut i = 2;
            while i < bytes.len() {
                if bytes[i] == 0x07 {
                    return Some(i + 1);
                }
                if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                    return Some(i + 2);
                }
                i += 1;
            }
            None
        }
        _ => Some(2),
    }
}

fn apply(seq: &[u8]) {
    if seq.len() < 3 || seq[1] != b'[' {
        return;
    }
    let body = &seq[2..seq.len() - 1];
    let final_byte = seq[seq.len() - 1];

    match (body.first(), final_byte) {
        (Some(b'?'), b'h') | (Some(b'?'), b'l') => {
            let on = final_byte == b'h';
            for param in body[1..].split(|b| *b == b';') {
                match param {
                    b"1049" => set(ALT_1049, on),
                    b"1047" => set(ALT_1047, on),
                    b"47" => set(ALT_47, on),
                    b"2004" => set(BRACKETED, on),
                    b"1000" => set(MOUSE_1000, on),
                    b"1002" => set(MOUSE_1002, on),
                    b"1003" => set(MOUSE_1003, on),
                    b"1005" => set(MOUSE_1005, on),
                    b"1006" => set(MOUSE_1006, on),
                    b"1015" => set(MOUSE_1015, on),
                    b"25" => set(CURSOR_HIDDEN, !on),
                    b"7" => set(NO_AUTOWRAP, !on),
                    _ => {}
                }
            }
        }
        (Some(b'>'), b'u') => {
            KITTY_DEPTH.fetch_add(1, Ordering::SeqCst);
        }
        (Some(b'<'), b'u') => {
            let _ = KITTY_DEPTH.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |depth| {
                Some(depth.saturating_sub(1))
            });
        }
        (Some(b'>'), b'm') => {
            let off = body[1..].is_empty() || body[1..].ends_with(b";0");
            set(MODIFY_KEYS, !off);
        }
        _ => {}
    }
}

/// Sequences undoing exactly the modes the child turned on and left on.
pub fn cleanup() -> Vec<u8> {
    let active = ACTIVE.load(Ordering::SeqCst);
    let mut out = Vec::new();

    for _ in 0..KITTY_DEPTH.swap(0, Ordering::SeqCst).min(16) {
        out.extend_from_slice(b"\x1b[<u");
    }
    if active & MODIFY_KEYS != 0 {
        out.extend_from_slice(b"\x1b[>4;0m");
    }
    for (bit, seq) in [
        (MOUSE_1000, &b"\x1b[?1000l"[..]),
        (MOUSE_1002, &b"\x1b[?1002l"[..]),
        (MOUSE_1003, &b"\x1b[?1003l"[..]),
        (MOUSE_1005, &b"\x1b[?1005l"[..]),
        (MOUSE_1006, &b"\x1b[?1006l"[..]),
        (MOUSE_1015, &b"\x1b[?1015l"[..]),
        (BRACKETED, &b"\x1b[?2004l"[..]),
        (NO_AUTOWRAP, &b"\x1b[?7h"[..]),
        (CURSOR_HIDDEN, &b"\x1b[?25h"[..]),
        (ALT_47, &b"\x1b[?47l"[..]),
        (ALT_1047, &b"\x1b[?1047l"[..]),
        (ALT_1049, &b"\x1b[?1049l"[..]),
    ] {
        if active & bit != 0 {
            out.extend_from_slice(seq);
        }
    }
    if !out.is_empty() {
        out.extend_from_slice(b"\x1b[0m");
    }
    ACTIVE.store(0, Ordering::SeqCst);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests share global statics; serialize them to prevent races.
    static LOCK: Mutex<()> = Mutex::new(());

    fn reset() -> std::sync::MutexGuard<'static, ()> {
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        ACTIVE.store(0, Ordering::SeqCst);
        KITTY_DEPTH.store(0, Ordering::SeqCst);
        guard
    }

    fn feed(chunks: &[&[u8]]) -> Vec<u8> {
        let mut buf = Vec::new();
        for chunk in chunks {
            observe(&mut buf, chunk);
        }
        cleanup()
    }

    #[test]
    fn a_quiet_child_needs_no_cleanup() {
        let _g = reset();
        assert!(feed(&[b"hello world\r\n"]).is_empty());
    }

    #[test]
    fn an_app_that_cleaned_up_after_itself_needs_no_cleanup() {
        let _g = reset();
        assert!(feed(&[b"\x1b[?1049h", b"drawing", b"\x1b[?1049l"]).is_empty());
    }

    #[test]
    fn a_killed_full_screen_app_gets_the_alt_screen_undone() {
        let _g = reset();
        let out = feed(&[b"\x1b[?1049h", b"drawing"]);
        assert_eq!(out, b"\x1b[?1049l\x1b[0m");
    }

    #[test]
    fn cleanup_never_leaves_the_alt_screen_that_was_not_entered() {
        let _g = reset();
        let out = feed(&[b"\x1b[?25l", b"text"]);
        assert!(!out.windows(4).any(|w| w == b"1049"), "got {out:?}");
        assert!(out.starts_with(b"\x1b[?25h"));
    }

    #[test]
    fn kitty_keyboard_is_popped_once_per_push() {
        let _g = reset();
        let out = feed(&[b"\x1b[>1u", b"\x1b[>1u"]);
        assert_eq!(out, b"\x1b[<u\x1b[<u\x1b[0m");
    }

    #[test]
    fn kitty_keyboard_the_child_popped_is_not_popped_again() {
        let _g = reset();
        assert!(feed(&[b"\x1b[>1u", b"\x1b[<u"]).is_empty());
    }

    #[test]
    fn sequences_split_across_reads_are_still_recognised() {
        let _g = reset();
        let out = feed(&[b"\x1b[?10", b"49h", b"\x1b[>", b"1u"]);
        assert!(out.starts_with(b"\x1b[<u"));
        assert!(out.ends_with(b"\x1b[?1049l\x1b[0m"));
    }

    #[test]
    fn mouse_modes_are_undone_individually() {
        let _g = reset();
        let out = feed(&[b"\x1b[?1000h\x1b[?1006h"]);
        assert_eq!(out, b"\x1b[?1000l\x1b[?1006l\x1b[0m");
    }

    #[test]
    fn osc_titles_do_not_trip_the_scanner() {
        let _g = reset();
        assert!(feed(&[b"\x1b]0;\x1b[?1049h fake title\x07plain"]).is_empty());
    }

    #[test]
    fn a_partial_tail_is_retained_not_dropped() {
        let _g = reset();
        let mut buf = Vec::new();
        observe(&mut buf, b"text\x1b[?20");
        assert!(!buf.is_empty());
        observe(&mut buf, b"04h");
        assert_eq!(cleanup(), b"\x1b[?2004l\x1b[0m");
    }
}
