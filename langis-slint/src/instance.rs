//! One langis per user. Two would share one database and one Signal account; starting langis
//! again (e.g. from the launcher, while it sits in the tray) shows the running one instead.
//!
//! | Platform | Lock and message |
//! |---|---|
//! | Linux, BSD | the D-Bus name `org.langis.Langis`, called through `org.freedesktop.Application` |
//! | Windows | a named pipe; connecting to it is the message |
//! | macOS | the system already starts an app only once |

/// What starting found.
pub enum Claim {
    /// This is the only langis. The running one is asked to show itself through `activations`.
    First(Primary),
    /// Another langis is running and was asked to show itself; this one should end.
    Second,
    /// Couldn't tell, e.g. without a session bus. Runs as if alone.
    Unchecked,
}

pub struct Primary {
    /// One message per later start.
    pub activations: async_channel::Receiver<()>,
    /// Holds the lock until langis ends.
    _lock: platform::Lock,
}

/// Claims langis for this process, or asks the one that already has it to show itself.
pub fn claim() -> Claim {
    let (sender, activations) = async_channel::unbounded();
    match platform::claim(sender) {
        Ok(Some(lock)) => Claim::First(Primary {
            activations,
            _lock: lock,
        }),
        Ok(None) => Claim::Second,
        Err(error) => {
            tracing::warn!(%error, "can't tell whether langis is already running");
            Claim::Unchecked
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::collections::HashMap;

    use zbus::zvariant::{OwnedValue, Value};

    const NAME: &str = "org.langis.Langis";
    const PATH: &str = "/org/langis/Langis";

    /// The connection owning the name; the bus releases it when langis ends, however it ends.
    pub type Lock = zbus::blocking::Connection;

    /// The part of `org.freedesktop.Application` langis needs: launchers call `Activate`.
    struct Application(async_channel::Sender<()>);

    #[zbus::interface(name = "org.freedesktop.Application")]
    impl Application {
        fn activate(&self, _platform_data: HashMap<String, OwnedValue>) {
            let _ = self.0.try_send(());
        }

        fn open(&self, _uris: Vec<String>, _platform_data: HashMap<String, OwnedValue>) {
            let _ = self.0.try_send(());
        }

        fn activate_action(
            &self,
            _action: String,
            _parameter: Vec<OwnedValue>,
            _platform_data: HashMap<String, OwnedValue>,
        ) {
        }
    }

    /// `None` if another langis has the name; it was asked to show itself.
    pub fn claim(activations: async_channel::Sender<()>) -> zbus::Result<Option<Lock>> {
        let connection = zbus::blocking::connection::Builder::session()?
            .serve_at(PATH, Application(activations))?
            .build()?;
        // Not through the builder's `name`: together with `serve_at`, zbus 5.19 reports success
        // there even when the name is taken.
        let requested = connection
            .request_name_with_flags(NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into());
        match requested {
            Ok(_) => Ok(Some(connection)),
            Err(zbus::Error::NameTaken) => {
                // It is running either way; if it doesn't answer, a second copy still mustn't.
                if let Err(error) = activate_running() {
                    tracing::warn!(%error, "langis is already running but didn't answer");
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn activate_running() -> zbus::Result<()> {
        let connection = zbus::blocking::Connection::session()?;
        // Passed on so the running langis may take focus, should it learn to use it.
        let mut platform_data = HashMap::new();
        if let Ok(token) = std::env::var("XDG_ACTIVATION_TOKEN") {
            platform_data.insert("activation-token", Value::from(token));
        }
        connection.call_method(
            Some(NAME),
            PATH,
            Some("org.freedesktop.Application"),
            "Activate",
            &(platform_data,),
        )?;
        Ok(())
    }
}

#[cfg(windows)]
mod platform {
    use std::time::Duration;

    use windows::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    use windows::core::HSTRING;

    /// The pipe lives as long as the thread serving it, which is as long as langis.
    pub type Lock = ();

    /// Pipe names are machine-wide, so each user gets their own.
    fn pipe_name() -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        format!(r"\\.\pipe\org.langis.Langis-{user}")
    }

    /// `None` if another langis has the pipe; it was asked to show itself.
    pub fn claim(activations: async_channel::Sender<()>) -> windows::core::Result<Option<Lock>> {
        let name = pipe_name();
        // Only the first instance of a pipe may be created with this flag, which makes the
        // pipe itself the lock.
        let pipe = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(&name),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                0,
                0,
                0,
                None,
            )
        };
        if pipe.is_invalid() {
            let error = unsafe { GetLastError() };
            if error != ERROR_ACCESS_DENIED {
                return Err(error.into());
            }
            activate_running(&name);
            return Ok(None);
        }
        // Raw handles aren't `Send`; the thread gets it as a number.
        let pipe = pipe.0 as isize;
        let spawned = std::thread::Builder::new()
            .name("langis-instance".into())
            .spawn(move || serve(HANDLE(pipe as _), activations));
        if let Err(error) = spawned {
            // The pipe stays open, so it still keeps other copies out; they just can't show this one.
            tracing::warn!(%error, "later starts can't show langis");
        }
        Ok(Some(()))
    }

    /// Each connection is one later start asking langis to show itself.
    fn serve(pipe: HANDLE, activations: async_channel::Sender<()>) {
        loop {
            match unsafe { ConnectNamedPipe(pipe, None) } {
                Ok(()) => {}
                // Connected between creating the pipe and waiting on it.
                Err(error) if error.code() == ERROR_PIPE_CONNECTED.to_hresult() => {}
                Err(error) => {
                    tracing::warn!(%error, "later starts can't show langis");
                    return;
                }
            }
            if activations.try_send(()).is_err() {
                return;
            }
            let _ = unsafe { DisconnectNamedPipe(pipe) };
        }
    }

    fn activate_running(name: &str) {
        // This process was just started by the user, so it may hand the foreground on.
        let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        // Busy only between two connections; a few tries are plenty.
        for _ in 0..20 {
            match std::fs::OpenOptions::new().write(true).open(name) {
                Ok(_) => return,
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => {
                    tracing::warn!(%error, "langis is already running but didn't answer");
                    return;
                }
            }
        }
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
mod platform {
    pub type Lock = ();

    pub fn claim(_: async_channel::Sender<()>) -> Result<Option<Lock>, std::convert::Infallible> {
        Ok(Some(()))
    }
}
