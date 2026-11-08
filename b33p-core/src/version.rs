//! Comparing b33p's versions, for installing over an older copy and, later, updating.
//!
//! Versions are `MAJOR.MINOR.PATCH`, compared part by part as numbers: `0.10.0` is newer than
//! `0.9.3`. A pre-release (`0.3.0-rc.1`) counts as older than its release, and older than
//! nothing else of the same numbers, so a release always replaces its candidates.

use std::cmp::Ordering;

/// The version this build is.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A version, as far as ordering goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    numbers: Vec<u64>,
    /// Whether it has a pre-release suffix (`-rc.1`); build metadata (`+…`) counts for nothing.
    pre_release: bool,
}

impl Version {
    /// Reads `0.3.0`, `v0.3.0` or `0.3.0-rc.1`. `None` for anything that isn't numbers
    /// separated by dots: such a version can't be put in order, so nothing is replaced for it.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let text = text.split('+').next().unwrap_or(text);
        let (numbers, pre_release) = match text.split_once('-') {
            Some((numbers, _)) => (numbers, true),
            None => (text, false),
        };
        let numbers = numbers
            .split('.')
            .map(|part| part.parse().ok())
            .collect::<Option<Vec<u64>>>()?;
        Some(Self {
            numbers,
            pre_release,
        })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let len = self.numbers.len().max(other.numbers.len());
        let part = |v: &Self, i: usize| v.numbers.get(i).copied().unwrap_or(0);
        (0..len)
            .map(|i| part(self, i).cmp(&part(other, i)))
            .find(|order| order.is_ne())
            .unwrap_or_else(|| other.pre_release.cmp(&self.pre_release))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// How `other` compares to this build, `None` if it can't be read.
pub fn compare_to_current(other: &str) -> Option<Ordering> {
    Some(Version::parse(other)?.cmp(&Version::parse(CURRENT)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn compares_numerically() {
        assert!(v("0.10.0") > v("0.9.3"));
        assert!(v("0.1.1") > v("0.1.0"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("0.2.0"), v("v0.2.0"));
        assert_eq!(v("0.2").cmp(&v("0.2.0")), Ordering::Equal);
    }

    #[test]
    fn a_release_is_newer_than_its_candidates() {
        assert!(v("0.3.0") > v("0.3.0-rc.1"));
        assert!(v("0.3.0-rc.1") > v("0.2.9"));
        assert_eq!(v("0.3.0+build.5").cmp(&v("0.3.0")), Ordering::Equal);
    }

    #[test]
    fn rejects_what_it_cannot_order() {
        assert_eq!(Version::parse("garbage"), None);
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("0..1"), None);
        assert_eq!(compare_to_current("nightly"), None);
    }

    #[test]
    fn knows_its_own_version() {
        assert_eq!(compare_to_current(CURRENT), Some(Ordering::Equal));
    }
}
