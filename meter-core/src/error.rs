use thiserror::Error;

pub type Result<T> = std::result::Result<T, MeterError>;

#[derive(Error, Debug)]
pub enum MeterError {
    #[error("Invalid frame: {0}")]
    InvalidFrame(String),

    #[error("Checksum mismatch: expected {expected:02X}, got {actual:02X}")]
    ChecksumMismatch { expected: u8, actual: u8 },

    #[error("Unsupported control code: {0:02X}")]
    UnsupportedControlCode(u8),

    #[error("Data item not found: {0}")]
    DataItemNotFound(String),

    #[error("Access denied: {0}")]
    AccessDenied(String),

    #[error("Invalid password")]
    InvalidPassword,

    #[error("Invalid data length: expected {expected}, got {actual}")]
    InvalidDataLength { expected: usize, actual: usize },

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Invalid address: {0}")]
    InvalidAddress(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
