//! Telling presage's and Signal's errors apart by what can be done about them: worth trying
//! again (no connection, a timeout, the server busy or asking to wait), or not (Signal said no,
//! or what was asked makes no sense).

use crate::outbox::SendFailure;
use presage::{
    Error as PresageError,
    libsignal_service::prelude::{MessageSenderError, ServiceError},
};

use crate::ErrorKind;

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
        // No answer: not connected, the request cut off, or too slow (an upload to the CDN).
        ServiceError::Http(error)
            if error.is_connect() || error.is_request() || error.is_timeout() =>
        {
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

/// Whether the upload's request went out on a kept-alive connection that the server closed as
/// it went: hyper's `IncompleteMessage`, the connection ending before any answer. A CDN closes
/// connections idle for a while (the test box's, wrangler's, after 5 s); one that closes just
/// as the request is written can't be told from a live one in time, the later the slower the
/// link. The request never got an answer, so trying once more, on a new connection, is safe.
pub(crate) fn closed_under_request(
    error: &presage::libsignal_service::sender::AttachmentUploadError,
) -> bool {
    use presage::libsignal_service::sender::AttachmentUploadError;
    let AttachmentUploadError::ServiceError(ServiceError::Http(error)) = error else {
        return false;
    };
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = cause {
        if error
            .downcast_ref::<hyper::Error>()
            .is_some_and(hyper::Error::is_incomplete_message)
        {
            return true;
        }
        cause = error.source();
    }
    false
}

/// The kind of what the storage service answered: an undecipherable blob won't decipher next
/// time.
pub(crate) fn storage_service_kind(
    error: &presage::libsignal_service::StorageServiceError,
) -> ErrorKind {
    use presage::libsignal_service::StorageServiceError;
    match error {
        StorageServiceError::Service(error) => service_kind(error),
        StorageServiceError::Invalid => ErrorKind::Refused,
    }
}

/// The kind of a send through the outbox: a session that ended first is
/// [`ErrorKind::Unreachable`], since the next one may send it; presage's errors as
/// [`kind_of`].
pub(crate) fn send_kind(failure: &SendFailure) -> ErrorKind {
    match failure {
        SendFailure::Stopped => ErrorKind::Unreachable,
        SendFailure::Presage(error) => kind_of(error),
    }
}

/// The kind of a reqwest request's failure: the network, unless the server refused it.
pub(crate) fn http_kind(error: &reqwest::Error) -> ErrorKind {
    match error.status() {
        Some(status) if !status.is_server_error() && status.as_u16() != 429 => ErrorKind::Refused,
        _ => ErrorKind::Unreachable,
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
    }

    /// An upload that gets no answer (here, nothing listens) may go through later: it is no
    /// refusal (pending/tour.md, finding 19).
    #[tokio::test]
    async fn no_answer_over_http_may_pass() {
        let error = reqwest::Client::new()
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .expect_err("nothing listens on port 1");
        let upload = presage::libsignal_service::sender::AttachmentUploadError::ServiceError(
            ServiceError::Http(error),
        );
        assert_eq!(upload_kind(&upload), ErrorKind::Unreachable);
        assert!(
            !closed_under_request(&upload),
            "a refused connection never took the request"
        );
    }

    /// A server that reads the request and closes without an answer, as a CDN closing an
    /// idle connection just as the request comes, is told apart: the upload may go once more.
    #[tokio::test]
    async fn a_request_the_server_closed_under_is_told_apart() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port on the loopback");
        let address = listener.local_addr().expect("the listener's address");
        let server = tokio::spawn(async move {
            let (mut connection, _) = listener.accept().await.expect("the client connects");
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            // The whole head and the one-byte body, then the connection closes unanswered.
            while !request.ends_with(b"\r\n\r\nx") {
                let read = connection
                    .read(&mut buffer)
                    .await
                    .expect("the request comes");
                assert_ne!(read, 0, "the client closed first");
                request.extend_from_slice(&buffer[..read]);
            }
        });
        let error = reqwest::Client::new()
            .post(format!("http://{address}/upload/attachments"))
            .body("x")
            .send()
            .await
            .expect_err("the server answers nothing");
        server.await.expect("the server read the request");
        let upload = presage::libsignal_service::sender::AttachmentUploadError::ServiceError(
            ServiceError::Http(error),
        );
        assert!(closed_under_request(&upload));
    }
}
