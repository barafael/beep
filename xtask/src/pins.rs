//! The testbox builds presage-cli and the sticker tool on the presage and libsignal-service b33p
//! pins (presage-cli on b33p's SQLCipher too), but names those revisions in files of their own: testbox/presage/Containerfile's
//! arguments, its lock, and testbox/stickers'. A test fails when they drift from b33p's
//! Cargo.toml, as they did when b33p took the session lock and the box's presage-cli didn't.

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The 40-hex-digit revisions that follow `marker` in `text`, in order.
    fn revisions_after(text: &str, marker: &str) -> Vec<String> {
        text.match_indices(marker)
            .filter_map(|(at, _)| {
                let rest = &text[at + marker.len()..];
                let revision: String = rest.chars().take_while(char::is_ascii_hexdigit).collect();
                (revision.len() == 40).then_some(revision)
            })
            .collect()
    }

    fn read(repo: &Path, path: &str) -> String {
        std::fs::read_to_string(repo.join(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    /// The one revision `marker` is followed by in `text`, however often.
    fn one(text: &str, marker: &str, what: &str) -> String {
        let mut found = revisions_after(text, marker);
        found.dedup();
        assert_eq!(found.len(), 1, "{what}: {found:?}");
        found.remove(0)
    }

    #[test]
    fn the_box_builds_on_b33ps_presage_and_libsignal_service() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let presage = "github.com/barafael/presage\", rev = \"";
        let service = "github.com/barafael/libsignal-service-rs\", rev = \"";
        let manifest = read(repo, "Cargo.toml");
        let b33p = (
            one(&manifest, presage, "b33p's presage"),
            one(&manifest, service, "b33p's libsignal-service"),
        );

        let containerfile = read(repo, "testbox/presage/Containerfile");
        let presage_cli = (
            one(
                &containerfile,
                "ARG PRESAGE_REV=",
                "the Containerfile's presage",
            ),
            one(
                &containerfile,
                "ARG LIBSIGNAL_SERVICE_REV=",
                "the Containerfile's libsignal-service",
            ),
        );
        assert_eq!(presage_cli, b33p, "testbox/presage/Containerfile");

        let lock = read(repo, "testbox/presage/presage-cli.lock");
        let locked = one(
            &lock,
            "barafael/libsignal-service-rs?rev=",
            "presage-cli.lock",
        );
        assert_eq!(locked, b33p.1, "testbox/presage/presage-cli.lock");

        let rusqlite = "barafael/rusqlite\", rev = \"";
        let sqlcipher = one(&manifest, rusqlite, "b33p's libsqlite3-sys");
        assert_eq!(
            one(
                &containerfile,
                "ARG RUSQLITE_REV=",
                "the Containerfile's libsqlite3-sys"
            ),
            sqlcipher,
            "testbox/presage/Containerfile"
        );
        assert_eq!(
            one(
                &lock,
                "barafael/rusqlite?rev=",
                "presage-cli.lock's libsqlite3-sys"
            ),
            sqlcipher,
            "testbox/presage/presage-cli.lock"
        );

        let stickers = read(repo, "testbox/stickers/Cargo.toml");
        let sticker_tool = (
            one(&stickers, presage, "the sticker tool's presage"),
            one(&stickers, service, "the sticker tool's libsignal-service"),
        );
        assert_eq!(sticker_tool, b33p, "testbox/stickers/Cargo.toml");
    }

    /// signal-cli's image builds the libsignal that signal-cli takes (its `libsignal-version`,
    /// which the Containerfile checks at the revision it fetches), patched and published under a
    /// version of its own: the Containerfile, the patch and the jar the image names agree on it.
    #[test]
    fn signal_clis_image_builds_the_libsignal_it_takes() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let containerfile = read(repo, "testbox/signal-cli/Containerfile");
        let patch = read(repo, "testbox/patches/libsignal-cli-local.patch");
        let version = containerfile
            .split_once("= 0.106.1")
            .map(|_| "0.106.1")
            .expect("the Containerfile checks signal-cli's libsignal-version");
        let testbox = format!("{version}-testbox");
        assert!(
            patch.contains(&format!("+    version = \"{testbox}\"")),
            "libsignal-cli-local.patch publishes libsignal {testbox}"
        );
        assert_eq!(
            containerfile
                .matches(&format!("libsignal-client-{testbox}.jar"))
                .count(),
            // The build's copy out, the Gradle step's and the build stage's: all the same jar.
            containerfile.matches("libsignal-client-").count(),
            "every jar the Containerfile names is libsignal-client-{testbox}.jar"
        );
    }
}
