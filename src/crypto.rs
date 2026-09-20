use anyhow::{anyhow, bail, ensure, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::{
    aead, hkdf,
    rand::{SecureRandom, SystemRandom},
};
use std::collections::HashMap;

pub const SNAPSHOT: u8 = 1;
pub const OUTPUT: u8 = 2;
pub const INPUT: u8 = 3;
pub const HEADER_LEN: usize = 30;
const MAX_WRITERS: usize = 1024;
const CONTEXT: &[u8] = b"tcomp-e2ee-v1";

pub struct Key([u8; 32]);

impl Key {
    pub fn generate() -> Result<Self> {
        Ok(Self(random()?))
    }

    pub fn fragment(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    fn derive(&self, session: &str, writer: &[u8; 16], input: bool) -> Result<aead::LessSafeKey> {
        let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, session.as_bytes());
        let prk = salt.extract(&self.0);
        let direction = if input {
            b"input".as_slice()
        } else {
            b"output".as_slice()
        };
        let info = [CONTEXT, direction, writer.as_slice()];
        let okm = prk
            .expand(&info, &aead::AES_256_GCM)
            .map_err(|_| anyhow!("key derivation failed"))?;
        Ok(aead::LessSafeKey::new(aead::UnboundKey::from(okm)))
    }
}

pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow!("system randomness unavailable"))?;
    Ok(bytes)
}

pub struct Header {
    pub kind: u8,
    pub writer: [u8; 16],
    pub sequence: u64,
}

pub fn header(frame: &[u8]) -> Result<Header> {
    ensure!(
        frame.len() >= HEADER_LEN + aead::MAX_TAG_LEN && frame.len() <= proto::MAX_ENCRYPTED_FRAME,
        "invalid encrypted frame size"
    );
    ensure!(
        &frame[..4] == b"TCMP" && frame[4] == 1,
        "unsupported encrypted frame"
    );
    ensure!(
        matches!(frame[5], SNAPSHOT | OUTPUT | INPUT),
        "invalid encrypted frame kind"
    );
    Ok(Header {
        kind: frame[5],
        writer: frame[6..22].try_into()?,
        sequence: u64::from_be_bytes(frame[22..30].try_into()?),
    })
}

fn nonce(sequence: u64) -> aead::Nonce {
    let mut bytes = [0; 12];
    bytes[4..].copy_from_slice(&sequence.to_be_bytes());
    aead::Nonce::assume_unique_for_key(bytes)
}

fn aad(header: &[u8], session: &str) -> Vec<u8> {
    [CONTEXT, session.as_bytes(), header].concat()
}

pub struct Sender {
    key: aead::LessSafeKey,
    session: String,
    writer: [u8; 16],
    sequence: u64,
    input: bool,
}

impl Sender {
    pub fn new(key: &Key, session: &str, input: bool) -> Result<Self> {
        Self::with_writer(key, session, input, random()?)
    }

    fn with_writer(key: &Key, session: &str, input: bool, writer: [u8; 16]) -> Result<Self> {
        Ok(Self {
            key: key.derive(session, &writer, input)?,
            session: session.into(),
            writer,
            sequence: 0,
            input,
        })
    }

    pub fn seal(&mut self, kind: u8, plaintext: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            matches!(kind, SNAPSHOT | OUTPUT | INPUT) && (kind == INPUT) == self.input,
            "wrong encryption direction"
        );
        ensure!(
            plaintext.len() <= proto::MAX_ENCRYPTED_FRAME - HEADER_LEN - aead::MAX_TAG_LEN,
            "encrypted frame too large"
        );
        let sequence = self.sequence;
        self.sequence = sequence
            .checked_add(1)
            .ok_or_else(|| anyhow!("encryption counter exhausted"))?;
        let mut frame = Vec::from(b"TCMP\x01".as_slice());
        frame.push(kind);
        frame.extend_from_slice(&self.writer);
        frame.extend_from_slice(&sequence.to_be_bytes());
        let mut body = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(
                nonce(sequence),
                aead::Aad::from(aad(&frame, &self.session)),
                &mut body,
            )
            .map_err(|_| anyhow!("encryption failed"))?;
        frame.extend_from_slice(&body);
        Ok(frame)
    }
}

pub(crate) fn open(
    key: &Key,
    session: &str,
    frame: &[u8],
    input: bool,
) -> Result<(Header, Vec<u8>)> {
    let header = header(frame)?;
    ensure!(
        (header.kind == INPUT) == input,
        "wrong encryption direction"
    );
    let cipher = key.derive(session, &header.writer, input)?;
    let mut body = frame[HEADER_LEN..].to_vec();
    let plain = cipher
        .open_in_place(
            nonce(header.sequence),
            aead::Aad::from(aad(&frame[..HEADER_LEN], session)),
            &mut body,
        )
        .map_err(|_| anyhow!("encrypted frame authentication failed"))?;
    Ok((header, plain.to_vec()))
}

pub struct InputReceiver {
    epoch: String,
    writers: HashMap<[u8; 16], u64>,
}

impl InputReceiver {
    pub fn new() -> Result<Self> {
        Ok(Self {
            epoch: URL_SAFE_NO_PAD.encode(random::<32>()?),
            writers: HashMap::new(),
        })
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn receive(&mut self, key: &Key, session: &str, frame: &[u8]) -> Result<Vec<u8>> {
        let (header, plaintext) = open(key, session, frame, true)?;
        let proto::EncryptedPayload::Input { epoch, bytes } = serde_json::from_slice(&plaintext)?
        else {
            bail!("expected encrypted input");
        };
        ensure!(epoch == self.epoch, "expired input epoch");
        match self.writers.get(&header.writer) {
            Some(last) => ensure!(header.sequence > *last, "replayed input"),
            None => ensure!(self.writers.len() < MAX_WRITERS, "too many input writers"),
        }
        self.writers.insert(header.writer, header.sequence);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(sender: &mut Sender, epoch: &str) -> Vec<u8> {
        sender
            .seal(
                INPUT,
                &serde_json::to_vec(&proto::EncryptedPayload::Input {
                    epoch: epoch.into(),
                    bytes: b"hello\r".to_vec(),
                })
                .unwrap(),
            )
            .unwrap()
    }

    #[test]
    fn envelopes_authenticate_header_direction_session_and_body() {
        let key = Key([7; 32]);
        let mut sender = Sender::new(&key, "session", false).unwrap();
        let frame = sender.seal(SNAPSHOT, b"private output").unwrap();
        assert_eq!(
            open(&key, "session", &frame, false).unwrap().1,
            b"private output"
        );
        assert!(open(&key, "other", &frame, false).is_err());
        assert!(open(&Key([8; 32]), "session", &frame, false).is_err());
        assert!(open(&key, "session", &frame, true).is_err());
        for index in 0..frame.len() {
            let mut changed = frame.clone();
            changed[index] ^= 1;
            assert!(
                open(&key, "session", &changed, false).is_err(),
                "byte {index}"
            );
        }
        for length in 0..frame.len() {
            assert!(open(&key, "session", &frame[..length], false).is_err());
        }
    }

    #[test]
    fn multiple_writers_replays_and_reconnect_epochs() {
        let key = Key([7; 32]);
        let mut receiver = InputReceiver::new().unwrap();
        let mut first = Sender::new(&key, "s", true).unwrap();
        let mut second = Sender::new(&key, "s", true).unwrap();
        let frame = input(&mut first, receiver.epoch());
        assert_eq!(receiver.receive(&key, "s", &frame).unwrap(), b"hello\r");
        assert!(receiver.receive(&key, "s", &frame).is_err());
        assert!(receiver
            .receive(&key, "s", &input(&mut second, receiver.epoch()))
            .is_ok());
        let earlier = input(&mut first, receiver.epoch());
        let later = input(&mut first, receiver.epoch());
        assert!(receiver.receive(&key, "s", &later).is_ok());
        assert!(receiver.receive(&key, "s", &earlier).is_err());
        assert!(InputReceiver::new()
            .unwrap()
            .receive(&key, "s", &frame)
            .is_err());
    }

    #[test]
    fn input_writer_limit_does_not_evict_replay_protection() {
        let key = Key([7; 32]);
        let mut receiver = InputReceiver::new().unwrap();
        for writer in 0..MAX_WRITERS {
            receiver.writers.insert((writer as u128).to_be_bytes(), 10);
        }
        let mut known = Sender::with_writer(&key, "s", true, [0; 16]).unwrap();
        known.sequence = 11;
        let frame = input(&mut known, receiver.epoch());
        assert!(receiver.receive(&key, "s", &frame).is_ok());
        let mut unknown = Sender::with_writer(&key, "s", true, [255; 16]).unwrap();
        assert!(receiver
            .receive(&key, "s", &input(&mut unknown, receiver.epoch()))
            .is_err());
        assert!(receiver.receive(&key, "s", &frame).is_err());
    }

    #[test]
    fn browser_compatibility_vector() {
        let key = Key([7; 32]);
        let mut sender = Sender::with_writer(&key, "vector-session", false, [9; 16]).unwrap();
        let payload = proto::EncryptedPayload::Snapshot {
            cols: 80,
            rows: 24,
            name: "vector".into(),
            cmd: "bash".into(),
            title: None,
            cwd: None,
            input: true,
            epoch: URL_SAFE_NO_PAD.encode([3; 32]),
            screen: "hello €".into(),
            carry: vec![],
            exit: None,
        };
        let frame = sender
            .seal(SNAPSHOT, &serde_json::to_vec(&payload).unwrap())
            .unwrap();
        assert_eq!(
            URL_SAFE_NO_PAD.encode(&frame),
            include_str!("crypto-vector.txt").trim()
        );
        assert_eq!(
            open(&key, "vector-session", &frame, false).unwrap().1,
            serde_json::to_vec(&payload).unwrap()
        );
    }

    #[test]
    fn counters_never_wrap() {
        let mut sender = Sender::new(&Key([0; 32]), "s", false).unwrap();
        sender.sequence = u64::MAX;
        assert!(sender.seal(OUTPUT, b"x").is_err());
    }
}
