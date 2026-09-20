use bytes::Bytes;

pub const MAX_COLS: u16 = 1000;
pub const MAX_ROWS: u16 = 500;
pub const RESET: &[u8] = b"\x1bc";

pub fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(1, MAX_COLS), rows.clamp(1, MAX_ROWS))
}

pub fn dump_bytes(vt: &avt::Vt) -> Bytes {
    let mut out = Vec::from(RESET);
    out.extend_from_slice(normalize_sgr(&vt.dump()).as_bytes());
    Bytes::from(out)
}

fn normalize_sgr(dump: &str) -> String {
    let mut out = String::with_capacity(dump.len());
    let mut rest = dump;
    while let Some(start) = rest.find("\x1b[") {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let Some(end) = tail.find(|c: char| ('\u{40}'..='\u{7e}').contains(&c)) else {
            out.push_str(&rest[start..]);
            return out;
        };
        out.push_str("\x1b[");
        if &tail[end..end + 1] == "m" {
            out.push_str(&normalize_sgr_params(&tail[..end]));
        } else {
            out.push_str(&tail[..end]);
        }
        out.push_str(&tail[end..end + 1]);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

fn normalize_sgr_params(params: &str) -> String {
    params
        .split(';')
        .map(|param| {
            let parts: Vec<&str> = param.split(':').collect();
            match parts.as_slice() {
                ["38" | "48" | "58", "2", _, _, _] | ["38" | "48" | "58", "5", _] => {
                    parts.join(";")
                }
                _ => param.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

pub fn decode_into(vt: &mut avt::Vt, carry: &mut Vec<u8>) {
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
