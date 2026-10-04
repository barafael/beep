//! `org.freedesktop.Notifications`, plus KDE Plasma's inline-reply extension: an
//! `inline-reply` action makes Plasma show a text field and report the answer with the
//! `NotificationReplied` signal.

use std::collections::HashMap;

use futures_util::stream::{self, StreamExt};
use zbus::zvariant::Value;

use crate::{Action, AppInfo, Backend, Notification};

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: &str) -> zbus::Result<()>;

    /// KDE extension.
    #[zbus(signal)]
    fn notification_replied(&self, id: u32, text: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

enum Request {
    Show(Notification),
    Withdraw(String),
}

pub struct Freedesktop {
    requests: async_channel::Sender<Request>,
}

impl Freedesktop {
    pub fn new(app: &AppInfo, actions: async_channel::Sender<Action>) -> Self {
        let (requests, receiver) = async_channel::unbounded();
        let (id, name) = (app.id, app.name);
        // zbus on async-io needs no particular runtime. A small thread drives it, so the UI
        // never waits on D-Bus; it ends when the `Notifier` is dropped.
        let spawned = std::thread::Builder::new()
            .name("langis-notify".into())
            .spawn(move || {
                if let Err(error) =
                    futures_lite::future::block_on(serve(id, name, receiver, actions))
                {
                    tracing::warn!(%error, "desktop notifications unavailable");
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "desktop notifications unavailable");
        }
        Self { requests }
    }
}

impl Backend for Freedesktop {
    fn show(&self, notification: Notification) {
        let _ = self.requests.try_send(Request::Show(notification));
    }

    fn withdraw(&self, key: &str) {
        let _ = self.requests.try_send(Request::Withdraw(key.to_string()));
    }
}

enum Input {
    Request(Request),
    /// The `Notifier` is gone.
    Quit,
    Invoked {
        id: u32,
        action: String,
    },
    Replied {
        id: u32,
        text: String,
    },
    Closed {
        id: u32,
    },
}

async fn serve(
    app_id: &str,
    app_name: &str,
    requests: async_channel::Receiver<Request>,
    actions: async_channel::Sender<Action>,
) -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    let proxy = NotificationsProxy::new(&connection).await?;
    let capabilities = proxy.get_capabilities().await?;
    let has = |capability: &str| capabilities.iter().any(|c| c == capability);
    let (clickable, replies, markup) = (has("actions"), has("inline-reply"), has("body-markup"));

    // Signals cover every app's notifications; only ids in `shown` are ours.
    let invoked = proxy
        .receive_action_invoked()
        .await?
        .filter_map(|signal| async move {
            let args = signal.args().ok()?;
            Some(Input::Invoked {
                id: args.id,
                action: args.action_key.to_string(),
            })
        });
    let replied = proxy
        .receive_notification_replied()
        .await?
        .filter_map(|signal| async move {
            let args = signal.args().ok()?;
            Some(Input::Replied {
                id: args.id,
                text: args.text.to_string(),
            })
        });
    let closed = proxy
        .receive_notification_closed()
        .await?
        .filter_map(|signal| async move {
            Some(Input::Closed {
                id: signal.args().ok()?.id,
            })
        });
    let requests = requests
        .map(Input::Request)
        .chain(stream::once(async { Input::Quit }));
    let mut inputs = std::pin::pin!(stream::select(
        stream::select(requests, invoked),
        stream::select(replied, closed)
    ));

    // Our notifications on screen, by key.
    let mut shown: HashMap<Option<String>, u32> = HashMap::new();
    let key_of = |shown: &HashMap<Option<String>, u32>, id: u32| {
        shown
            .iter()
            .find(|(_, shown_id)| **shown_id == id)
            .map(|(key, _)| key.clone())
    };
    while let Some(input) = inputs.next().await {
        match input {
            Input::Quit => break,
            Input::Request(Request::Show(notification)) => {
                let mut buttons = Vec::new();
                if clickable {
                    buttons.extend(["default", "Open"]);
                }
                if replies && notification.key.is_some() {
                    buttons.extend(["inline-reply", "Reply"]);
                }
                let mut hints = HashMap::from([
                    ("desktop-entry", Value::from(app_id)),
                    ("category", Value::from("im.received")),
                ]);
                if replies {
                    hints.insert("x-kde-reply-placeholder-text", Value::from("Reply…"));
                }
                let body = if markup {
                    escape_markup(&notification.body)
                } else {
                    notification.body
                };
                let replaces = shown.get(&notification.key).copied().unwrap_or(0);
                let result = proxy
                    .notify(
                        app_name,
                        replaces,
                        app_id,
                        &notification.title,
                        &body,
                        &buttons,
                        hints,
                        -1,
                    )
                    .await;
                match result {
                    Ok(id) => {
                        shown.insert(notification.key, id);
                    }
                    Err(error) => tracing::warn!(%error, "failed to show a notification"),
                }
            }
            Input::Request(Request::Withdraw(key)) => {
                if let Some(id) = shown.remove(&Some(key)) {
                    let _ = proxy.close_notification(id).await;
                }
            }
            Input::Invoked { id, action } if action == "default" => {
                if let Some(key) = key_of(&shown, id) {
                    let _ = actions.try_send(Action::Open { key });
                }
            }
            Input::Invoked { .. } => {}
            Input::Replied { id, text } => {
                if let Some(Some(key)) = key_of(&shown, id) {
                    let _ = actions.try_send(Action::Reply { key, text });
                }
            }
            Input::Closed { id } => shown.retain(|_, shown_id| *shown_id != id),
        }
    }
    Ok(())
}

/// Servers with `body-markup` interpret a subset of HTML in the body.
fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_escaped() {
        assert_eq!(
            escape_markup("<b>a & b</b>"),
            "&lt;b&gt;a &amp; b&lt;/b&gt;"
        );
    }
}
