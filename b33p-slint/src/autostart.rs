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

/// What to start: the AppImage rather than the binary unpacked from it, which moves.
// Not on macOS, which has no autostart here yet.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn executable() -> io::Result<PathBuf> {
    match std::env::var_os("APPIMAGE") {
        Some(appimage) => Ok(appimage.into()),
        None => std::env::current_exe(),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::io;
    use std::path::PathBuf;

    pub const SUPPORTED: bool = true;

    fn entry() -> io::Result<PathBuf> {
        let dirs = directories::BaseDirs::new()
            .ok_or_else(|| io::Error::other("could not determine a config directory"))?;
        Ok(dirs
            .config_dir()
            .join("autostart/io.github.barafael.B33p.desktop"))
    }

    pub fn enabled() -> bool {
        entry().is_ok_and(|entry| entry.exists())
    }

    pub fn set(enabled: bool) -> io::Result<()> {
        let entry = entry()?;
        if !enabled {
            return match std::fs::remove_file(&entry) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        let executable = super::executable()?;
        let executable = executable
            .to_str()
            .ok_or_else(|| io::Error::other("the path to b33p isn't valid UTF-8"))?;
        if let Some(dir) = entry.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(
            entry,
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
    use std::io;

    use windows_registry::CURRENT_USER;

    pub const SUPPORTED: bool = true;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "b33p";

    pub fn enabled() -> bool {
        CURRENT_USER
            .open(RUN)
            .and_then(|run| run.get_string(VALUE))
            .is_ok()
    }

    pub fn set(enabled: bool) -> io::Result<()> {
        let run = CURRENT_USER.create(RUN).map_err(io::Error::other)?;
        if !enabled {
            return match run.remove_value(VALUE) {
                Ok(()) => Ok(()),
                Err(_) if !self::enabled() => Ok(()),
                Err(error) => Err(io::Error::other(error)),
            };
        }
        let executable = super::executable()?;
        let command = format!("\"{}\" {}", executable.display(), super::HIDDEN);
        run.set_string(VALUE, command).map_err(io::Error::other)
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
mod platform {
    use std::io;

    pub const SUPPORTED: bool = false;

    pub fn enabled() -> bool {
        false
    }

    pub fn set(_: bool) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

pub use platform::{SUPPORTED, enabled, set};
