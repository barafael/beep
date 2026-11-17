//! When this build was made, for its expiry (`src/expiry.rs`): `SOURCE_DATE_EPOCH` if set (as
//! reproducible builds set it), else the time of the commit it is built from, as Signal-Desktop
//! takes it (scripts/utils/getBuildCreationTimestamp.mjs). Neither, say from a source tarball:
//! no build time, and no expiry.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&manifest)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    // Again for another commit: HEAD moves, or a commit is made on its branch.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/logs/HEAD");
    }
    let time = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .or_else(|| git(&["log", "-1", "--format=%ct"]));
    if let Some(seconds) = time.and_then(|time| time.trim().parse::<u64>().ok()) {
        println!("cargo:rustc-env=B33P_BUILD_TIME={seconds}");
    }
}
