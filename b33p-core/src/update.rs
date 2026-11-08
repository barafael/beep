//! Updating b33p from its signed GitHub releases. Windows' zip only, for now: the installed
//! copy (`%LOCALAPPDATA%\b33p\b33p.exe`, see b33p-slint's `install.rs`) is the one file to
//! replace. The AppImage could follow; distribution packages update themselves.
//!
//! - [`latest`] asks GitHub for the latest release and makes an [`Offer`] of it if it is newer
//!   than this build and has this platform's zip and its minisign signature.
//! - [`download`] streams the zip to disk while hashing it, checks the signature, and takes
//!   `b33p.exe` out of it. The signature's trusted comment must name that exact version and
//!   file (`b33p 0.3.0 b33p-0.3.0-windows-x86_64.zip`), so an older signed build can't be
//!   served again as a newer one, nor another platform's file in its place.
//! - [`swap_in`] renames the running exe aside (Windows allows that) and moves the new one
//!   to its place; the next start of the installed copy removes the old one.
//!
//! The key to check signatures with comes in at build time, from `B33P_UPDATE_PUBLIC_KEY`
//! (the release workflow takes it from a repository variable). A build without it has no
//! [`PUBLIC_KEY`], and the app offers no updates at all.
//!
//! Ported from Othereum/sigwin 80e3b62 (`src/update.rs`).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use minisign_verify::{PublicKey, Signature};

use crate::version::Version;

/// The minisign public key releases are signed with, as the second line of minisign's `.pub`
/// file; `None` in a build made without one, which then never updates itself.
pub const PUBLIC_KEY: Option<&str> = match option_env!("B33P_UPDATE_PUBLIC_KEY") {
    // An unset repository variable comes through as an empty string.
    Some(key) if !key.is_empty() => Some(key),
    _ => None,
};

/// Where GitHub says which release is the latest (drafts and pre-releases never are).
pub const LATEST_URL: &str = "https://api.github.com/repos/barafael/beep/releases/latest";

/// No b33p release comes near this; a larger download is refused as it streams in.
const MAX_DOWNLOAD: u64 = 200 << 20;

/// Why an update couldn't be had.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("this build has no key to check updates with")]
    NoKey,
    #[error("couldn't reach GitHub")]
    Network(#[from] reqwest::Error),
    #[error("GitHub's answer can't be read")]
    Answer(#[from] serde_json::Error),
    #[error("the update key can't be read")]
    Key(#[source] minisign_verify::Error),
    #[error("the signature can't be read")]
    SignatureFile(#[source] minisign_verify::Error),
    #[error("the signature is for {found:?}, not {expected:?}")]
    WrongRelease { found: String, expected: String },
    #[error("the download doesn't match its signature")]
    BadSignature(#[source] minisign_verify::Error),
    #[error("the download is larger than {MAX_DOWNLOAD} bytes")]
    TooLarge,
    #[error("the zip has no b33p.exe")]
    NoExe,
    #[error("the zip can't be read")]
    Zip(#[from] zip::result::ZipError),
    #[error("failed to write the update")]
    Io(#[from] std::io::Error),
}

/// A release, as GitHub's API describes it; only what an update needs.
#[derive(Debug, serde::Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, serde::Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

impl Release {
    pub fn parse(json: &str) -> Result<Self, UpdateError> {
        Ok(serde_json::from_str(json)?)
    }
}

/// A newer release this build can update to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// `0.3.0`, without the tag's `v`.
    pub version: String,
    /// The zip's file name, which the signature names too.
    pub asset: String,
    pub url: String,
    pub signature_url: String,
}

/// This build's architecture, as the release names its files.
pub fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// The Windows zip of `version`, as `release.yml` names it.
pub fn asset_name(version: &str, arch: &str) -> String {
    format!("b33p-{version}-windows-{arch}.zip")
}

/// What the signature of `asset` of `version` says in its trusted comment, as `release.yml`
/// signs it.
pub fn trusted_comment(version: &str, asset: &str) -> String {
    format!("b33p {version} {asset}")
}

/// The offer `release` makes to a build of version `current` on `arch`: `None` unless it is
/// newer and has the zip and its signature.
pub fn offer(release: &Release, current: &str, arch: &str) -> Option<Offer> {
    let version = release.tag_name.trim().trim_start_matches('v');
    if Version::parse(version)? <= Version::parse(current)? {
        return None;
    }
    let asset = asset_name(version, arch);
    let url = |name: &str| {
        release
            .assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
    };
    Some(Offer {
        version: version.to_owned(),
        url: url(&asset)?,
        signature_url: url(&format!("{asset}.minisig"))?,
        asset,
    })
}

/// Reads the key and the signature, and checks that the signature is meant for `offer`. Its
/// trusted comment is proven genuine only with the file it signs ([`Checked::finish`]).
pub fn check_signature(
    public_key: &str,
    signature: &str,
    offer: &Offer,
) -> Result<(PublicKey, Signature), UpdateError> {
    let key = PublicKey::from_base64(public_key.trim()).map_err(UpdateError::Key)?;
    let signature = Signature::decode(signature).map_err(UpdateError::SignatureFile)?;
    let expected = trusted_comment(&offer.version, &offer.asset);
    if signature.trusted_comment().trim() != expected {
        return Err(UpdateError::WrongRelease {
            found: signature.trusted_comment().to_owned(),
            expected,
        });
    }
    Ok((key, signature))
}

/// Checks a file against a signature as it streams by.
pub struct Checked<'a>(minisign_verify::StreamVerifier<'a>);

impl<'a> Checked<'a> {
    pub fn new(key: &'a PublicKey, signature: &'a Signature) -> Result<Self, UpdateError> {
        key.verify_stream(signature)
            .map(Self)
            .map_err(UpdateError::BadSignature)
    }

    pub fn update(&mut self, chunk: &[u8]) {
        self.0.update(chunk);
    }

    /// Whether the file and the trusted comment are the ones signed.
    pub fn finish(mut self) -> Result<(), UpdateError> {
        self.0.finalize().map_err(UpdateError::BadSignature)
    }
}

/// The one HTTP client, as `preview.rs` makes its own: GitHub needs a user agent.
fn client() -> Result<reqwest::Client, UpdateError> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        // Fails only if another thread installed one meanwhile, which does as well.
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
    }
    let client = reqwest::Client::builder()
        .user_agent(concat!("b33p/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .build()?;
    Ok(CLIENT.get_or_init(|| client).clone())
}

/// What the latest release offers this build, if anything.
pub async fn latest() -> Result<Option<Offer>, UpdateError> {
    let text = client()?
        .get(LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let release = Release::parse(&text)?;
    Ok(offer(&release, crate::version::CURRENT, arch()))
}

/// Downloads `offer` into `dir` and checks it; returns where the new `b33p.exe` is
/// (`b33p.exe.new` in `dir`). Nothing is left behind if it fails.
pub async fn download(offer: &Offer, dir: &Path) -> Result<PathBuf, UpdateError> {
    let public_key = PUBLIC_KEY.ok_or(UpdateError::NoKey)?;
    let client = client()?;
    let signature = client
        .get(&offer.signature_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let (key, signature) = check_signature(public_key, &signature, offer)?;
    let zip = dir.join("b33p-update.zip");
    let exe = dir.join("b33p.exe.new");
    let result = async {
        let mut checked = Checked::new(&key, &signature)?;
        let mut file = std::fs::File::create(&zip)?;
        let mut response = client.get(&offer.url).send().await?.error_for_status()?;
        let mut total = 0u64;
        while let Some(chunk) = response.chunk().await? {
            total += chunk.len() as u64;
            if total > MAX_DOWNLOAD {
                return Err(UpdateError::TooLarge);
            }
            checked.update(&chunk);
            file.write_all(&chunk)?;
        }
        file.sync_all()?;
        drop(file);
        checked.finish()?;
        extract_exe(&zip, &exe)
    }
    .await;
    std::fs::remove_file(&zip).ok();
    if result.is_err() {
        std::fs::remove_file(&exe).ok();
    }
    result.map(|()| exe)
}

/// Takes `b33p.exe` out of the release zip (`b33p/b33p.exe`) into `dest`.
pub fn extract_exe(zip: &Path, dest: &Path) -> Result<(), UpdateError> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?)?;
    let name = archive
        .file_names()
        .filter_map(Result::ok)
        .find(|name| {
            name == "b33p.exe" || name.ends_with("/b33p.exe") || name.ends_with("\\b33p.exe")
        })
        .map(|name| name.into_owned())
        .ok_or(UpdateError::NoExe)?;
    let entry = archive.by_name(&name)?;
    let mut out = std::fs::File::create(dest)?;
    // The size the zip claims can't be trusted; the bytes are counted as they come.
    let copied = std::io::copy(&mut entry.take(MAX_DOWNLOAD + 1), &mut out)?;
    if copied > MAX_DOWNLOAD {
        drop(out);
        std::fs::remove_file(dest).ok();
        return Err(UpdateError::TooLarge);
    }
    out.sync_all()?;
    Ok(())
}

/// Puts `new` in the place of `installed`, which may be running: Windows lets a running exe
/// be renamed, not overwritten. The old one goes to `old` (removed by the next start), and
/// comes back if the new one can't be moved in.
pub fn swap_in(installed: &Path, new: &Path, old: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(old) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            tracing::debug!(%error, "an earlier replaced exe is still there");
        }
        _ => {}
    }
    std::fs::rename(installed, old)?;
    if let Err(error) = std::fs::rename(new, installed) {
        if let Err(back) = std::fs::rename(old, installed) {
            tracing::warn!(error = %back, "can't put the old exe back");
        }
        return Err(error);
    }
    Ok(())
}

/// [`latest`] on a runtime of its own, for a thread of its own: nothing is held between
/// checks.
pub fn latest_blocking() -> Result<Option<Offer>, UpdateError> {
    runtime()?.block_on(latest())
}

/// Downloads `offer`, checks it and swaps it in for `installed` (moving the running exe to
/// `old`). Blocks; for a thread of its own.
pub fn install_blocking(offer: &Offer, installed: &Path, old: &Path) -> Result<(), UpdateError> {
    let dir = installed.parent().unwrap_or(Path::new("."));
    let new = runtime()?.block_on(download(offer, dir))?;
    let swapped = swap_in(installed, &new, old);
    if swapped.is_err() {
        std::fs::remove_file(&new).ok();
    }
    Ok(swapped?)
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use blake2::Digest;
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    /// minisign's formats: a key is "Ed", its 8-byte id and the Ed25519 public key; a
    /// signature line is "ED" (pre-hashed), the key id and the signature of BLAKE2b-512 of the
    /// file; the global signature signs that signature and the trusted comment.
    fn key_pair(seed: u8) -> (SigningKey, String) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let mut public = b"Ed".to_vec();
        public.extend_from_slice(&[seed; 8]);
        public.extend_from_slice(key.verifying_key().as_bytes());
        (key, B64.encode(public))
    }

    fn sign(key: &SigningKey, key_id: u8, data: &[u8], comment: &str) -> String {
        let signature = key.sign(&blake2::Blake2b512::digest(data)).to_bytes();
        let mut line = b"ED".to_vec();
        line.extend_from_slice(&[key_id; 8]);
        line.extend_from_slice(&signature);
        let mut global = signature.to_vec();
        global.extend_from_slice(comment.as_bytes());
        let global = key.sign(&global).to_bytes();
        format!(
            "untrusted comment: signature from minisign secret key\n{}\ntrusted comment: {comment}\n{}\n",
            B64.encode(line),
            B64.encode(global)
        )
    }

    fn offer_of(version: &str) -> Offer {
        let asset = asset_name(version, "x86_64");
        Offer {
            version: version.into(),
            url: format!("https://example.invalid/{asset}"),
            signature_url: format!("https://example.invalid/{asset}.minisig"),
            asset,
        }
    }

    /// Checks `chunks` against `signature` as [`download`] does.
    fn verify(
        public_key: &str,
        signature: &str,
        offer: &Offer,
        chunks: &[&[u8]],
    ) -> Result<(), UpdateError> {
        let (key, signature) = check_signature(public_key, signature, offer)?;
        let mut checked = Checked::new(&key, &signature)?;
        for chunk in chunks {
            checked.update(chunk);
        }
        checked.finish()
    }

    const ZIP: &str = "b33p-0.3.0-windows-x86_64.zip";

    #[test]
    fn accepts_a_good_signature_in_chunks() {
        let (key, public) = key_pair(7);
        let signature = sign(&key, 7, b"the new b33p", &format!("b33p 0.3.0 {ZIP}"));
        verify(
            &public,
            &signature,
            &offer_of("0.3.0"),
            &[b"the new ", b"b33p"],
        )
        .unwrap();
    }

    #[test]
    fn rejects_a_changed_file_and_a_foreign_key() {
        let (key, public) = key_pair(7);
        let signature = sign(&key, 7, b"the new b33p", &format!("b33p 0.3.0 {ZIP}"));
        let offer = offer_of("0.3.0");
        assert!(matches!(
            verify(&public, &signature, &offer, &[b"the new b33p!"]),
            Err(UpdateError::BadSignature(_))
        ));
        // Someone else's key, under their own id and under ours.
        let (other, other_public) = key_pair(9);
        assert!(verify(&other_public, &signature, &offer, &[b"the new b33p"]).is_err());
        let forged = sign(&other, 7, b"the new b33p", &format!("b33p 0.3.0 {ZIP}"));
        assert!(verify(&public, &forged, &offer, &[b"the new b33p"]).is_err());
        assert!(matches!(
            verify(&public, "not a signature", &offer, &[]),
            Err(UpdateError::SignatureFile(_))
        ));
    }

    #[test]
    fn rejects_an_old_build_served_as_a_new_one() {
        let (key, public) = key_pair(7);
        let old_zip = asset_name("0.2.0", "x86_64");
        let old = sign(&key, 7, b"the old b33p", &format!("b33p 0.2.0 {old_zip}"));
        // Genuine, but for 0.2.0: offered as 0.3.0, it is refused before downloading.
        assert!(matches!(
            check_signature(&public, &old, &offer_of("0.3.0")),
            Err(UpdateError::WrongRelease { .. })
        ));
        // The trusted comment edited to say 0.3.0: the global signature no longer holds.
        let edited = old.replace(
            &format!("b33p 0.2.0 {old_zip}"),
            &format!("b33p 0.3.0 {ZIP}"),
        );
        assert!(matches!(
            verify(&public, &edited, &offer_of("0.3.0"), &[b"the old b33p"]),
            Err(UpdateError::BadSignature(_))
        ));
        // Another platform's file of the right version.
        let arm = sign(&key, 7, b"arm", "b33p 0.3.0 b33p-0.3.0-windows-aarch64.zip");
        assert!(check_signature(&public, &arm, &offer_of("0.3.0")).is_err());
    }

    const LATEST: &str = include_str!("testdata/github-release-latest.json");

    #[test]
    fn reads_githubs_answer() {
        let release = Release::parse(LATEST).unwrap();
        assert_eq!(release.tag_name, "v0.3.0");
        assert_eq!(release.assets.len(), 5);
        assert!(Release::parse("{\"message\": \"Not Found\"}").is_err());
    }

    #[test]
    fn offers_only_a_newer_version_with_its_zip_and_signature() {
        let release = Release::parse(LATEST).unwrap();
        let offer = offer(&release, "0.2.0", "x86_64").unwrap();
        assert_eq!(offer.version, "0.3.0");
        assert_eq!(offer.asset, ZIP);
        assert_eq!(
            offer.url,
            "https://github.com/barafael/beep/releases/download/v0.3.0/b33p-0.3.0-windows-x86_64.zip"
        );
        assert_eq!(offer.signature_url, format!("{}.minisig", offer.url));
        // The same, and newer, versions aren't offered it.
        assert_eq!(super::offer(&release, "0.3.0", "x86_64"), None);
        assert_eq!(super::offer(&release, "0.4.0", "x86_64"), None);
        // The fixture's arm64 zip has no signature: a release signed only in part.
        assert_eq!(super::offer(&release, "0.2.0", "aarch64"), None);
    }

    #[test]
    fn swaps_in_over_an_open_exe() {
        let dir = tempfile::tempdir().unwrap();
        let (installed, new, old) = (
            dir.path().join("b33p.exe"),
            dir.path().join("b33p.exe.new"),
            dir.path().join("b33p.exe.old"),
        );
        std::fs::write(&installed, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        // Held open, as a running exe is.
        let held = std::fs::File::open(&installed).unwrap();
        swap_in(&installed, &new, &old).unwrap();
        drop(held);
        assert_eq!(std::fs::read(&installed).unwrap(), b"new");
        assert_eq!(std::fs::read(&old).unwrap(), b"old");
        assert!(!new.exists());
        // Nothing to swap in: the installed one stays.
        assert!(swap_in(&installed, &new, &old).is_err());
        assert_eq!(std::fs::read(&installed).unwrap(), b"new");
    }

    #[test]
    fn takes_the_exe_out_of_the_zip() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("b33p.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("b33p/LICENSE", options).unwrap();
        zip.write_all(b"AGPL").unwrap();
        zip.start_file("b33p/b33p.exe", options).unwrap();
        zip.write_all(b"MZ the program").unwrap();
        zip.finish().unwrap();
        let exe = dir.path().join("b33p.exe.new");
        extract_exe(&zip_path, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZ the program");

        let empty_path = dir.path().join("empty.zip");
        zip::ZipWriter::new(std::fs::File::create(&empty_path).unwrap())
            .finish()
            .unwrap();
        assert!(matches!(
            extract_exe(&empty_path, &exe),
            Err(UpdateError::NoExe)
        ));
    }
}
