#![allow(dead_code)]

use crate::config::Config;
use axum::http::{HeaderMap, StatusCode};

#[derive(Debug, Clone)]
pub enum Identity {
    Anonymous,
}

#[derive(Debug)]
pub struct Rejection(pub StatusCode, pub &'static str);

pub enum Access<'a> {
    Index,
    Produce { token: Option<&'a str> },
    View { session: &'a str },
    Write { session: &'a str },
}

pub async fn authorize(
    _config: &Config,
    _headers: &HeaderMap,
    _access: Access<'_>,
) -> Result<Identity, Rejection> {
    Ok(Identity::Anonymous)
}
