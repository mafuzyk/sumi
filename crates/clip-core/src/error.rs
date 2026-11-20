use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Png(png::EncodingError),
    Format(String),
    Unsupported(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Sqlite(e) => write!(f, "sqlite: {e}"),
            Error::Png(e) => write!(f, "png: {e}"),
            Error::Format(s) => write!(f, "format: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<png::EncodingError> for Error {
    fn from(e: png::EncodingError) -> Self {
        Error::Png(e)
    }
}

impl Error {
    pub fn format(m: impl Into<String>) -> Self {
        Error::Format(m.into())
    }
    pub fn unsupported(m: impl Into<String>) -> Self {
        Error::Unsupported(m.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
