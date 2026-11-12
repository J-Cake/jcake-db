use std::fmt::Formatter;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub enum Error {
    Misc(String),
    EOF,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self, f)
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn new(msg: impl AsRef<str>) -> Self {
        Self::Misc(msg.as_ref().to_string())
    }

    pub fn eof() -> Self {
        Self::EOF
    }

    pub fn is_eof(&self) -> bool {
        matches!(self, Self::EOF)
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