//! The agents of this machine: `$KNODE_HOME/agents.json`.
//!
//! It holds, per agent, its id, name and kind, the fingerprints of its keys (never the keys)
//! and the spaces it was given a level in, so a revocation reaches every one of them. Only
//! Knode Service writes it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::KeyInfo;
use crate::keys::write_private;

/// The file in the Knode home with the agents of this machine.
pub const AGENTS_FILE: &str = "agents.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MachineAgent {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub created_at: i64,
    #[serde(default)]
    pub keys: Vec<KeyInfo>,
    #[serde(default)]
    pub spaces: Vec<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    agents: Vec<MachineAgent>,
}

#[derive(Debug)]
pub(crate) struct Registry {
    file: PathBuf,
    pub agents: Vec<MachineAgent>,
}

/// What a fingerprint is on this machine.
pub(crate) enum Lookup<'a> {
    Valid(&'a MachineAgent),
    Revoked(&'a MachineAgent),
    Unknown,
}

impl Registry {
    pub fn load(home: &Path) -> io::Result<Registry> {
        let file = home.join(AGENTS_FILE);
        let agents = match fs::read(&file) {
            Ok(bytes) => {
                serde_json::from_slice::<RegistryFile>(&bytes)
                    .map_err(|e| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("{} is not valid: {e}", file.display()),
                        )
                    })?
                    .agents
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Registry { file, agents })
    }

    pub fn save(&self) -> io::Result<()> {
        let file = RegistryFile {
            agents: self.agents.clone(),
        };
        let mut text = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
        text.push(b'\n');
        write_private(&self.file, &text)
    }

    pub fn lookup(&self, fingerprint: &str) -> Lookup<'_> {
        for agent in &self.agents {
            if let Some(key) = agent.keys.iter().find(|k| k.fingerprint == fingerprint) {
                return if key.revoked_at.is_none() {
                    Lookup::Valid(agent)
                } else {
                    Lookup::Revoked(agent)
                };
            }
        }
        Lookup::Unknown
    }

    pub fn by_name(&self, name: &str) -> Option<&MachineAgent> {
        self.agents.iter().find(|a| a.name == name)
    }

    pub fn by_name_mut(&mut self, name: &str) -> Option<&mut MachineAgent> {
        self.agents.iter_mut().find(|a| a.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_finds_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::load(dir.path()).unwrap();
        assert!(registry.agents.is_empty());
        registry.agents.push(MachineAgent {
            id: "01K83N6V4W5D2XQJ0Y7RZ9T1AB".into(),
            name: "claude-code".into(),
            kind: "claude-code".into(),
            created_at: 1,
            keys: vec![
                KeyInfo {
                    fingerprint: "sha256:old".into(),
                    created_at: 1,
                    revoked_at: Some(2),
                },
                KeyInfo {
                    fingerprint: "sha256:new".into(),
                    created_at: 2,
                    revoked_at: None,
                },
            ],
            spaces: vec![],
        });
        registry.save().unwrap();
        let registry = Registry::load(dir.path()).unwrap();
        assert!(matches!(registry.lookup("sha256:new"), Lookup::Valid(_)));
        assert!(matches!(registry.lookup("sha256:old"), Lookup::Revoked(_)));
        assert!(matches!(registry.lookup("sha256:none"), Lookup::Unknown));
        assert!(registry.by_name("claude-code").is_some());

        fs::write(dir.path().join(AGENTS_FILE), "{nope").unwrap();
        assert!(Registry::load(dir.path()).is_err());
    }
}
