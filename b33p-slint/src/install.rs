//! Installing b33p for one user on Windows, without admin rights or an installer.
//!
//! A release build started from anywhere but `%LOCALAPPDATA%\b33p\b33p.exe` (the unpacked zip,
//! say) copies itself there, adds a Start menu shortcut and an entry under "Installed apps"
//! (`HKCU\…\Uninstall\b33p`), starts the installed copy and ends. One fixed place keeps the
//! shortcut, autostart and later updates pointing at the same file: before, the `Run` value
//! held wherever the zip was unpacked, so moving or unpacking it again silently broke starting
//! with Windows.
//!
//! An installed version newer than this one is never replaced: its database may have been
//! migrated already, and this one couldn't read it. It is started instead. The same version is
//! replaced only if its file differs (another build of the same number, or a damaged copy);
//! the same file is only started, so starting the zip again changes nothing.
//!
//! `b33p.exe --uninstall`, which "Installed apps" runs, removes all of it, and the account's
//! data only if the user says so.
//!
//! Debug builds, `B33P_NO_INSTALL` and the mock leave everything where it is.
//!
//! Ported from Othereum/sigwin 80e3b62 (`src/install.rs`).

// The plans are made everywhere, so their tests run on Linux too; only Windows carries them out.
#![cfg_attr(not(windows), allow(dead_code))]

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// What "Installed apps" runs to remove b33p.
pub const UNINSTALL: &str = "--uninstall";

/// Set (to anything), b33p runs where it is and installs nothing.
pub const NO_INSTALL: &str = "B33P_NO_INSTALL";

/// The "Installed apps" entry, under `HKEY_CURRENT_USER`.
pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\b33p";

/// Where an installed b33p lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// `%LOCALAPPDATA%\b33p`: the program, and the notifications' icon (`b33p-notify`).
    pub home: PathBuf,
    /// `%LOCALAPPDATA%\b33p\b33p.exe`.
    pub exe: PathBuf,
    /// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\b33p.lnk`.
    pub shortcut: PathBuf,
}

impl Layout {
    /// The layout under the given `%LOCALAPPDATA%` and `%APPDATA%`.
    pub fn new(local_app_data: &Path, app_data: &Path) -> Self {
        let home = local_app_data.join("b33p");
        Self {
            exe: home.join("b33p.exe"),
            home,
            shortcut: app_data
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("b33p.lnk"),
        }
    }

    /// Where the installed exe is moved aside while it is replaced: a running exe can't be
    /// overwritten or deleted, but it can be renamed. Deleted on the next start.
    pub fn old_exe(&self) -> PathBuf {
        self.home.join("b33p.exe.old")
    }
}

/// Why this start would leave things where they are.
#[derive(Clone, Copy, Debug, Default)]
pub struct Skip {
    /// A debug build: a developer's, from `target/`.
    pub debug: bool,
    /// `B33P_NO_INSTALL` is set.
    pub opted_out: bool,
    /// The mock backend, which touches no account.
    pub mock: bool,
}

impl Skip {
    fn any(self) -> bool {
        self.debug || self.opted_out || self.mock
    }
}

/// What a start does about installing.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Run here, as is: installing is skipped, or this is the installed copy.
    Run,
    /// Copy this exe to the installed place (over an older or different copy, `upgrade`),
    /// register it, start it and end.
    Install { upgrade: bool },
    /// The installed copy is newer, or this very file: start it and end.
    StartInstalled,
}

/// Decides what a start does. `installed` is the installed copy's version, `None` if there is
/// none; `Some("")` if one is there but says no version (damaged, so it is replaced).
/// `same_file` is asked only when the versions are equal: comparing reads both files.
pub fn plan(
    skip: Skip,
    running_installed: bool,
    installed: Option<&str>,
    own: &str,
    same_file: impl FnOnce() -> bool,
) -> Plan {
    if skip.any() || running_installed {
        return Plan::Run;
    }
    let Some(installed) = installed else {
        return Plan::Install { upgrade: false };
    };
    let order = b33p_core::version::Version::parse(installed)
        .zip(b33p_core::version::Version::parse(own))
        .map(|(installed, own)| installed.cmp(&own));
    match order {
        Some(Ordering::Greater) => Plan::StartInstalled,
        Some(Ordering::Equal) if same_file() => Plan::StartInstalled,
        _ => Plan::Install { upgrade: true },
    }
}

/// The version in `b33p --version`'s answer ("b33p 0.2.0").
pub fn version_from_answer(answer: &str) -> Option<&str> {
    let version = answer.trim().strip_prefix("b33p ")?.trim();
    b33p_core::version::Version::parse(version).map(|_| version)
}

/// One step of uninstalling, in the order [`uninstall_plan`] gives them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    RemoveShortcut(PathBuf),
    RemoveUninstallEntry,
    /// The `Run` value (`autostart.rs`).
    RemoveAutostart,
    /// The AppUserModelID and the toasts' icon (`b33p_notify::unregister`).
    UnregisterNotifications,
    /// A folder of the account's data, with everything in it.
    RemoveData(PathBuf),
    /// A folder that held only data folders, if nothing else is in it now.
    RemoveIfEmpty(PathBuf),
    /// The database key in Credential Manager: after the database, as wiping does, since
    /// while it exists the key is the only way into it.
    ForgetDatabaseKey,
    /// The installed exe and its folder. The running exe can't delete itself, so it is
    /// deleted once this process has ended.
    RemoveProgram {
        home: PathBuf,
        exe: PathBuf,
    },
}

/// What uninstalling does. `data` are the account's folders (messages and keys, settings,
/// the log), given only if the user chose to delete them too.
pub fn uninstall_plan(layout: &Layout, data: Option<&[PathBuf]>) -> Vec<Step> {
    let mut steps = vec![
        Step::RemoveShortcut(layout.shortcut.clone()),
        Step::RemoveUninstallEntry,
        Step::RemoveAutostart,
        Step::UnregisterNotifications,
    ];
    if let Some(data) = data {
        steps.extend(data.iter().cloned().map(Step::RemoveData));
        // `…\barafael\b33p\data` leaves `…\barafael\b33p` and `…\barafael`, once empty.
        let mut parents: Vec<PathBuf> = Vec::new();
        for dir in data {
            for parent in dir.ancestors().skip(1).take(2) {
                if !parents.iter().any(|p| p == parent) {
                    parents.push(parent.to_owned());
                }
            }
        }
        // Deepest first, so that a parent is tried once its children are gone.
        parents.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        steps.extend(parents.into_iter().map(Step::RemoveIfEmpty));
        steps.push(Step::ForgetDatabaseKey);
    }
    steps.push(Step::RemoveProgram {
        home: layout.home.clone(),
        exe: layout.exe.clone(),
    });
    steps
}

/// `"C:\…\b33p.exe" --uninstall`, as the Uninstall key's `UninstallString`.
pub fn uninstall_command(exe: &Path) -> String {
    format!("\"{}\" {UNINSTALL}", exe.display())
}

/// Whether two paths name the same file, as Windows compares them: without regard to case.
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase(),
        _ => false,
    }
}

/// Whether two files hold the same bytes.
pub fn same_contents(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let open = |path: &Path| -> std::io::Result<(u64, std::fs::File)> {
        let file = std::fs::File::open(path)?;
        Ok((file.metadata()?.len(), file))
    };
    let (Ok((len_a, a)), Ok((len_b, b))) = (open(a), open(b)) else {
        return false;
    };
    if len_a != len_b {
        return false;
    }
    let (mut a, mut b) = (std::io::BufReader::new(a), std::io::BufReader::new(b));
    let (mut chunk_a, mut chunk_b) = (vec![0; 64 * 1024], vec![0; 64 * 1024]);
    loop {
        let read = match a.read(&mut chunk_a) {
            Ok(0) => return true,
            Ok(read) => read,
            Err(_) => return false,
        };
        if b.read_exact(&mut chunk_b[..read]).is_err() || chunk_a[..read] != chunk_b[..read] {
            return false;
        }
    }
}

/// What `main` does after [`on_start`].
#[derive(Debug, PartialEq, Eq)]
pub enum Flow {
    /// Carry on starting.
    Continue,
    /// End now: the installed copy was started, or installing failed and said why.
    Exit,
}

/// Installs or hands over to the installed copy, if this start should; see the module's doc.
pub fn on_start() -> Flow {
    #[cfg(windows)]
    return win::on_start();
    #[cfg(not(windows))]
    Flow::Continue
}

/// The running exe, if it is the installed copy: only that one updates itself.
pub fn installed_self() -> Option<PathBuf> {
    #[cfg(windows)]
    return win::installed_self();
    #[cfg(not(windows))]
    None
}

/// `b33p --uninstall`.
pub fn uninstall() {
    #[cfg(windows)]
    win::uninstall();
    #[cfg(not(windows))]
    println!("Only the Windows build installs itself; elsewhere, remove b33p as you installed it.");
}

#[cfg(windows)]
mod win {
    use std::ffi::OsString;
    use std::fs;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use windows::Win32::UI::WindowsAndMessaging::{
        IDNO, IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK,
        MB_YESNOCANCEL, MESSAGEBOX_RESULT, MESSAGEBOX_STYLE, MessageBoxW,
    };
    use windows::core::HSTRING;
    use windows_registry::CURRENT_USER;

    use super::{Flow, Layout, Plan, Skip, Step, UNINSTALL_KEY};
    use b33p_platform::{autostart, instance};

    use crate::APP;

    /// No console window for the installed copy's `--version`.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    impl Layout {
        fn user() -> Option<Self> {
            Some(Self::new(
                Path::new(&std::env::var_os("LOCALAPPDATA")?),
                Path::new(&std::env::var_os("APPDATA")?),
            ))
        }
    }

    fn skip() -> Skip {
        Skip {
            debug: cfg!(debug_assertions),
            opted_out: std::env::var_os(super::NO_INSTALL).is_some(),
            mock: b33p_core::Backend::mock_requested(),
        }
    }

    pub fn installed_self() -> Option<PathBuf> {
        if skip().any() {
            return None;
        }
        let layout = Layout::user()?;
        let exe = std::env::current_exe().ok()?;
        super::same_path(&exe, &layout.exe).then_some(layout.exe)
    }

    pub fn on_start() -> Flow {
        let skip = skip();
        if skip.any() {
            return Flow::Continue;
        }
        let (Some(layout), Ok(exe)) = (Layout::user(), std::env::current_exe()) else {
            return Flow::Continue;
        };
        let running_installed = super::same_path(&exe, &layout.exe);
        let installed = if layout.exe.exists() {
            Some(installed_version(&layout).unwrap_or_default())
        } else {
            None
        };
        let plan = super::plan(
            skip,
            running_installed,
            installed.as_deref(),
            b33p_core::version::CURRENT,
            || super::same_contents(&exe, &layout.exe),
        );
        tracing::info!(?plan, installed = ?installed, "installing");
        match plan {
            Plan::Run => {
                if running_installed {
                    settle(&layout);
                }
                Flow::Continue
            }
            Plan::StartInstalled => {
                start(&layout);
                Flow::Exit
            }
            Plan::Install { upgrade } => {
                match install(&layout, &exe, upgrade) {
                    Ok(()) => start(&layout),
                    Err(error) => {
                        message(
                            &format!("b33p couldn't be installed.\n\n{error}"),
                            MB_OK | MB_ICONERROR,
                        );
                    }
                }
                Flow::Exit
            }
        }
    }

    /// The installed copy, started: what an update moved aside goes, and the registration
    /// follows the version an update swapped in.
    fn settle(layout: &Layout) {
        match fs::remove_file(layout.old_exe()) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                tracing::warn!(error = %b33p_core::scrub::chain(&error), "can't remove the replaced b33p.exe");
            }
            _ => {}
        }
        if let Err(error) = register(layout) {
            tracing::warn!(error = %b33p_core::scrub::chain(&error), "can't update the Installed apps entry");
        }
        if autostart::enabled()
            && let Err(error) = autostart::point_at(&layout.exe)
        {
            tracing::warn!(error = %b33p_core::scrub::chain(&error), "can't point autostart at the installed b33p");
        }
    }

    /// The installed copy's version, as it says itself: the registry can be behind it, or
    /// gone. Its `--version` is answered before anything starts, in every version of b33p.
    fn installed_version(layout: &Layout) -> Option<String> {
        let output = Command::new(&layout.exe)
            .arg("--version")
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let answered = match output {
            Ok(output) if output.status.success() => {
                let answer = String::from_utf8_lossy(&output.stdout).into_owned();
                super::version_from_answer(&answer).map(str::to_owned)
            }
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(error = %b33p_core::scrub::chain(&error), "the installed b33p doesn't say its version");
                None
            }
        };
        answered.or_else(|| {
            CURRENT_USER
                .open(UNINSTALL_KEY)
                .and_then(|key| key.get_string("DisplayVersion"))
                .ok()
        })
    }

    fn install(layout: &Layout, exe: &Path, upgrade: bool) -> Result<(), String> {
        // A b33p still running would keep the old version's window, and the new one would
        // only show it.
        if !instance::quit_running() {
            return Err(
                "b33p is still running. Quit it (in the tray, Quit), then start this one again."
                    .into(),
            );
        }
        place(layout, exe).map_err(|error| b33p_core::Chain(&error).to_string())?;
        register(layout).map_err(|error| {
            format!(
                "The Installed apps entry couldn't be written: {}",
                b33p_core::Chain(&error)
            )
        })?;
        if let Err(error) = shortcut(layout) {
            tracing::warn!(error = %b33p_core::scrub::chain(&*error), "no Start menu shortcut");
        }
        if autostart::enabled()
            && let Err(error) = autostart::point_at(&layout.exe)
        {
            tracing::warn!(error = %b33p_core::scrub::chain(&error), "can't point autostart at the installed b33p");
        }
        tracing::info!(upgrade, "installed b33p {}", b33p_core::version::CURRENT);
        Ok(())
    }

    /// Copies `src` to the installed place. A running exe can't be overwritten, but it can be
    /// renamed, so the old one goes aside first, and comes back if copying fails.
    pub fn place(layout: &Layout, src: &Path) -> std::io::Result<()> {
        fs::create_dir_all(&layout.home)?;
        let old = layout.old_exe();
        let moved = layout.exe.exists() && {
            match fs::remove_file(&old) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    tracing::debug!(%error, "an earlier b33p.exe.old is still there");
                }
                _ => {}
            }
            fs::rename(&layout.exe, &old)?;
            true
        };
        if let Err(error) = fs::copy(src, &layout.exe) {
            if moved && let Err(error) = fs::rename(&old, &layout.exe) {
                tracing::warn!(%error, "can't put the old b33p.exe back");
            }
            return Err(error);
        }
        // Fails while the old one still runs; then the next start removes it (`settle`).
        if moved && fs::remove_file(&old).is_err() {
            tracing::debug!("the old b33p.exe stays until the next start");
        }
        Ok(())
    }

    /// The "Installed apps" entry.
    pub fn register(layout: &Layout) -> windows::core::Result<()> {
        let key = CURRENT_USER.create(UNINSTALL_KEY)?;
        let exe = layout.exe.display().to_string();
        key.set_string("DisplayName", "b33p")?;
        key.set_string("DisplayVersion", b33p_core::version::CURRENT)?;
        key.set_string("Publisher", "b33p contributors")?;
        key.set_string("DisplayIcon", format!("{exe},0"))?;
        key.set_string("UninstallString", super::uninstall_command(&layout.exe))?;
        key.set_string("InstallLocation", layout.home.display().to_string())?;
        key.set_string("URLInfoAbout", env!("CARGO_PKG_REPOSITORY"))?;
        key.set_u32("NoModify", 1)?;
        key.set_u32("NoRepair", 1)?;
        // In KiB, as Windows reads it.
        let size = fs::metadata(&layout.exe).map_or(0, |m| m.len() / 1024);
        key.set_u32("EstimatedSize", u32::try_from(size).unwrap_or(u32::MAX))?;
        Ok(())
    }

    /// The Start menu entry.
    pub fn shortcut(layout: &Layout) -> Result<(), Box<dyn std::error::Error>> {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            IPersistFile,
        };
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        use windows::core::Interface;

        if let Some(dir) = layout.shortcut.parent() {
            fs::create_dir_all(dir)?;
        }
        // SAFETY: plain COM calls on this thread, before any window exists. S_FALSE (COM
        // already set up here) is fine too.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().ok();
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(layout.exe.as_os_str()))?;
            link.SetWorkingDirectory(&HSTRING::from(layout.home.as_os_str()))?;
            link.SetDescription(&HSTRING::from("Signal messenger client"))?;
            link.cast::<IPersistFile>()?
                .Save(&HSTRING::from(layout.shortcut.as_os_str()), true)?;
        }
        Ok(())
    }

    /// Starts the installed copy with this start's arguments (`--hidden`, say).
    fn start(layout: &Layout) {
        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        let started = Command::new(&layout.exe)
            .args(args)
            .current_dir(&layout.home)
            .spawn();
        if let Err(error) = started {
            message(
                &format!(
                    "The installed b33p couldn't be started.\n\n{}",
                    b33p_core::Chain(&error)
                ),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    pub fn uninstall() {
        let Some(layout) = Layout::user() else { return };
        let answer = message(
            "Remove b33p from this computer?\n\n\
             Also delete your messages, keys and settings here?\n\n\
             Yes: delete them too.\n\
             No: keep them; installing b33p again carries on where you left off.\n\
             Cancel: keep b33p installed.",
            MB_YESNOCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
        );
        let delete_data = match answer {
            IDYES => true,
            IDNO => false,
            _ => return,
        };
        if !instance::quit_running() {
            message(
                "b33p is still running, so it wasn't removed. Quit it (in the tray, Quit), \
                 then try again.",
                MB_OK | MB_ICONERROR,
            );
            return;
        }
        // The folder being removed mustn't be this process's working directory.
        std::env::set_current_dir(std::env::temp_dir()).ok();
        let data = delete_data.then(data_dirs);
        let mut failed = Vec::new();
        for step in super::uninstall_plan(&layout, data.as_deref()) {
            if let Err(error) = carry_out(&step) {
                failed.push(format!("{step:?}: {error}"));
            }
        }
        if !failed.is_empty() {
            message(
                &format!(
                    "b33p was removed, but not all of it:\n\n{}",
                    failed.join("\n")
                ),
                MB_OK | MB_ICONERROR,
            );
        } else if delete_data {
            message(
                "b33p was removed.\n\nOn your phone, also remove it from Signal's linked \
                 devices (Settings, Linked devices).",
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    /// The account's folders: messages and keys, settings, and the log.
    fn data_dirs() -> Vec<PathBuf> {
        b33p_core::settings::project_dirs().map_or_else(Vec::new, |dirs| {
            vec![
                dirs.data_dir().to_owned(),
                dirs.config_dir().to_owned(),
                dirs.data_local_dir().to_owned(),
            ]
        })
    }

    fn carry_out(step: &Step) -> Result<(), String> {
        let gone = |result: std::io::Result<()>| match result {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        };
        match step {
            Step::RemoveShortcut(path) => gone(fs::remove_file(path)),
            Step::RemoveUninstallEntry => match CURRENT_USER.remove_tree(UNINSTALL_KEY) {
                Ok(()) => Ok(()),
                Err(_) if CURRENT_USER.open(UNINSTALL_KEY).is_err() => Ok(()),
                Err(error) => Err(error.to_string()),
            },
            Step::RemoveAutostart => {
                autostart::set(false).map_err(|error| b33p_core::Chain(&error).to_string())
            }
            Step::UnregisterNotifications => {
                b33p_notify::unregister(&APP);
                Ok(())
            }
            Step::RemoveData(dir) => gone(fs::remove_dir_all(dir)),
            Step::RemoveIfEmpty(dir) => {
                // Not empty, or gone: either way it stays as it is.
                fs::remove_dir(dir).ok();
                Ok(())
            }
            Step::ForgetDatabaseKey => {
                #[cfg(feature = "signal")]
                return b33p_core::forget_database_key().map_err(|error| error.to_string());
                #[cfg(not(feature = "signal"))]
                Ok(())
            }
            Step::RemoveProgram { home, exe } => remove_program(home, exe),
        }
    }

    /// The installed exe and its folder. The running exe can only be deleted once it has
    /// ended: `self_replace` copies it out of the folder, starts the copy to wait for this
    /// process to end and delete the original, and the copy deletes itself as it closes
    /// (`FILE_FLAG_DELETE_ON_CLOSE`). Chosen over a delayed `cmd /c del`, which guesses how
    /// long the exit takes, flashes a console window and quotes a path through the shell.
    fn remove_program(home: &Path, exe: &Path) -> Result<(), String> {
        let running = std::env::current_exe().is_ok_and(|me| super::same_path(&me, exe));
        if running {
            self_replace::self_delete_outside_path(home).map_err(|error| error.to_string())?;
        }
        // The rest: an exe that isn't this one, its `.old`, what else is left.
        if let Ok(entries) = fs::read_dir(home) {
            for entry in entries.flatten() {
                let path = entry.path();
                if running && super::same_path(&path, exe) {
                    continue;
                }
                let removed = if path.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
                if let Err(error) = removed {
                    tracing::warn!(%error, "can't remove {}", path.display());
                }
            }
        }
        // Only if empty; `self_replace` removes it after the exe otherwise.
        fs::remove_dir(home).ok();
        Ok(())
    }

    fn message(text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
        // SAFETY: two strings that outlive the call, and no owner window.
        unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("b33p"), style) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn layout(tag: &str) -> Layout {
            let root =
                std::env::temp_dir().join(format!("b33p-install-{tag}-{}", std::process::id()));
            fs::remove_dir_all(&root).ok();
            Layout::new(&root.join("local"), &root.join("roaming"))
        }

        #[test]
        fn places_over_an_open_exe() {
            let layout = layout("place");
            let src = layout.home.with_file_name("download.exe");
            fs::create_dir_all(src.parent().unwrap()).unwrap();
            fs::write(&src, b"one").unwrap();
            place(&layout, &src).unwrap();
            assert_eq!(fs::read(&layout.exe).unwrap(), b"one");
            // Held open, as a running exe is.
            let held = fs::File::open(&layout.exe).unwrap();
            fs::write(&src, b"two").unwrap();
            place(&layout, &src).unwrap();
            drop(held);
            assert_eq!(fs::read(&layout.exe).unwrap(), b"two");
            fs::remove_dir_all(layout.home.parent().unwrap().parent().unwrap()).ok();
        }

        #[test]
        fn makes_a_shortcut() {
            let layout = layout("shortcut");
            fs::create_dir_all(&layout.home).unwrap();
            fs::write(&layout.exe, b"exe").unwrap();
            shortcut(&layout).unwrap();
            assert!(layout.shortcut.exists());
            fs::remove_dir_all(layout.home.parent().unwrap().parent().unwrap()).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: Skip = Skip {
        debug: false,
        opted_out: false,
        mock: false,
    };

    fn never() -> bool {
        panic!("files compared when the versions differ")
    }

    #[test]
    fn lays_out_under_the_user_folders() {
        let layout = Layout::new(Path::new("L"), Path::new("R"));
        assert_eq!(layout.exe, Path::new("L").join("b33p").join("b33p.exe"));
        assert_eq!(
            layout.old_exe(),
            Path::new("L").join("b33p").join("b33p.exe.old")
        );
        assert!(
            layout
                .shortcut
                .starts_with(Path::new("R").join("Microsoft"))
        );
        assert!(
            layout
                .shortcut
                .ends_with(Path::new("Start Menu").join("Programs").join("b33p.lnk"))
        );
    }

    #[test]
    fn skipped_builds_and_the_installed_copy_run_as_they_are() {
        for skip in [
            Skip {
                debug: true,
                ..RELEASE
            },
            Skip {
                opted_out: true,
                ..RELEASE
            },
            Skip {
                mock: true,
                ..RELEASE
            },
        ] {
            assert_eq!(plan(skip, false, None, "0.2.0", never), Plan::Run);
        }
        assert_eq!(
            plan(RELEASE, true, Some("0.2.0"), "0.2.0", never),
            Plan::Run
        );
    }

    #[test]
    fn installs_where_nothing_is() {
        assert_eq!(
            plan(RELEASE, false, None, "0.2.0", never),
            Plan::Install { upgrade: false }
        );
    }

    #[test]
    fn never_replaces_a_newer_version() {
        assert_eq!(
            plan(RELEASE, false, Some("0.3.0"), "0.2.0", never),
            Plan::StartInstalled
        );
        assert_eq!(
            plan(RELEASE, false, Some("0.10.0"), "0.9.9", never),
            Plan::StartInstalled
        );
    }

    #[test]
    fn replaces_an_older_version() {
        assert_eq!(
            plan(RELEASE, false, Some("0.1.9"), "0.2.0", never),
            Plan::Install { upgrade: true }
        );
        assert_eq!(
            plan(RELEASE, false, Some("0.2.0-rc.1"), "0.2.0", never),
            Plan::Install { upgrade: true }
        );
    }

    #[test]
    fn the_same_version_is_replaced_only_by_another_file() {
        assert_eq!(
            plan(RELEASE, false, Some("0.2.0"), "0.2.0", || true),
            Plan::StartInstalled
        );
        assert_eq!(
            plan(RELEASE, false, Some("0.2.0"), "0.2.0", || false),
            Plan::Install { upgrade: true }
        );
    }

    #[test]
    fn a_copy_without_a_version_is_replaced() {
        assert_eq!(
            plan(RELEASE, false, Some(""), "0.2.0", never),
            Plan::Install { upgrade: true }
        );
    }

    #[test]
    fn reads_the_version_answer() {
        assert_eq!(version_from_answer("b33p 0.2.0\r\n"), Some("0.2.0"));
        assert_eq!(version_from_answer("b33p 0.3.0-rc.1"), Some("0.3.0-rc.1"));
        assert_eq!(version_from_answer("something else"), None);
        assert_eq!(version_from_answer(""), None);
    }

    #[test]
    fn uninstalling_keeps_the_data_unless_asked() {
        let layout = Layout::new(Path::new("L"), Path::new("R"));
        let steps = uninstall_plan(&layout, None);
        assert_eq!(
            steps,
            [
                Step::RemoveShortcut(layout.shortcut.clone()),
                Step::RemoveUninstallEntry,
                Step::RemoveAutostart,
                Step::UnregisterNotifications,
                Step::RemoveProgram {
                    home: layout.home.clone(),
                    exe: layout.exe.clone()
                },
            ]
        );
        assert!(!steps.contains(&Step::ForgetDatabaseKey));
    }

    #[test]
    fn uninstalling_with_the_data_removes_its_folders_then_the_key() {
        let layout = Layout::new(Path::new("L"), Path::new("R"));
        let roaming = Path::new("R").join("barafael").join("b33p");
        let local = Path::new("L").join("barafael").join("b33p");
        let data = [
            roaming.join("data"),
            roaming.join("config"),
            local.join("data"),
        ];
        let steps = uninstall_plan(&layout, Some(&data));
        assert_eq!(
            steps[4..],
            [
                Step::RemoveData(data[0].clone()),
                Step::RemoveData(data[1].clone()),
                Step::RemoveData(data[2].clone()),
                Step::RemoveIfEmpty(roaming.clone()),
                Step::RemoveIfEmpty(local.clone()),
                Step::RemoveIfEmpty(Path::new("R").join("barafael")),
                Step::RemoveIfEmpty(Path::new("L").join("barafael")),
                Step::ForgetDatabaseKey,
                Step::RemoveProgram {
                    home: layout.home.clone(),
                    exe: layout.exe.clone()
                },
            ]
        );
    }

    #[test]
    fn uninstall_command_quotes_the_path() {
        let exe = Path::new(r"C:\Users\A B\AppData\Local\b33p\b33p.exe");
        assert_eq!(
            uninstall_command(exe),
            r#""C:\Users\A B\AppData\Local\b33p\b33p.exe" --uninstall"#
        );
    }

    #[test]
    fn compares_files() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        std::fs::write(&a, vec![7u8; 200_000]).unwrap();
        std::fs::write(&b, vec![7u8; 200_000]).unwrap();
        let mut other = vec![7u8; 200_000];
        other[150_000] = 8;
        std::fs::write(&c, other).unwrap();
        assert!(same_contents(&a, &b));
        assert!(!same_contents(&a, &c));
        assert!(!same_contents(&a, &dir.path().join("missing")));
        assert!(same_path(&a, &dir.path().join(".").join("a")));
        assert!(!same_path(&a, &b));
    }
}
