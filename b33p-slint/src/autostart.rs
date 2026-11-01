//! Starting b33p when the user logs in, straight into the tray.
//!
//! The entry the system reads is the only record of whether this is on, so it can't disagree
//! with a setting.
//!
//! | Platform | Entry |
//! |---|---|
//! | Linux, BSD | `~/.config/autostart/io.github.barafael.B33p.desktop` (XDG autostart) |
//! | Windows | the `b33p` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |
//! | macOS | not yet |

use std::io;
use std::path::PathBuf;

/// Starts b33p with its window closed, where the tray can take it.
pub const HIDDEN: &str = "--hidden";

/// Why starting with the system couldn't be turned on or off.
#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    #[error("could not determine where b33p is")]
    Executable(#[source] io::Error),
    #[error("the path to b33p isn't valid UTF-8: {}", path.display())]
    NotUtf8 { path: PathBuf },
    #[error("could not determine a config directory")]
    NoConfigDir,
    #[error("failed to create {}", dir.display())]
    CreateDir { dir: PathBuf, source: io::Error },
    #[error("failed to write {}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("failed to remove {}", path.display())]
    Remove { path: PathBuf, source: io::Error },
    #[cfg(windows)]
    #[error("failed to open the registry key {key}")]
    OpenKey {
        key: &'static str,
        source: windows_registry::Error,
    },
    #[cfg(windows)]
    #[error("failed to set the registry value {value}")]
    SetValue {
        value: &'static str,
        source: windows_registry::Error,
    },
    #[cfg(windows)]
    #[error("failed to remove the registry value {value}")]
    RemoveValue {
        value: &'static str,
        source: windows_registry::Error,
    },
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    #[error("starting with the system isn't supported here")]
    Unsupported,
}

/// What to start: the AppImage rather than the binary unpacked from it, which moves.
// Not on macOS, which has no autostart here yet.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn executable() -> Result<PathBuf, AutostartError> {
    match std::env::var_os("APPIMAGE") {
        Some(appimage) => Ok(appimage.into()),
        None => std::env::current_exe().map_err(AutostartError::Executable),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::io;
    use std::path::PathBuf;

    use super::AutostartError;

    pub const SUPPORTED: bool = true;

    fn entry() -> Result<PathBuf, AutostartError> {
        let dirs = directories::BaseDirs::new().ok_or(AutostartError::NoConfigDir)?;
        Ok(dirs
            .config_dir()
            .join("autostart/io.github.barafael.B33p.desktop"))
    }

    pub fn enabled() -> bool {
        entry().is_ok_and(|entry| entry.exists())
    }

    pub fn set(enabled: bool) -> Result<(), AutostartError> {
        let entry = entry()?;
        if !enabled {
            return match std::fs::remove_file(&entry) {
                Err(source) if source.kind() != io::ErrorKind::NotFound => {
                    Err(AutostartError::Remove {
                        path: entry,
                        source,
                    })
                }
                _ => Ok(()),
            };
        }
        let path = super::executable()?;
        let Some(executable) = path.to_str() else {
            return Err(AutostartError::NotUtf8 { path });
        };
        if let Some(dir) = entry.parent() {
            std::fs::create_dir_all(dir).map_err(|source| AutostartError::CreateDir {
                dir: dir.to_owned(),
                source,
            })?;
        }
        std::fs::write(
            &entry,
            format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name=b33p\n\
                 Comment=Signal messenger client\n\
                 Exec={} {}\n\
                 Icon=io.github.barafael.B33p\n\
                 Terminal=false\n",
                quote(executable),
                super::HIDDEN
            ),
        )
        .map_err(|source| AutostartError::Write {
            path: entry,
            source,
        })
    }

    /// An `Exec` argument, quoted as the desktop entry specification asks.
    fn quote(argument: &str) -> String {
        let mut quoted = String::from('"');
        for c in argument.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted.push('"');
        // A desktop entry value escapes backslashes once more.
        quoted.replace('\\', "\\\\")
    }

    #[cfg(test)]
    #[test]
    fn quotes_exec_arguments() {
        assert_eq!(quote("/opt/b33p/b33p"), r#""/opt/b33p/b33p""#);
        assert_eq!(quote("/home/a b/$x"), r#""/home/a b/\\$x""#);
    }
}

#[cfg(windows)]
mod platform {
    use windows_registry::CURRENT_USER;

    use super::AutostartError;

    pub const SUPPORTED: bool = true;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "b33p";

    pub fn enabled() -> bool {
        CURRENT_USER
            .open(RUN)
            .and_then(|run| run.get_string(VALUE))
            .is_ok()
    }

    pub fn set(enabled: bool) -> Result<(), AutostartError> {
        let run = CURRENT_USER
            .create(RUN)
            .map_err(|source| AutostartError::OpenKey { key: RUN, source })?;
        if !enabled {
            return match run.remove_value(VALUE) {
                Ok(()) => Ok(()),
                Err(_) if !self::enabled() => Ok(()),
                Err(source) => Err(AutostartError::RemoveValue {
                    value: VALUE,
                    source,
                }),
            };
        }
        let executable = super::executable()?;
        let command = format!("\"{}\" {}", executable.display(), super::HIDDEN);
        run.set_string(VALUE, command)
            .map_err(|source| AutostartError::SetValue {
                value: VALUE,
                source,
            })
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
mod platform {
    use super::AutostartError;

    pub const SUPPORTED: bool = false;

    pub fn enabled() -> bool {
        false
    }

    pub fn set(_: bool) -> Result<(), AutostartError> {
        Err(AutostartError::Unsupported)
    }
}

pub use platform::{SUPPORTED, enabled, set};
