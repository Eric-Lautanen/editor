use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Parse {
        line: usize,
        col: usize,
        msg: String,
    },
    Platform(String),
    Usage(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Parse { line, col, msg } => write!(f, "parse error at {line}:{col}: {msg}"),
            Error::Platform(m) => write!(f, "platform error: {m}"),
            Error::Usage(m) => write!(f, "{m}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
