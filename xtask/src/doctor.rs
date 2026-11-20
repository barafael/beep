//! `cargo testbox doctor` and `clean`: what boxes there are on this machine, by instance, and
//! which of them are strays (a network, volumes or image tags of a box whose pod is gone, as
//! after a run that was killed), or idle (a pod left up, nobody holding it).
//!
//! Boxes of every worktree share Podman's storage, so this reads Podman, not a worktree's
//! `target`. It only reports, unless `clean` is told to remove (`--yes`).

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use crate::Failure;
use crate::pod::{self, POD, Pod, podman};
use crate::process;

/// What Podman holds of one box, by its pod's name.
#[derive(Default)]
struct Found {
    /// The pod's status (`Running`, `Exited`, `Degraded`, …), if it exists.
    pod: Option<String>,
    containers: usize,
    network: bool,
    volumes: usize,
    images: usize,
}

/// The instance a name belongs to: `Some("")` for the default box, `None` if it isn't a box's.
fn instance(name: &str) -> Option<&str> {
    if name == POD {
        Some("")
    } else {
        name.strip_prefix(POD)?.strip_prefix('.')
    }
}

/// A box's name by its instance.
fn named(instance: &str) -> String {
    if instance.is_empty() {
        POD.to_owned()
    } else {
        format!("{POD}.{instance}")
    }
}

fn lines(args: &[&str]) -> Result<Vec<String>, Failure> {
    Ok(process::output(podman(args), "")?
        .lines()
        .map(str::to_owned)
        .collect())
}

/// Every box Podman knows, by instance.
fn boxes() -> Result<BTreeMap<String, Found>, Failure> {
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    for row in lines(&[
        "pod",
        "ls",
        "--format",
        "{{.Name}}\t{{.Status}}\t{{.NumberOfContainers}}",
    ])? {
        let mut cells = row.split('\t');
        let (Some(name), Some(status), Some(count)) = (cells.next(), cells.next(), cells.next())
        else {
            continue;
        };
        if let Some(instance) = instance(name) {
            let entry = found.entry(instance.to_owned()).or_default();
            entry.pod = Some(status.to_owned());
            entry.containers = count.parse().unwrap_or(0);
        }
    }
    for name in lines(&["network", "ls", "--format", "{{.Name}}"])? {
        if let Some(instance) = instance(&name) {
            found.entry(instance.to_owned()).or_default().network = true;
        }
    }
    // Volumes by the longest box that is a prefix, `<box>-<claim>`: the default box's name is
    // the start of every instance's, and an instance's of its `-j1` boxes'.
    let known: Vec<String> = found.keys().map(|instance| named(instance)).collect();
    for name in lines(&["volume", "ls", "--format", "{{.Name}}"])? {
        if !name.starts_with(POD) {
            continue;
        }
        let owner = known
            .iter()
            .filter(|box_name| {
                name.strip_prefix(box_name.as_str())
                    .is_some_and(|rest| rest.starts_with('-'))
            })
            .max_by_key(|box_name| box_name.len());
        if let Some(instance) = owner.and_then(|owner| instance(owner)) {
            found.entry(instance.to_owned()).or_default().volumes += 1;
        }
    }
    // Images by the tag's end: `<box>-<image>`, the image's name one of a known few, as an
    // instance's own may hold dashes.
    for repository in lines(&["images", "--format", "{{.Repository}}"])? {
        let Some(tag) = repository.strip_prefix("localhost/") else {
            continue;
        };
        let Some(box_name) = pod::image_names()
            .into_iter()
            .find_map(|image| tag.strip_suffix(&format!("-{image}")))
        else {
            continue;
        };
        if let Some(instance) = instance(box_name) {
            found.entry(instance.to_owned()).or_default().images += 1;
        }
    }
    Ok(found)
}

/// Whether a command holds the box named `name` at this moment: its lock is taken.
fn held(name: &str) -> Option<bool> {
    let lock = File::open(Pod::lock_path(name)).ok()?;
    match lock.try_lock() {
        Ok(()) => Some(false),
        Err(std::fs::TryLockError::WouldBlock) => Some(true),
        Err(std::fs::TryLockError::Error(_)) => None,
    }
}

/// What is wrong with a box, if anything.
fn note(found: &Found, held: Option<bool>) -> &'static str {
    match (&found.pod, held) {
        (Some(status), Some(true)) if status != "Running" => "held, its pod not running",
        (Some(_), Some(true)) => "in use",
        (Some(status), _) if status == "Running" => "idle: a pod left up, nobody holds it",
        (Some(_), _) => "pod stopped or degraded, nobody holds it",
        (None, Some(true)) => "held, no pod yet",
        (None, _) if found.network || found.volumes > 0 => "STRAY: no pod; network or volumes kept",
        (None, _) => "images only",
    }
}

/// Prints every box, one line each, and the strays among them.
pub(crate) fn doctor() -> Result<(), Failure> {
    let found = boxes()?;
    println!("INSTANCE                 POD        CTRS NET VOLS IMAGES  NOTE");
    let mut strays = 0;
    let mut idle = 0;
    for (instance, found) in &found {
        let held = held(&named(instance));
        let note = note(found, held);
        strays += usize::from(note.starts_with("STRAY"));
        idle += usize::from(note.starts_with("idle"));
        println!(
            "{:<24} {:<10} {:>4} {:>3} {:>4} {:>6}  {note}",
            if instance.is_empty() {
                "(default)"
            } else {
                instance
            },
            found.pod.as_deref().unwrap_or("-"),
            found.containers,
            if found.network { "yes" } else { "-" },
            found.volumes,
            found.images,
        );
    }
    let networks = found.values().filter(|found| found.network).count();
    println!(
        "\n{} boxes, {networks} networks (netavark has 256 subnets), {strays} strays, {idle} idle",
        found.len()
    );
    if strays + idle > 0 {
        println!(
            "`cargo testbox --instance <name> clean --yes` takes one box away; `cargo testbox \
             clean --strays --yes` every stray (it asks nothing of boxes in use)"
        );
    }
    Ok(())
}

/// Takes `instance`'s box away: its pod, volumes, network and image tags, and this worktree's
/// scratch directories for it (not its record); or says what it would, without `yes`.
fn clean_box(repo: &Path, instance: &str, found: &Found, yes: bool) -> Result<(), Failure> {
    let pod = Pod::new(repo, (!instance.is_empty()).then_some(instance))?;
    let name = pod.pod_name().to_owned();
    let tags: Vec<String> = lines(&["images", "--format", "{{.Repository}}"])?
        .into_iter()
        .filter(|repository| {
            pod::image_names()
                .iter()
                .any(|image| *repository == format!("localhost/{name}-{image}"))
        })
        .collect();
    let volumes = pod.volumes_present()?;
    println!(
        "{name}: pod {}, {} volumes, network {}, {} image tags",
        found.pod.as_deref().unwrap_or("-"),
        volumes.len(),
        if found.network { "yes" } else { "-" },
        tags.len()
    );
    if !yes {
        return Ok(());
    }
    pod.holding(|| -> Result<(), Failure> {
        pod.down(true)?;
        for tag in &tags {
            // Untagging: the layers stay for the next build's cache.
            let status = process::output(podman(&["rmi", "--force", tag]), "");
            if let Err(error) = status {
                println!("  {tag}: {error}");
            }
        }
        for dir in pod.scratch_dirs() {
            if dir.is_dir() {
                std::fs::remove_dir_all(&dir)?;
            } else if dir.is_file() {
                std::fs::remove_file(&dir)?;
            }
        }
        Ok(())
    })?;
    println!("  removed");
    Ok(())
}

/// `clean`: the box `instance` names, or with `strays` every box with no pod and nobody
/// holding it; nothing is removed without `yes`.
pub(crate) fn clean(
    repo: &Path,
    instance: Option<&str>,
    strays: bool,
    yes: bool,
) -> Result<(), Failure> {
    let found = boxes()?;
    let chosen: Vec<&String> = if strays {
        found
            .iter()
            .filter(|(instance, found)| {
                found.pod.is_none()
                    && held(&named(instance)) != Some(true)
                    && (found.network || found.volumes > 0)
            })
            .map(|(instance, _)| instance)
            .collect()
    } else {
        let wanted = instance.unwrap_or("").to_owned();
        found.keys().filter(|key| **key == wanted).collect()
    };
    if chosen.is_empty() {
        println!("nothing to clean");
        return Ok(());
    }
    for instance in chosen {
        if let Some(found) = found.get(instance) {
            clean_box(repo, instance, found, yes)?;
        }
    }
    if !yes {
        println!("this was a look; --yes removes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pod_name_is_a_boxes_instance() {
        assert_eq!(instance("b33p-testbox"), Some(""));
        assert_eq!(instance("b33p-testbox.sc1"), Some("sc1"));
        assert_eq!(instance("b33p-testbox-foo"), None);
        assert_eq!(instance("mic-testbox-bob"), None);
        assert_eq!(named("sc1"), "b33p-testbox.sc1");
        assert_eq!(named(""), "b33p-testbox");
    }

    #[test]
    fn a_box_with_no_pod_and_a_network_is_a_stray() {
        let leftovers = Found {
            network: true,
            ..Found::default()
        };
        assert!(note(&leftovers, Some(false)).starts_with("STRAY"));
        assert_eq!(note(&leftovers, Some(true)), "held, no pod yet");
        let up = Found {
            pod: Some("Running".to_owned()),
            ..Found::default()
        };
        assert!(note(&up, Some(false)).starts_with("idle"));
        assert_eq!(note(&up, Some(true)), "in use");
    }
}
