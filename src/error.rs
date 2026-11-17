use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum Error {
    /// The file is not a valid file of the expected format.
    #[error("invalid {format} file: {msg}")]
    Format { format: &'static str, msg: String },
    /// The data cannot be written in the requested format.
    #[error("cannot write {format}: {msg}")]
    Write { format: &'static str, msg: String },
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("font error: {0}")]
    Font(String),
    #[error("image error: {0}")]
    Image(String),
    #[error("licence error: {0}")]
    License(String),
    #[error("invalid input: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn format(format: &'static str, msg: impl Into<String>) -> Self {
        Error::Format {
            format,
            msg: msg.into(),
        }
    }
    pub fn write(format: &'static str, msg: impl Into<String>) -> Self {
        Error::Write {
            format,
            msg: msg.into(),
        }
    }
}
