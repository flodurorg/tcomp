use serde::{Deserialize, Serialize};

pub const VERSION: u8 = 1;
pub const ENCRYPTED_VERSION: u8 = 2;
pub const MAX_ENCRYPTED_FRAME: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub v: u8,
    #[serde(default)]
    pub token: Option<String>,
    pub name: String,
    pub cmd: String,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub input: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub session: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_bytes: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum EncryptedPayload {
    Snapshot {
        cols: u16,
        rows: u16,
        name: String,
        cmd: String,
        title: Option<String>,
        cwd: Option<String>,
        input: bool,
        epoch: String,
        screen: String,
        carry: Vec<u8>,
        exit: Option<i32>,
        #[serde(default, skip_serializing_if = "is_false")]
        checkpoint: bool,
    },
    Output {
        bytes: Vec<u8>,
    },
    Input {
        epoch: String,
        bytes: Vec<u8>,
    },
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Live,
    Ended,
    Stale,
}

impl Status {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Status::Live)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Producer {
    Resize { cols: u16, rows: u16 },
    Title { text: String },
    Cwd { path: String },
    Exit { code: i32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Server {
    Init {
        cols: u16,
        rows: u16,
        status: Status,
        name: String,
        cmd: String,
        input: bool,
        title: Option<String>,
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encryption: Option<u8>,
    },
    ReplayUnavailable,
    Resize {
        cols: u16,
        rows: u16,
    },
    Status {
        status: Status,
    },
    Exit {
        code: Option<i32>,
    },
    /// Out-of-band message from tcomp itself, rendered as a styled bar in the viewer.
    Banner {
        text: String,
    },
    Title {
        text: String,
    },
    Cwd {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub cmd: String,
    pub cols: u16,
    pub rows: u16,
    pub status: Status,
    pub input: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    pub preview: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<u8>,
}
