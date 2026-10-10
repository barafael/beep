//! Private scratch directories for files handed to other programs: attachments opened in the
//! desktop's default app, and pictures pasted into the composer.
//!
//! On Linux they go in the per-session runtime directory (`XDG_RUNTIME_DIR`): private to the
//! user, memory-backed and cleared at logout. Without one, the system's temporary directory is
//! shared with every user on the machine, so a fixed name there could be planted beforehand
//! (a directory someone else can read, or a symbolic link pointing wherever they like); each
//! run then gets a directory of its own, made by this process and nobody else.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Directories made by [`dir`] in the shared temporary directory, by name.
static PER_RUN: Mutex<Vec<(String, PathBuf)>> = Mutex::new(Vec::new());

/// A directory called `name`, readable by this user only, created if need be. In the shared
/// temporary directory, one made by this run.
pub fn dir(name: &str) -> io::Result<PathBuf> {
    if let Some(base) = runtime_dir() {
        let path = base.join(name);
        return match create_private(&path) {
            Ok(()) => Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // Ours, from earlier in this run; the runtime directory is private, so
                // nobody else could have put it there.
                if fs::symlink_metadata(&path)?.is_dir() {
                    restrict(&path)?;
                    Ok(path)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{} is not a directory", path.display()),
                    ))
                }
            }
            Err(error) => Err(error),
        };
    }
    let mut per_run = PER_RUN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, path)) = per_run.iter().find(|(n, _)| n == name) {
        return Ok(path.clone());
    }
    let base = std::env::temp_dir();
    let pid = std::process::id();
    // Whatever is there already (left by a crashed run, or put there by someone else) is
    // not used: a fresh directory is made, under another name if need be.
    let mut error = None;
    for attempt in 0..100 {
        let path = if attempt == 0 {
            base.join(format!("{name}-{pid}"))
        } else {
            base.join(format!("{name}-{pid}-{attempt}"))
        };
        match create_private(&path) {
            Ok(()) => {
                per_run.push((name.to_owned(), path.clone()));
                return Ok(path);
            }
            Err(e) => error = Some(e),
        }
    }
    Err(error.expect("at least one attempt"))
}

/// Removes the directory `name`: what an earlier run left in the runtime directory (whose
/// files other programs have read by now), or this run's own.
pub fn clean(name: &str) {
    if let Some(base) = runtime_dir() {
        let _ = fs::remove_dir_all(base.join(name));
    }
    let mut per_run = PER_RUN.lock().unwrap_or_else(|e| e.into_inner());
    per_run.retain(|(n, path)| {
        if n == name {
            let _ = fs::remove_dir_all(path);
            false
        } else {
            true
        }
    });
}

/// The user's private runtime directory, if the session has one.
fn runtime_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    dir.is_dir().then_some(dir)
}

/// Makes `path` as a new directory readable by its owner only; fails if anything is there
/// already (a directory, a file or a symbolic link alike).
fn create_private(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

#[cfg(unix)]
fn restrict(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if fs::metadata(path)?.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Windows has no unix modes; the user's temporary directory already keeps others out.
#[cfg(not(unix))]
fn restrict(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_a_fresh_private_directory_and_removes_it() {
        let name = format!("b33p-test-{}", std::process::id());
        let made = dir(&name).unwrap();
        assert!(made.is_dir());
        assert_eq!(dir(&name).unwrap(), made, "the same one for the whole run");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&made).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        clean(&name);
        assert!(!made.exists());
    }

    /// Without a runtime directory, a directory planted under the expected name is left alone.
    #[test]
    fn never_uses_what_someone_else_put_there() {
        let planted = std::env::temp_dir().join(format!("b33p-planted-{}", std::process::id()));
        let _ = fs::remove_dir_all(&planted);
        fs::create_dir(&planted).unwrap();
        fs::write(planted.join("theirs"), b"").unwrap();
        assert!(create_private(&planted).is_err());
        let _ = fs::remove_dir_all(&planted);
    }
}
