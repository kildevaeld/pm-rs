use std::error::Error as StdError;
use std::fmt;
use std::io::Error as IoError;
use std::sync::Arc;

use crate::Pid;

#[derive(Debug, Clone)]

pub enum ErrorKind {
    Io(Arc<IoError>),
}

impl From<IoError> for ErrorKind {
    fn from(error: IoError) -> ErrorKind {
        ErrorKind::Io(Arc::new(error))
    }
}

#[derive(Debug, Clone)]
pub struct Error {
    pid: Option<Pid>,
    kind: ErrorKind,
}

impl Error {
    pub fn new(pid: impl Into<Option<Pid>>, kind: ErrorKind) -> Error {
        Error {
            pid: pid.into(),
            kind,
        }
    }

    pub fn pid(&self) -> Option<Pid> {
        self.pid
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ErrorKind::Io(err) => write!(f, "{:?}:Io error: {}", self.pid, err)?,
        }
        Ok(())
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match &self.kind {
            ErrorKind::Io(err) => Some(err),
        }
    }
}
