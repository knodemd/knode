//! Where Knode keeps its settings and spaces, and which space to use.
//!
//! - **Knode home** holds `config.toml` and, later, the Knode Service socket. It is the
//!   `KNODE_HOME` environment variable if set, else `~/Library/Application Support/Knode` on
//!   macOS, else `$XDG_DATA_HOME/knode` (by default `~/.local/share/knode`).
//! - **`config.toml`** names the default space: `default_space = "/path/to/Work.knode"`.
//! - **Spaces** are created by default in `~/Knode`, as `~/Knode/<name>.knode`.
//!
//! The space a command uses is, in order: the `--space <path>` flag, the `KNODE_SPACE`
//! environment variable, then `default_space` in `config.toml`.
//!
//! ```
//! use knode_config::{Config, Paths, SpaceSource};
//!
//! # let dir = tempfile::tempdir().unwrap();
//! let paths = Paths::from_vars(|name| match name {
//!     "KNODE_HOME" => Some(dir.path().join("home").into()),
//!     "HOME" => Some(dir.path().into()),
//!     _ => None,
//! })?;
//! let mut config = Config::load(&paths)?;
//! config.default_space = Some(paths.space_path("Work"));
//! config.save(&paths)?;
//!
//! let space = knode_config::resolve_space(None, None, &Config::load(&paths)?)?;
//! assert_eq!(space.path, dir.path().join("Knode/Work.knode"));
//! assert_eq!(space.source, SpaceSource::Config);
//! # Ok::<(), knode_config::Error>(())
//! ```

#![warn(missing_docs)]

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The environment variable that overrides the Knode home folder.
pub const KNODE_HOME_VAR: &str = "KNODE_HOME";
/// The environment variable that picks the space to use.
pub const KNODE_SPACE_VAR: &str = "KNODE_SPACE";
/// The settings file inside the Knode home folder.
pub const CONFIG_FILE: &str = "config.toml";
/// The Knode Service socket inside the Knode home folder.
pub const SERVICE_SOCKET: &str = "service.sock";
/// The folder in the user's home where spaces are created by default.
pub const SPACES_DIR: &str = "Knode";

/// Everything that can go wrong finding or reading the settings.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Neither `HOME` nor `KNODE_HOME` is set, so there is nowhere to keep settings.
    #[error("cannot find your home folder: set HOME or KNODE_HOME")]
    NoHome,
    /// `config.toml` could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
    /// `config.toml` is not valid TOML or has a value of the wrong type.
    #[error("{path} is not valid: {message}")]
    Invalid {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        message: String,
    },
    /// No space was given and there is no default space.
    #[error("no space to use: create one with `knode init <name>`, or pass --space <path>")]
    NoSpace,
}

/// A `Result` whose error is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The folders Knode uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The Knode home folder, with `config.toml`.
    pub home: PathBuf,
    /// The folder where new spaces are created by default, `~/Knode`.
    pub spaces_dir: PathBuf,
}

impl Paths {
    /// The folders, from the environment of this process.
    pub fn from_env() -> Result<Paths> {
        Self::from_vars(|name| std::env::var_os(name))
    }

    /// The folders, reading environment variables through `var`. Useful for tests.
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Result<Paths> {
        let get = |name: &str| {
            var(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        let user_home = get("HOME").or_else(|| get("USERPROFILE"));
        let home = match get(KNODE_HOME_VAR) {
            Some(home) => home,
            None => {
                let user_home = user_home.as_ref().ok_or(Error::NoHome)?;
                if cfg!(target_os = "macos") {
                    user_home.join("Library/Application Support/Knode")
                } else {
                    get("XDG_DATA_HOME")
                        .filter(|dir| dir.is_absolute())
                        .unwrap_or_else(|| user_home.join(".local/share"))
                        .join("knode")
                }
            }
        };
        let spaces_dir = match user_home {
            Some(user_home) => user_home.join(SPACES_DIR),
            None => home.join("spaces"),
        };
        Ok(Paths { home, spaces_dir })
    }

    /// `config.toml` in the Knode home folder.
    pub fn config_file(&self) -> PathBuf {
        self.home.join(CONFIG_FILE)
    }

    /// The Knode Service socket in the Knode home folder.
    pub fn service_socket(&self) -> PathBuf {
        self.home.join(SERVICE_SOCKET)
    }

    /// Where a space with this name is created by default: `~/Knode/<name>.knode`.
    pub fn space_path(&self, name: &str) -> PathBuf {
        self.spaces_dir.join(format!("{name}.knode"))
    }

    /// The `.knode` packages in the default spaces folder, sorted by path. A missing folder
    /// has none.
    pub fn spaces_in_dir(&self) -> Result<Vec<PathBuf>> {
        let entries = match fs::read_dir(&self.spaces_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(Error::Io {
                    path: self.spaces_dir.clone(),
                    source,
                });
            }
        };
        let mut spaces: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir() && path.extension().is_some_and(|e| e == "knode"))
            .collect();
        spaces.sort();
        Ok(spaces)
    }
}

/// The contents of `config.toml`. Keys this version does not know are kept when it is saved.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The space commands and agents use when none is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_space: Option<PathBuf>,
    /// Keys written by newer versions, kept as they are.
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl Config {
    /// Reads `config.toml`. A missing file is an empty config.
    pub fn load(paths: &Paths) -> Result<Config> {
        Self::load_from(&paths.config_file())
    }

    /// Reads a config file. A missing file is an empty config.
    pub fn load_from(path: &Path) -> Result<Config> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(source) => {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|e| Error::Invalid {
            path: path.to_path_buf(),
            message: e.message().to_string(),
        })
    }

    /// Writes `config.toml`, creating the Knode home folder if needed. The file is replaced
    /// atomically, so a reader never sees half of it.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        self.save_to(&paths.config_file())
    }

    /// Writes a config file atomically, creating its folder if needed.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let io_error = |source| Error::Io {
            path: path.to_path_buf(),
            source,
        };
        let text = toml::to_string(self).map_err(|e| Error::Invalid {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).map_err(io_error)?;
        fs::rename(&tmp, path).map_err(io_error)
    }
}

/// Where the space to use came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceSource {
    /// The `--space` flag.
    Flag,
    /// The `KNODE_SPACE` environment variable.
    Env,
    /// `default_space` in `config.toml`.
    Config,
}

impl fmt::Display for SpaceSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SpaceSource::Flag => "--space",
            SpaceSource::Env => KNODE_SPACE_VAR,
            SpaceSource::Config => "default_space in config.toml",
        })
    }
}

/// The space to use and where that choice came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpace {
    /// The `.knode` package.
    pub path: PathBuf,
    /// Where the path came from.
    pub source: SpaceSource,
}

/// Picks the space to use: `flag` (from `--space`), else `env` (the value of `KNODE_SPACE`),
/// else the default space in `config`. Empty values are skipped. Fails with
/// [`Error::NoSpace`] when there is none.
pub fn resolve_space(
    flag: Option<&Path>,
    env: Option<&Path>,
    config: &Config,
) -> Result<ResolvedSpace> {
    fn non_empty(path: Option<&Path>) -> Option<&Path> {
        path.filter(|p| !p.as_os_str().is_empty())
    }
    let (path, source) = if let Some(path) = non_empty(flag) {
        (path.to_path_buf(), SpaceSource::Flag)
    } else if let Some(path) = non_empty(env) {
        (path.to_path_buf(), SpaceSource::Env)
    } else if let Some(path) = non_empty(config.default_space.as_deref()) {
        (path.to_path_buf(), SpaceSource::Config)
    } else {
        return Err(Error::NoSpace);
    };
    Ok(ResolvedSpace { path, source })
}

/// [`resolve_space`] with `KNODE_SPACE` read from the environment of this process.
pub fn resolve_space_from_env(flag: Option<&Path>, config: &Config) -> Result<ResolvedSpace> {
    let env = std::env::var_os(KNODE_SPACE_VAR).map(PathBuf::from);
    resolve_space(flag, env.as_deref(), config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn knode_home_wins() {
        let paths = Paths::from_vars(vars(&[("KNODE_HOME", "/k"), ("HOME", "/u")])).unwrap();
        assert_eq!(paths.home, PathBuf::from("/k"));
        assert_eq!(paths.config_file(), PathBuf::from("/k/config.toml"));
        assert_eq!(paths.service_socket(), PathBuf::from("/k/service.sock"));
        assert_eq!(paths.spaces_dir, PathBuf::from("/u/Knode"));
        assert_eq!(
            paths.space_path("Work"),
            PathBuf::from("/u/Knode/Work.knode")
        );
    }

    #[test]
    fn default_home_depends_on_the_system() {
        let paths = Paths::from_vars(vars(&[("HOME", "/u"), ("XDG_DATA_HOME", "/x")])).unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(
                paths.home,
                PathBuf::from("/u/Library/Application Support/Knode")
            );
        } else {
            assert_eq!(paths.home, PathBuf::from("/x/knode"));
            let paths = Paths::from_vars(vars(&[("HOME", "/u")])).unwrap();
            assert_eq!(paths.home, PathBuf::from("/u/.local/share/knode"));
        }
    }

    #[test]
    fn no_home_is_an_error() {
        assert!(matches!(Paths::from_vars(vars(&[])), Err(Error::NoHome)));
        let paths = Paths::from_vars(vars(&[("KNODE_HOME", "/k")])).unwrap();
        assert_eq!(paths.spaces_dir, PathBuf::from("/k/spaces"));
    }

    #[test]
    fn space_resolution_order() {
        let config = Config {
            default_space: Some("/c.knode".into()),
            ..Default::default()
        };
        let flag = Path::new("/f.knode");
        let env = Path::new("/e.knode");
        let got = resolve_space(Some(flag), Some(env), &config).unwrap();
        assert_eq!((got.path.as_path(), got.source), (flag, SpaceSource::Flag));
        let got = resolve_space(None, Some(env), &config).unwrap();
        assert_eq!((got.path.as_path(), got.source), (env, SpaceSource::Env));
        let got = resolve_space(Some(Path::new("")), None, &config).unwrap();
        assert_eq!(got.source, SpaceSource::Config);
        assert!(matches!(
            resolve_space(None, None, &Config::default()),
            Err(Error::NoSpace)
        ));
    }

    #[test]
    fn config_round_trips_and_keeps_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("home/config.toml");
        assert_eq!(Config::load_from(&file).unwrap(), Config::default());

        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "default_space = \"/a.knode\"\nfuture = 1\n[later]\nx = \"y\"\n",
        )
        .unwrap();
        let mut config = Config::load_from(&file).unwrap();
        assert_eq!(config.default_space, Some(PathBuf::from("/a.knode")));
        config.default_space = Some("/b.knode".into());
        config.save_to(&file).unwrap();

        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("default_space = \"/b.knode\""), "{text}");
        assert!(text.contains("future = 1"), "{text}");
        assert!(text.contains("[later]"), "{text}");
    }

    #[test]
    fn invalid_config_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "default_space = 3\n").unwrap();
        let error = Config::load_from(&file).unwrap_err();
        assert!(matches!(error, Error::Invalid { .. }), "{error}");
    }

    #[test]
    fn lists_spaces_in_the_spaces_folder() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            home: dir.path().join("home"),
            spaces_dir: dir.path().join("Knode"),
        };
        assert!(paths.spaces_in_dir().unwrap().is_empty());
        fs::create_dir_all(paths.space_path("B")).unwrap();
        fs::create_dir_all(paths.space_path("A")).unwrap();
        fs::write(paths.spaces_dir.join("notes.md"), "").unwrap();
        assert_eq!(
            paths.spaces_in_dir().unwrap(),
            vec![paths.space_path("A"), paths.space_path("B")]
        );
    }
}
