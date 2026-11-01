//! What went wrong in the backend, and what can be done about it: whether the same again may
//! work decides what the window offers (trying again, or only saying why).
//!
//! Each module says what fails in it with error types of its own, one variant per thing that
//! can go wrong, carrying what it was about and the error that caused it. Each of them is
//! [`Classified`]: it knows its [`ErrorKind`]. What reaches the UI is an [`Error`]: any failure,
//! with its kind, shared so that events can be cloned.

use std::{fmt, sync::Arc};

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

/// An error of the backend's (or the UI's): it knows whether the same again may work.
pub trait Classified: std::error::Error + Send + Sync + 'static {
    fn kind(&self) -> ErrorKind;
}

/// A failure as the UI gets it: its [`ErrorKind`], and the error itself, causes and all.
///
/// It says the whole chain ("failed to send to “Alice”: the server answered 413"), since that
/// is what the banner shows and the log keeps.
#[derive(Clone)]
pub struct Error {
    kind: ErrorKind,
    failure: Arc<dyn Classified>,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Whether doing the same again may work.
    pub fn retryable(&self) -> bool {
        self.kind == ErrorKind::Unreachable
    }

    /// The failure itself, e.g. to look for a particular one with `downcast_ref`.
    pub fn failure(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        &*self.failure
    }
}

impl<F: Classified> From<F> for Error {
    fn from(failure: F) -> Self {
        Self {
            kind: failure.kind(),
            failure: Arc::new(failure),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&Chain(&*self.failure), f)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
            .field("failure", &self.failure)
            .finish()
    }
}

/// Two errors are the same if they are of the same kind and say the same.
impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.to_string() == other.to_string()
    }
}

impl Eq for Error {}

// Its Display already says the causes: walking them again would say them twice.
impl std::error::Error for Error {}

/// `error` and its causes, each after a colon: "failed to save “a.png”: permission denied".
/// A cause that the error before it already says (as some libraries put their cause in their
/// own message) is left out.
pub struct Chain<'a>(pub &'a (dyn std::error::Error + 'static));

impl fmt::Display for Chain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut said = self.0.to_string();
        f.write_str(&said)?;
        let mut cause = self.0.source();
        while let Some(error) = cause {
            let says = error.to_string();
            if !says.is_empty() && !said.contains(&says) {
                write!(f, ": {says}")?;
            }
            said = says;
            cause = error.source();
        }
        Ok(())
    }
}

/// An error of a kind that says what it is given, for the tests of how failures are shown.
#[cfg(test)]
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct Said {
    kind: ErrorKind,
    message: String,
}

#[cfg(test)]
impl Classified for Said {
    fn kind(&self) -> ErrorKind {
        self.kind
    }
}

#[cfg(test)]
impl Error {
    pub(crate) fn said(kind: ErrorKind, message: impl Into<String>) -> Self {
        Said {
            kind,
            message: message.into(),
        }
        .into()
    }

    pub(crate) fn unreachable(message: impl Into<String>) -> Self {
        Self::said(ErrorKind::Unreachable, message)
    }

    pub(crate) fn refused(message: impl Into<String>) -> Self {
        Self::said(ErrorKind::Refused, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    enum Sending {
        #[error("failed to send")]
        TimedOut(#[source] std::io::Error),
        #[error("not a group link")]
        NotAGroupLink,
    }

    impl Classified for Sending {
        fn kind(&self) -> ErrorKind {
            match self {
                Self::TimedOut(_) => ErrorKind::Unreachable,
                Self::NotAGroupLink => ErrorKind::Refused,
            }
        }
    }

    #[test]
    fn only_what_couldnt_reach_signal_is_worth_trying_again() {
        let timed_out = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        let error = Error::from(Sending::TimedOut(timed_out));
        assert!(error.retryable());
        assert_eq!(error.kind(), ErrorKind::Unreachable);
        assert_eq!(error.to_string(), "failed to send: timed out");
        assert!(!Error::from(Sending::NotAGroupLink).retryable());
    }

    #[derive(Debug, thiserror::Error)]
    #[error("failed to send: {0}")]
    struct SaysItsCause(#[source] std::io::Error);

    #[test]
    fn a_cause_already_said_is_said_once() {
        let error = SaysItsCause(std::io::Error::other("timed out"));
        assert_eq!(Chain(&error).to_string(), "failed to send: timed out");
    }
}
