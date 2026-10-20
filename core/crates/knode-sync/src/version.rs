//! [`Version`]: the version of a Loro document, as `loro_docs` keeps it.

use std::collections::BTreeMap;
use std::fmt;

use loro::VersionVector;

use crate::{Error, Result};

/// The version of a node's Loro document: for each peer, the number of its operations included.
///
/// It is written as the specification keeps it in `loro_docs` (21.3), a JSON object from each
/// peer id in decimal to a count, such as `{"7318201936571142":52}`, with
/// [`to_json`](Self::to_json) and [`from_json`](Self::from_json). An editor keeps the version it
/// last saw and passes it back with its next edit, so edits made meanwhile are merged, not undone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Version(BTreeMap<u64, u32>);

impl Version {
    /// The empty version, before any operation.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Reads a version as `loro_docs` keeps it.
    pub fn from_json(text: &str) -> Result<Self> {
        let invalid = || Error::Invalid(format!("not a Loro version: {text}"));
        let map: BTreeMap<String, u64> = serde_json::from_str(text).map_err(|_| invalid())?;
        let mut version = BTreeMap::new();
        for (peer, count) in map {
            if !peer.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            let peer: u64 = peer.parse().map_err(|_| invalid())?;
            let count = u32::try_from(count).map_err(|_| invalid())?;
            if count > 0 {
                version.insert(peer, count);
            }
        }
        Ok(Version(version))
    }

    /// Writes the version as `loro_docs` keeps it, peers in ascending order.
    pub fn to_json(&self) -> String {
        let map: BTreeMap<String, u32> = self.0.iter().map(|(p, c)| (p.to_string(), *c)).collect();
        serde_json::to_string(&map).expect("a map of strings to numbers is JSON")
    }

    /// The number of operations of `peer` included.
    pub fn count(&self, peer: u64) -> u32 {
        self.0.get(&peer).copied().unwrap_or(0)
    }

    /// Each peer with the number of its operations included.
    pub fn peers(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        self.0.iter().map(|(p, c)| (*p, *c))
    }

    /// Whether every operation `other` includes is included here.
    pub fn includes(&self, other: &Version) -> bool {
        other.0.iter().all(|(p, c)| self.count(*p) >= *c)
    }

    /// The peers with operations included here and not in `other`.
    pub fn peers_ahead_of(&self, other: &Version) -> Vec<u64> {
        self.0
            .iter()
            .filter(|(p, c)| **c > other.count(**p))
            .map(|(p, _)| *p)
            .collect()
    }

    pub(crate) fn from_loro(vv: &VersionVector) -> Self {
        Version(
            vv.iter()
                .filter(|(_, c)| **c > 0)
                .map(|(p, c)| (*p, *c as u32))
                .collect(),
        )
    }

    pub(crate) fn to_loro(&self) -> VersionVector {
        self.0.iter().map(|(p, c)| (*p, *c as i32)).collect()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_the_spec_form() {
        let v = Version::from_json(r#"{"7318201936571142": 52, "3": 0}"#).unwrap();
        assert_eq!(v.count(7318201936571142), 52);
        assert_eq!(v.to_json(), r#"{"7318201936571142":52}"#);
        assert_eq!(Version::from_json(&v.to_json()).unwrap(), v);
        assert_eq!(Version::from_json("{}").unwrap(), Version::empty());
        for bad in ["[]", r#"{"x":1}"#, r#"{"-1":1}"#, r#"{"1":-1}"#, "nope"] {
            assert!(Version::from_json(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn compares_versions() {
        let a = Version::from_json(r#"{"1":3,"2":1}"#).unwrap();
        let b = Version::from_json(r#"{"1":3}"#).unwrap();
        assert!(a.includes(&b));
        assert!(!b.includes(&a));
        assert_eq!(a.peers_ahead_of(&b), vec![2]);
        assert_eq!(Version::from_loro(&a.to_loro()), a);
    }
}
