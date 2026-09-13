use serde::{Deserialize, Serialize};

pub const VERSION: u8 = 1;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub session: String,
    pub url: String,
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
    },
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
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    pub preview: String,
}
