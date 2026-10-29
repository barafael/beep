//! How b33p names itself to Signal's servers: on every request and websocket, presage's and its
//! own, as Signal's apps do (`Signal-Android/8.32.1 Android/34`).

use std::sync::Once;

use presage::ClientIdentity;
use presage::libsignal_service::configuration::SignalServers;
use presage::libsignal_service::push_service::PushService;
use tracing::warn;

/// `b33p/<version>`, then what it is built on.
const USER_AGENT: &str = concat!("b33p/", env!("CARGO_PKG_VERSION"), " presage-rs");

/// The user's languages for the server's localized text: English, as b33p's text is. Once b33p
/// speaks others, the language it shows.
const ACCEPT_LANGUAGE: &str = "en";

static IDENTIFY: Once = Once::new();

/// Gives presage b33p's [`ClientIdentity`], before the first manager talks to the servers.
pub(crate) fn identify() {
    IDENTIFY.call_once(|| {
        let identity = ClientIdentity {
            user_agent: USER_AGENT.to_owned(),
            accept_language: Some(ACCEPT_LANGUAGE.to_owned()),
        };
        if let Err(identity) = presage::set_client_identity(identity) {
            warn!(?identity, "presage had an identity for the client already");
        }
    });
}

/// A push service without credentials, for b33p's own requests: the CDN's.
pub(crate) fn push_service(servers: SignalServers) -> PushService {
    identify();
    presage::client::push_service(servers, None)
}
