use std::fmt::Formatter;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Error {
    msg: String,
    recoverable: bool,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.msg, f)
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn new(msg: impl AsRef<str>) -> Self {
        Self {
            msg: msg.as_ref().to_string(),
            recoverable: false
        }
    }

    pub fn recoverable(msg: impl AsRef<str>) -> Self {
        Self {
            msg: msg.as_ref().to_string(),
            recoverable: true
        }
    }

    pub(crate) fn is_recoverable(&self) -> bool {
        self.recoverable
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::new(value.to_string())
    }
}

impl From<jsonb::Error> for Error {
    fn from(value: jsonb::Error) -> Self {
        Self::new(format!("Deserialisation error: {msg}", msg=value.to_string()))
    }
}