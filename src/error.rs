use std::fmt;

#[derive(Debug)]
pub enum MoqError {
    VersionMismatch,
    UnexpectedMessage(String),
    TrackNotFound(String),
    Io(std::io::Error),
    Protocol(String),
    Connection(String),
}

impl fmt::Display for MoqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoqError::VersionMismatch => write!(f, "version mismatch"),
            MoqError::UnexpectedMessage(msg) => write!(f, "unexpected message: {msg}"),
            MoqError::TrackNotFound(track) => write!(f, "track not found: {track}"),
            MoqError::Io(e) => write!(f, "io error: {e}"),
            MoqError::Protocol(msg) => write!(f, "protocol error: {msg}"),
            MoqError::Connection(msg) => write!(f, "connection error: {msg}"),
        }
    }
}

impl std::error::Error for MoqError {}

impl From<std::io::Error> for MoqError {
    fn from(e: std::io::Error) -> Self {
        MoqError::Io(e)
    }
}
