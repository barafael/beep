//! What went wrong in the backend, and what can be done about it: whether the same again may
//! work decides what the window offers (trying again, or only saying why).

use std::fmt;

/// How a failure can be dealt with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Signal or the network couldn't be reached, or asked to wait (a rate limit): the same
    /// again later may well work.
    Unreachable,
    /// Signal refused it, or it can't be done as asked (a bad link, an unknown chat, a
    /// recipient no longer on Signal): the same again won't work.
    Refused,
    /// Keeping it here failed: the database or the disk.
    Storage,
}

/// A failure, as said to the user, and its [`ErrorKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn unreachable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unreachable, message)
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Refused, message)
    }

    pub fn storage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Storage, message)
    }

    /// Whether doing the same again may work.
    pub fn retryable(&self) -> bool {
        self.kind == ErrorKind::Unreachable
    }

    /// The same failure, with `context` before what it says, e.g. "failed to send: …".
    pub fn context(self, context: &str) -> Self {
        Self {
            message: format!("{context}: {}", self.message),
            ..self
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Failing to keep something in the database is always [`ErrorKind::Storage`].
impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        Self::storage(format!("failed to save: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_couldnt_reach_signal_is_worth_trying_again() {
        assert!(Error::unreachable("offline").retryable());
        assert!(!Error::refused("not a group link").retryable());
        assert!(!Error::storage("disk full").retryable());
        let error = Error::unreachable("timed out").context("failed to send");
        assert_eq!(error.to_string(), "failed to send: timed out");
        assert_eq!(error.kind, ErrorKind::Unreachable);
    }
}
