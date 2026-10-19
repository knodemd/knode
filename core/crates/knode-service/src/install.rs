//! Start Knode Service at login: a launchd agent on macOS, a systemd user unit on Linux.
//!
//! [`LoginService`] writes the file and loads it (`launchctl bootstrap`, `systemctl --user
//! enable --now`), or unloads and removes it. The folder and the program it runs are given
//! when it is made, so tests never touch the real ones.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::user_home;
use crate::{Error, Result};

/// The launchd label of Knode Service.
pub const LAUNCH_AGENT_LABEL: &str = "md.knode.service";
/// The systemd user unit of Knode Service.
pub const SYSTEMD_UNIT: &str = "knode.service";

/// The launch agent property list that runs `<exe> service run` at login, and again if it
/// crashes. A service stopped on purpose (it exits cleanly on SIGTERM) stays stopped until the
/// next login. Its messages go to `log` when given.
pub fn launch_agent_plist(exe: &Path, log: Option<&Path>) -> String {
    let exe = xml_escape(&exe.to_string_lossy());
    let log = match log {
        Some(log) => {
            let log = xml_escape(&log.to_string_lossy());
            format!(
                "\t<key>StandardOutPath</key>\n\t<string>{log}</string>\n\
                 \t<key>StandardErrorPath</key>\n\t<string>{log}</string>\n"
            )
        }
        None => String::new(),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCH_AGENT_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>service</string>
		<string>run</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Background</string>
{log}</dict>
</plist>
"#
    )
}

/// Where the launch agent lives: `~/Library/LaunchAgents/md.knode.service.plist`.
pub fn launch_agent_path() -> Result<PathBuf> {
    Ok(launch_agents_dir()?.join(format!("{LAUNCH_AGENT_LABEL}.plist")))
}

fn launch_agents_dir() -> Result<PathBuf> {
    Ok(user_home()?.join("Library/LaunchAgents"))
}

/// The systemd user unit that runs `<exe> service run`, and again if it fails. A service
/// stopped on purpose stays stopped.
pub fn systemd_unit(exe: &Path) -> String {
    let exe = systemd_quote(&exe.to_string_lossy());
    format!(
        "[Unit]
Description=Knode Service
Documentation=https://knode.md

[Service]
Type=simple
ExecStart={exe} service run
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
"
    )
}

/// Where the systemd user unit lives: `$XDG_CONFIG_HOME/systemd/user/knode.service`, by
/// default `~/.config/systemd/user/knode.service`.
pub fn systemd_unit_path() -> Result<PathBuf> {
    Ok(systemd_user_dir()?.join(SYSTEMD_UNIT))
}

fn systemd_user_dir() -> Result<PathBuf> {
    let config = match env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(config) => config,
        None => user_home()?.join(".config"),
    };
    Ok(config.join("systemd/user"))
}

/// Which service manager starts Knode Service at login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginKind {
    /// A launchd agent, on macOS.
    LaunchAgent,
    /// A systemd user unit, on Linux.
    SystemdUnit,
}

/// Starting Knode Service at login: the file that does it, and the program that loads it.
///
/// ```no_run
/// let login = knode_service::LoginService::for_this_system()?;
/// login.install("/opt/homebrew/bin/knode", None)?;
/// assert!(login.is_installed());
/// login.uninstall()?;
/// # Ok::<(), knode_service::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginService {
    kind: LoginKind,
    dir: PathBuf,
    program: PathBuf,
}

impl LoginService {
    /// The launch agent in `~/Library/LaunchAgents` loaded with `launchctl` on macOS; the
    /// systemd user unit loaded with `systemctl` elsewhere.
    pub fn for_this_system() -> Result<Self> {
        if cfg!(target_os = "macos") {
            Ok(Self::launch_agent(launch_agents_dir()?, "launchctl"))
        } else {
            Ok(Self::systemd_unit(systemd_user_dir()?, "systemctl"))
        }
    }

    /// A launch agent in the folder `dir`, loaded with the `launchctl` program at `launchctl`.
    pub fn launch_agent(dir: impl Into<PathBuf>, launchctl: impl Into<PathBuf>) -> Self {
        LoginService {
            kind: LoginKind::LaunchAgent,
            dir: dir.into(),
            program: launchctl.into(),
        }
    }

    /// A systemd user unit in the folder `dir`, loaded with the `systemctl` program at
    /// `systemctl`.
    pub fn systemd_unit(dir: impl Into<PathBuf>, systemctl: impl Into<PathBuf>) -> Self {
        LoginService {
            kind: LoginKind::SystemdUnit,
            dir: dir.into(),
            program: systemctl.into(),
        }
    }

    /// Which service manager this is.
    pub fn kind(&self) -> LoginKind {
        self.kind
    }

    /// The launch agent or unit file.
    pub fn path(&self) -> PathBuf {
        match self.kind {
            LoginKind::LaunchAgent => self.dir.join(format!("{LAUNCH_AGENT_LABEL}.plist")),
            LoginKind::SystemdUnit => self.dir.join(SYSTEMD_UNIT),
        }
    }

    /// Whether the file is there.
    pub fn is_installed(&self) -> bool {
        self.path().is_file()
    }

    /// Writes the file for the `knode` executable at `exe` and loads it, which starts the
    /// service now and at every login. A file already there is unloaded and replaced. On macOS
    /// the service's messages go to `log` when given. Returns the file.
    pub fn install(&self, exe: impl AsRef<Path>, log: Option<&Path>) -> Result<PathBuf> {
        let exe = absolute_exe(exe.as_ref())?;
        let path = self.path();
        match self.kind {
            LoginKind::LaunchAgent => {
                if path.exists() {
                    // Loading a label that is already loaded fails: unload it first.
                    let _ = self.run(&["bootout".as_ref(), self.target().as_ref()]);
                }
                write_file(&path, &launch_agent_plist(&exe, log))?;
                self.run(&[
                    "bootstrap".as_ref(),
                    self.domain().as_ref(),
                    path.as_os_str(),
                ])?;
            }
            LoginKind::SystemdUnit => {
                write_file(&path, &systemd_unit(&exe))?;
                self.run(&["--user".as_ref(), "daemon-reload".as_ref()])?;
                self.run(&[
                    "--user".as_ref(),
                    "enable".as_ref(),
                    "--now".as_ref(),
                    SYSTEMD_UNIT.as_ref(),
                ])?;
            }
        }
        Ok(path)
    }

    /// Unloads the file, which stops a service it started, and removes it. Returns whether
    /// there was one.
    pub fn uninstall(&self) -> Result<bool> {
        let path = self.path();
        if !path.exists() {
            return Ok(false);
        }
        // Unloading fails when it is not loaded, which is fine.
        match self.kind {
            LoginKind::LaunchAgent => {
                let _ = self.run(&["bootout".as_ref(), self.target().as_ref()]);
            }
            LoginKind::SystemdUnit => {
                let _ = self.run(&[
                    "--user".as_ref(),
                    "disable".as_ref(),
                    "--now".as_ref(),
                    SYSTEMD_UNIT.as_ref(),
                ]);
            }
        }
        let removed = remove_file(&path)?;
        if self.kind == LoginKind::SystemdUnit {
            let _ = self.run(&["--user".as_ref(), "daemon-reload".as_ref()]);
        }
        Ok(removed)
    }

    /// `gui/<uid>`: the launchd domain of the user's login session.
    fn domain(&self) -> String {
        // SAFETY: getuid has no preconditions and cannot fail.
        format!("gui/{}", unsafe { libc::getuid() })
    }

    /// `gui/<uid>/md.knode.service`.
    fn target(&self) -> String {
        format!("{}/{LAUNCH_AGENT_LABEL}", self.domain())
    }

    fn run(&self, args: &[&OsStr]) -> Result<()> {
        let shown = format!(
            "{} {}",
            self.program.display(),
            args.iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        );
        let output = Command::new(&self.program)
            .args(args)
            .output()
            .map_err(|e| Error::Command {
                command: shown.clone(),
                message: e.to_string(),
            })?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(Error::Command {
            command: shown,
            message: if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            },
        })
    }
}

fn absolute_exe(exe: &Path) -> Result<PathBuf> {
    if exe.as_os_str().is_empty() {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the knode executable path is empty",
        )));
    }
    Ok(std::path::absolute(exe)?)
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

fn remove_file(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Quotes one word of a systemd command line.
fn systemd_quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A fake `launchctl` or `systemctl` that logs its arguments and fails with `code` when
    /// its first argument (after `--user`) is `fail_on`.
    fn fake_program(dir: &Path, fail_on: &str, code: i32) -> PathBuf {
        let path = dir.join("tool");
        let log = dir.join("tool.log");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1\" = '{fail_on}' ] || [ \"$2\" = '{fail_on}' ]; then echo 'no such service' >&2; exit {code}; fi\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn log(dir: &Path) -> Vec<String> {
        fs::read_to_string(dir.join("tool.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn launch_agent_is_written_and_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("LaunchAgents");
        let tool = fake_program(dir.path(), "bootout", 3);
        let login = LoginService::launch_agent(&agents, &tool);
        assert!(!login.is_installed());
        let log_file = dir.path().join("service.log");
        let path = login
            .install("/opt/Knode & Co/bin/knode", Some(&log_file))
            .unwrap();
        assert_eq!(path, agents.join("md.knode.service.plist"));
        assert!(login.is_installed());
        let plist = fs::read_to_string(&path).unwrap();
        assert!(plist.contains("<string>md.knode.service</string>"));
        assert!(plist.contains("<string>/opt/Knode &amp; Co/bin/knode</string>"));
        assert!(plist.contains("<string>service</string>\n\t\t<string>run</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(plist.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(plist.contains(&format!(
            "<key>StandardErrorPath</key>\n\t<string>{}</string>",
            log_file.display()
        )));
        let uid = unsafe { libc::getuid() };
        assert_eq!(
            log(dir.path()),
            vec![format!("bootstrap gui/{uid} {}", path.display())]
        );

        // Installing again unloads the old one first, even if unloading fails.
        login.install("/usr/local/bin/knode", None).unwrap();
        let calls = log(dir.path());
        assert_eq!(calls[1], format!("bootout gui/{uid}/md.knode.service"));
        assert!(calls[2].starts_with("bootstrap "));
        assert!(
            !fs::read_to_string(&path)
                .unwrap()
                .contains("StandardErrorPath")
        );

        assert!(login.uninstall().unwrap());
        assert!(!path.exists());
        assert_eq!(
            log(dir.path())[3],
            format!("bootout gui/{uid}/md.knode.service")
        );
        assert!(!login.uninstall().unwrap());
        assert_eq!(log(dir.path()).len(), 4);
    }

    #[test]
    fn a_failed_load_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let tool = fake_program(dir.path(), "bootstrap", 5);
        let login = LoginService::launch_agent(dir.path().join("agents"), &tool);
        let error = login.install("/bin/knode", None).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("bootstrap"), "{text}");
        assert!(text.contains("no such service"), "{text}");

        let missing =
            LoginService::launch_agent(dir.path().join("agents"), dir.path().join("none"));
        assert!(matches!(
            missing.install("/bin/knode", None),
            Err(Error::Command { .. })
        ));
    }

    #[test]
    fn systemd_unit_is_written_and_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let units = dir.path().join("systemd/user");
        let tool = fake_program(dir.path(), "disable", 1);
        let login = LoginService::systemd_unit(&units, &tool);
        let path = login
            .install("/home/me/100% \"knode\"/knode", None)
            .unwrap();
        assert_eq!(path, units.join("knode.service"));
        let unit = fs::read_to_string(&path).unwrap();
        assert!(unit.contains("ExecStart=\"/home/me/100%% \\\"knode\\\"/knode\" service run\n"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(login.uninstall().unwrap());
        assert!(!login.uninstall().unwrap());
        assert_eq!(
            log(dir.path()),
            vec![
                "--user daemon-reload",
                "--user enable --now knode.service",
                "--user disable --now knode.service",
                "--user daemon-reload",
            ]
        );
    }

    #[test]
    fn relative_executables_become_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let tool = fake_program(dir.path(), "none", 0);
        let login = LoginService::launch_agent(dir.path(), &tool);
        let path = login.install("knode", None).unwrap();
        let plist = fs::read_to_string(path).unwrap();
        let exe = std::env::current_dir().unwrap().join("knode");
        assert!(plist.contains(&format!("<string>{}</string>", exe.display())));
    }
}
