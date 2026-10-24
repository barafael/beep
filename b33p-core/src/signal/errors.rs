//! Telling presage's and Signal's errors apart by what can be done about them: worth trying
//! again (no connection, a timeout, the server busy or asking to wait), or not (Signal said no,
//! or what was asked makes no sense).

use presage::{
    Error as PresageError,
    libsignal_service::prelude::{MessageSenderError, ServiceError},
};

use crate::{Error, ErrorKind};

/// The kind of `error`; see [`ErrorKind`].
pub(crate) fn kind_of<S: std::error::Error>(error: &PresageError<S>) -> ErrorKind {
    match error {
        PresageError::ServiceError(error) => service_kind(error),
        PresageError::MessageSenderError(error) => sender_kind(error),
        PresageError::ProfileManagerError(_)
        | PresageError::Timeout(_)
        | PresageError::IoError(_)
        | PresageError::MessagePipeNotStarted
        | PresageError::MessagePipeInterruptedError => ErrorKind::Unreachable,
        PresageError::Store(_) => ErrorKind::Storage,
        _ => ErrorKind::Refused,
    }
}

fn sender_kind(error: &MessageSenderError) -> ErrorKind {
    match error {
        MessageSenderError::ServiceError(error) => service_kind(error),
        MessageSenderError::AttachmentUploadError(error) => upload_kind(error),
        MessageSenderError::MaximumRetriesLimitExceeded => ErrorKind::Unreachable,
        _ => ErrorKind::Refused,
    }
}

/// The kind of what Signal's servers (or getting to them) answered.
pub(crate) fn service_kind(error: &ServiceError) -> ErrorKind {
    match error {
        ServiceError::Timeout { .. }
        | ServiceError::IO(_)
        | ServiceError::WebSocket(_)
        | ServiceError::WsClosing { .. }
        | ServiceError::RateLimitExceeded { .. }
        | ServiceError::Cdn(_)
        // Presage sends again with the devices put right, up to a limit; later, it may work.
        | ServiceError::MismatchedDevicesException(_)
        | ServiceError::StaleDevices(_)
        | ServiceError::MultiRecipientMismatchedDevices(_)
        | ServiceError::MultiRecipientStaleDevices(_) => ErrorKind::Unreachable,
        ServiceError::UnhandledResponseCode { status, .. } if status.is_server_error() => {
            ErrorKind::Unreachable
        }
        _ => ErrorKind::Refused,
    }
}

/// The kind of a failed upload: the network, unless the file couldn't be read.
pub(crate) fn upload_kind(
    error: &presage::libsignal_service::sender::AttachmentUploadError,
) -> ErrorKind {
    use presage::libsignal_service::sender::AttachmentUploadError;
    match error {
        AttachmentUploadError::ServiceError(error) => service_kind(error),
        AttachmentUploadError::IoError(_) => ErrorKind::Storage,
    }
}

/// For `map_err` on Signal's servers directly: `error`, said after `context`, of its kind.
pub(crate) fn service_failed(context: &str) -> impl FnOnce(ServiceError) -> Error + '_ {
    move |error| Error::new(service_kind(&error), format!("{context}: {error}"))
}

/// For `map_err` on the storage service: an undecipherable blob won't decipher next time.
pub(crate) fn storage_service_failed(
    context: &str,
) -> impl FnOnce(presage::libsignal_service::StorageServiceError) -> Error + '_ {
    use presage::libsignal_service::StorageServiceError;
    move |error| {
        let kind = match &error {
            StorageServiceError::Service(error) => service_kind(error),
            StorageServiceError::Invalid => ErrorKind::Refused,
        };
        Error::new(kind, format!("{context}: {error}"))
    }
}

/// For `map_err`: `error`, said after `context` (e.g. "failed to send"), of its kind.
pub(crate) fn failed<S: std::error::Error>(
    context: &str,
) -> impl FnOnce(PresageError<S>) -> Error + '_ {
    move |error| Error::new(kind_of(&error), format!("{context}: {error}"))
}

/// For `map_err` on presage's store (not through the manager): always
/// [`ErrorKind::Storage`].
pub(crate) fn store_failed<E: std::fmt::Display>(context: &str) -> impl FnOnce(E) -> Error + '_ {
    move |error| Error::storage(format!("{context}: {error}"))
}

/// For `map_err` on the database: always [`ErrorKind::Storage`].
pub(crate) fn db(error: sqlx::Error) -> Error {
    Error::from(error)
}

/// For `map_err` on a reqwest request: the network, unless the server refused it.
pub(crate) fn http(context: &str) -> impl FnOnce(reqwest::Error) -> Error + '_ {
    move |error| {
        let kind = match error.status() {
            Some(status) if !status.is_server_error() && status.as_u16() != 429 => {
                ErrorKind::Refused
            }
            _ => ErrorKind::Unreachable,
        };
        Error::new(kind, format!("{context}: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use presage_store_sqlite::SqliteStoreError;

    type E = PresageError<SqliteStoreError>;

    #[test]
    fn what_may_pass_is_told_from_what_wont() {
        let timeout = E::ServiceError(ServiceError::Timeout { attempts: 3 });
        assert_eq!(kind_of(&timeout), ErrorKind::Unreachable);
        let limited = E::ServiceError(ServiceError::RateLimitExceeded { retry_after: None });
        assert_eq!(kind_of(&limited), ErrorKind::Unreachable);
        let gone = E::ServiceError(ServiceError::UnregisteredRecipient);
        assert_eq!(kind_of(&gone), ErrorKind::Refused);
        let sending = E::MessageSenderError(Box::new(MessageSenderError::ServiceError(
            ServiceError::Unauthorized,
        )));
        assert_eq!(kind_of(&sending), ErrorKind::Refused);
        assert_eq!(kind_of(&E::UnknownGroup), ErrorKind::Refused);
        assert_eq!(
            kind_of(&E::MessagePipeInterruptedError),
            ErrorKind::Unreachable
        );

        let error = failed("failed to send")(timeout);
        assert!(error.retryable());
        assert!(
            error.message.starts_with("failed to send: "),
            "{}",
            error.message
        );
    }
}
