use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, ErrorCode, RetrySafety};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryFingerprint([u8; 32]);

impl QueryFingerprint {
    pub fn new(canonical_query: &str) -> Self {
        Self(Sha256::digest(canonical_query.as_bytes()).into())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PageState {
    Offset(u64),
    Token(String),
}

pub fn encode_cursor(
    command: &str,
    fingerprint: &QueryFingerprint,
    state: PageState,
) -> Result<String, AppError> {
    let cursor = CursorV1 {
        version: 1,
        command,
        fingerprint: URL_SAFE_NO_PAD.encode(fingerprint.0),
        state,
    };
    let bytes = serde_json::to_vec(&cursor).map_err(|_| invalid_cursor())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn decode_cursor(
    cursor: &str,
    command: &str,
    fingerprint: &QueryFingerprint,
) -> Result<PageState, AppError> {
    if cursor.len() > 4 * 1024 {
        return Err(invalid_cursor());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor.as_bytes())
        .map_err(|_| invalid_cursor())?;
    let decoded: DecodedCursor = serde_json::from_slice(&bytes).map_err(|_| invalid_cursor())?;
    let expected_fingerprint = URL_SAFE_NO_PAD.encode(fingerprint.0);
    if decoded.version != 1
        || decoded.command != command
        || decoded.fingerprint != expected_fingerprint
    {
        return Err(invalid_cursor());
    }
    Ok(decoded.state)
}

#[derive(Serialize)]
struct CursorV1<'a> {
    #[serde(rename = "v")]
    version: u8,
    #[serde(rename = "c")]
    command: &'a str,
    #[serde(rename = "q")]
    fingerprint: String,
    #[serde(rename = "s")]
    state: PageState,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecodedCursor {
    #[serde(rename = "v")]
    version: u8,
    #[serde(rename = "c")]
    command: String,
    #[serde(rename = "q")]
    fingerprint: String,
    #[serde(rename = "s")]
    state: PageState,
}

fn invalid_cursor() -> AppError {
    AppError::new(
        ErrorCode::InvalidCursor,
        "the cursor is invalid for this query",
        RetrySafety::Safe,
    )
}
