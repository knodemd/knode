//! `manifest.json`: what a reader checks before opening `knode.db`.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The format version this library reads and writes: the `format` field of `manifest.json`.
pub const FORMAT_VERSION: u64 = 0;

/// The contents of `manifest.json`.
///
/// ```json
/// {
///   "format": 0,
///   "id": "01K83N6V4W5D2XQJ0Y7RZ9T1AB",
///   "name": "Example",
///   "created_at": "2026-10-20T09:30:00.000Z",
///   "generator": "knode-format 0.1.0"
/// }
/// ```
///
/// Fields this library does not know are kept when the manifest is written back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The format version, `0` for this specification.
    pub format: u64,
    /// The id of the space, a ULID. It never changes, even when the package is renamed.
    pub id: String,
    /// The name of the space, shown to people.
    pub name: String,
    /// When the space was created, in RFC 3339 and UTC.
    pub created_at: String,
    /// The program that last wrote the manifest, such as `knode-format 0.1.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    /// Fields added by newer writers, kept as they are.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Manifest {
    /// Reads `manifest.json` from a package.
    pub fn read(package: &Path) -> Result<Self> {
        let path = package.join(crate::MANIFEST_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotASpace(package.to_path_buf()));
            }
            Err(e) => return Err(e.into()),
        };
        // Check the version before the rest, so a newer manifest is reported as such.
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| Error::Manifest(e.to_string()))?;
        match value.get("format").and_then(serde_json::Value::as_u64) {
            Some(FORMAT_VERSION) => {}
            Some(found) => {
                return Err(Error::UnsupportedFormat {
                    found,
                    supported: FORMAT_VERSION,
                });
            }
            None => return Err(Error::Manifest("\"format\" must be a number".into())),
        }
        serde_json::from_value(value).map_err(|e| Error::Manifest(e.to_string()))
    }

    /// Writes `manifest.json` into a package, replacing it atomically.
    pub fn write(&self, package: &Path) -> Result<()> {
        let mut text =
            serde_json::to_string_pretty(self).map_err(|e| Error::Manifest(e.to_string()))?;
        text.push('\n');
        let tmp = package.join(".manifest.json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, package.join(crate::MANIFEST_FILE))?;
        Ok(())
    }
}
