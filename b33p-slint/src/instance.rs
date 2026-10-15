//! One b33p per user. Two would share one database and one Signal account; starting b33p
//! again (e.g. from the launcher, while it sits in the tray) shows the running one instead.
//!
//! | Platform | Lock and message |
//! |---|---|
//! | Linux, BSD | the D-Bus name `io.github.barafael.B33p`, called through `org.freedesktop.Application` |
//! | Windows | a named pipe; connecting to it is the message |
//! | macOS | the system already starts an app only once |
//!
//! The mock backend has a lock of its own (`io.github.barafael.B33p.Mock`): it touches no account, so
//! it may run next to the real b33p, and testing this needs two of it.

/// What starting found.
pub enum Claim {
    /// This is the only b33p. The running one is asked to show itself through `activations`.
    First(Primary),
    /// Another b33p is running and was asked to show itself; this one should end.
    Second,
    /// Couldn't tell, e.g. without a session bus. Runs as if alone.
    Unchecked,
}

pub struct Primary {
    /// One message per later start, with its Wayland activation token if it had one.
    pub activations: async_channel::Receiver<Option<String>>,
    /// Holds the lock until b33p ends.
    _lock: platform::Lock,
}

/// Claims b33p for this process, or asks the one that already has it to show itself.
pub fn claim(mock: bool) -> Claim {
    let id = if mock {
        "io.github.barafael.B33p.Mock"
    } else {
        "io.github.barafael.B33p"
    };
    let (sender, activations) = async_channel::unbounded();
    match platform::claim(id, sender) {
        Ok(Some(lock)) => Claim::First(Primary {
            activations,
            _lock: lock,
        }),
        Ok(None) => Claim::Second,
        Err(error) => {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "can't tell whether b33p is already running");
            Claim::Unchecked
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::collections::HashMap;

    use zbus::zvariant::{OwnedValue, Value};

    const PATH: &str = "/io/github/barafael/B33p";

    /// The connection owning the name; the bus releases it when b33p ends, however it ends.
    pub type Lock = zbus::blocking::Connection;

    /// The part of `org.freedesktop.Application` b33p needs: launchers call `Activate`.
    struct Application(async_channel::Sender<Option<String>>);

    #[zbus::interface(name = "org.freedesktop.Application")]
    impl Application {
        fn activate(&self, platform_data: HashMap<String, OwnedValue>) {
            let _ = self.0.try_send(activation_token(&platform_data));
        }

        fn open(&self, _uris: Vec<String>, platform_data: HashMap<String, OwnedValue>) {
            let _ = self.0.try_send(activation_token(&platform_data));
        }

        fn activate_action(
            &self,
            _action: String,
            _parameter: Vec<OwnedValue>,
            _platform_data: HashMap<String, OwnedValue>,
        ) {
        }
    }

    fn activation_token(platform_data: &HashMap<String, OwnedValue>) -> Option<String> {
        platform_data
            .get("activation-token")
            .and_then(|token| String::try_from(token.try_clone().ok()?).ok())
    }

    #[test]
    fn reads_the_activation_token() {
        let mut platform_data = HashMap::new();
        assert_eq!(activation_token(&platform_data), None);
        platform_data.insert(
            "activation-token".to_string(),
            OwnedValue::try_from(Value::from("kwin-42")).unwrap(),
        );
        assert_eq!(activation_token(&platform_data).as_deref(), Some("kwin-42"));
    }

    /// `None` if another b33p has the name `id`; it was asked to show itself.
    pub fn claim(
        id: &str,
        activations: async_channel::Sender<Option<String>>,
    ) -> zbus::Result<Option<Lock>> {
        let connection = zbus::blocking::connection::Builder::session()?
            .serve_at(PATH, Application(activations))?
            .build()?;
        // Not through the builder's `name`: together with `serve_at`, zbus 5.19 reports success
        // there even when the name is taken.
        let requested =
            connection.request_name_with_flags(id, zbus::fdo::RequestNameFlags::DoNotQueue.into());
        match requested {
            Ok(_) => Ok(Some(connection)),
            Err(zbus::Error::NameTaken) => {
                // It is running either way; if it doesn't answer, a second copy still mustn't.
                if let Err(error) = activate_running(id) {
                    tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "b33p is already running but didn't answer");
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn activate_running(id: &str) -> zbus::Result<()> {
        let connection = zbus::blocking::Connection::session()?;
        // Passed on so the running b33p may take focus, should it learn to use it.
        let mut platform_data = HashMap::new();
        if let Ok(token) = std::env::var("XDG_ACTIVATION_TOKEN") {
            platform_data.insert("activation-token", Value::from(token));
        }
        connection.call_method(
            Some(id),
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

    /// The pipe lives as long as the thread serving it, which is as long as b33p.
    pub type Lock = ();

    /// Pipe names are machine-wide, so each user gets their own.
    fn pipe_name(id: &str) -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        format!(r"\\.\pipe\{id}-{user}")
    }

    /// `None` if another b33p has the pipe for `id`; it was asked to show itself.
    pub fn claim(
        id: &str,
        activations: async_channel::Sender<Option<String>>,
    ) -> windows::core::Result<Option<Lock>> {
        let name = pipe_name(id);
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
            .name("b33p-instance".into())
            .spawn(move || serve(HANDLE(pipe as _), activations));
        if let Err(error) = spawned {
            // The pipe stays open, so it still keeps other copies out; they just can't show this one.
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "later starts can't show b33p");
        }
        Ok(Some(()))
    }

    /// Each connection is one later start asking b33p to show itself.
    fn serve(pipe: HANDLE, activations: async_channel::Sender<Option<String>>) {
        loop {
            match unsafe { ConnectNamedPipe(pipe, None) } {
                Ok(()) => {}
                // Connected between creating the pipe and waiting on it.
                Err(error) if error.code() == ERROR_PIPE_CONNECTED.to_hresult() => {}
                Err(error) => {
                    tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "later starts can't show b33p");
                    return;
                }
            }
            if activations.try_send(None).is_err() {
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
                    tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "b33p is already running but didn't answer");
                    return;
                }
            }
        }
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
mod platform {
    pub type Lock = ();

    pub fn claim(
        _: &str,
        _: async_channel::Sender<Option<String>>,
    ) -> Result<Option<Lock>, std::convert::Infallible> {
        Ok(Some(()))
    }
}
