//! What only Android can say, which `b33p-core` can't find out by itself.

/// Tells the core where this app keeps its files, before anything opens them.
///
/// Android gives every app one private directory (`/data/data/<package>/files`) that no other
/// app can read, and has none of the places `directories` looks in. The app's process is named
/// after its package, so the directory follows from that: no call into Java needed.
pub fn init() {
    #[cfg(target_os = "android")]
    match files_dir() {
        Some(dir) => {
            if !b33p_core::settings::use_app_dir(dir) {
                tracing::warn!("the app directory was already set");
            }
        }
        None => tracing::error!("could not find the app's private files directory"),
    }
}

#[cfg(target_os = "android")]
fn files_dir() -> Option<std::path::PathBuf> {
    let cmdline = std::fs::read("/proc/self/cmdline").ok()?;
    let name = cmdline.split(|byte| *byte == 0).next()?;
    let package = std::str::from_utf8(name).ok()?;
    if !is_package_name(package) {
        // A helper process (`package:name`) or something unexpected: not ours to guess for.
        return None;
    }
    let dir = std::path::Path::new("/data/data")
        .join(package)
        .join("files");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Whether `name` is made of what Android allows in a package name, so that it can't name a
/// path outside `/data/data`.
#[cfg(any(target_os = "android", test))]
fn is_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.contains('.')
        && name.split('.').all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_are_told_from_everything_else() {
        assert!(is_package_name("io.github.barafael.b33p"));
        assert!(is_package_name("com.example.B33PDioxus"));
        for not in [
            "",
            "b33p",
            "com.example:service",
            "../etc",
            "com..example",
            "com.example/",
            "com.exa mple",
        ] {
            assert!(!is_package_name(not), "{not:?}");
        }
    }
}
