//! Signal-Android's `gradle/verification-metadata.xml` with the patched libsignal's artifacts in:
//! they have no checksums there, so Gradle would refuse them. Their sha256 go in, as the image's
//! libsignal stage built them, so Gradle checks them by content like every other dependency.
//! POMs are left out: Signal-Android's own entries trust them by name.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::{Context, Error, Result};

/// The end of the metadata's components, before which the artifacts' go.
const END: &str = "</components>";

/// One of `org.signal`'s artifacts at the patched version: its name and its files' sha256.
pub struct Component {
    name: String,
    files: Vec<(String, String)>,
}

/// The components under `group` (a Maven repository's `org/signal`) at `version`, by name, their
/// files by name: the same order each time.
pub fn components(group: &Path, version: &str) -> Result<Vec<Component>> {
    let mut components = Vec::new();
    for name in names(group)? {
        let dir = group.join(&name).join(version);
        if !dir.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        for file in names(&dir)? {
            if file.ends_with(".pom") {
                continue;
            }
            let path = dir.join(&file);
            let bytes = std::fs::read(&path).context(path.display().to_string())?;
            files.push((file, hex::encode(Sha256::digest(&bytes))));
        }
        components.push(Component { name, files });
    }
    if components.is_empty() {
        return Err(Error::Failed(format!(
            "no artifact at {version} under {}",
            group.display()
        )));
    }
    Ok(components)
}

/// `metadata` with the `components` at `version` in, before its one `</components>`.
pub fn with_components(metadata: &str, version: &str, components: &[Component]) -> Result<String> {
    let ends = metadata.matches(END).count();
    if ends != 1 {
        return Err(Error::Failed(format!(
            "verification-metadata.xml has {ends} {END}, not 1"
        )));
    }
    let version = attribute(version);
    let entries: String = components
        .iter()
        .map(|component| {
            let artifacts: String = component
                .files
                .iter()
                .map(|(file, sha256)| {
                    format!(
                        "<artifact name=\"{}\"><sha256 value=\"{sha256}\" origin=\"the libsignal stage\"/></artifact>",
                        attribute(file)
                    )
                })
                .collect();
            format!(
                "<component group=\"org.signal\" name=\"{}\" version=\"{version}\">{artifacts}</component>",
                attribute(&component.name)
            )
        })
        .collect();
    Ok(metadata.replacen(END, &format!("{entries}{END}"), 1))
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Result<Vec<String>> {
    let mut names = std::fs::read_dir(dir)
        .context(dir.display().to_string())?
        .map(|entry| {
            entry
                .context(dir.display().to_string())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
        })
        .collect::<Result<Vec<String>>>()?;
    names.sort();
    Ok(names)
}

/// `value` as an XML attribute's value, between double quotes.
fn attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const METADATA: &str = "<verification-metadata><components><component group=\"x\"/></components></verification-metadata>";

    fn client() -> Component {
        Component {
            name: "libsignal-client".to_owned(),
            files: vec![(
                "libsignal-client-0.1-testbox.aar".to_owned(),
                "ab".repeat(32),
            )],
        }
    }

    #[test]
    fn the_components_go_before_the_end_of_the_others() {
        let patched = with_components(METADATA, "0.1-testbox", &[client()]).expect("one end");
        assert_eq!(
            patched,
            format!(
                "<verification-metadata><components><component group=\"x\"/>\
                 <component group=\"org.signal\" name=\"libsignal-client\" version=\"0.1-testbox\">\
                 <artifact name=\"libsignal-client-0.1-testbox.aar\"><sha256 value=\"{}\" \
                 origin=\"the libsignal stage\"/></artifact></component>\
                 </components></verification-metadata>",
                "ab".repeat(32)
            )
        );
    }

    #[test]
    fn metadata_without_one_end_fails() {
        assert!(with_components("<components/>", "0.1", &[client()]).is_err());
        let twice = format!("{METADATA}{METADATA}");
        assert!(with_components(&twice, "0.1", &[client()]).is_err());
    }

    #[test]
    fn values_are_escaped() {
        assert_eq!(attribute("a\"<&>"), "a&quot;&lt;&amp;&gt;");
    }

    /// Each component's files, sorted, the POMs left out; a name without the version skipped.
    #[test]
    fn components_are_read_in_order() {
        let group = std::env::temp_dir().join(format!("verification-{}", std::process::id()));
        let at = group.join("libsignal-client").join("0.1");
        std::fs::create_dir_all(&at).expect("a scratch repository");
        std::fs::create_dir_all(group.join("other").join("0.2")).expect("another version");
        for file in ["b.jar", "a.aar", "c.pom"] {
            std::fs::write(at.join(file), file).expect("an artifact");
        }
        let components = components(&group, "0.1").expect("the components");
        std::fs::remove_dir_all(&group).expect("the scratch repository removed");
        assert_eq!(components.len(), 1);
        let files: Vec<&str> = components[0]
            .files
            .iter()
            .map(|(file, _)| file.as_str())
            .collect();
        assert_eq!(files, ["a.aar", "b.jar"]);
        assert_eq!(
            components[0].files[0].1,
            hex::encode(Sha256::digest(b"a.aar"))
        );
    }
}
