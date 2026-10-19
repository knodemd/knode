//! Start Knode Service at login: a launchd agent on macOS, a systemd user unit on Linux.
//!
//! These functions only write and remove the files. Loading them (`launchctl bootstrap`,
//! `systemctl --user enable --now`) is left to the caller.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::user_home;
use crate::{Error, Result};

/// The launchd label of Knode Service.
pub const LAUNCH_AGENT_LABEL: &str = "md.knode.service";
/// The systemd user unit of Knode Service.
pub const SYSTEMD_UNIT: &str = "knode.service";

/// The launch agent property list that runs `<exe> service run` at login and keeps it running.
pub fn launch_agent_plist(exe: &Path) -> String {
    let exe = xml_escape(&exe.to_string_lossy());
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
	<true/>
	<key>ProcessType</key>
	<string>Background</string>
</dict>
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

/// Writes the launch agent for the `knode` executable at `exe` and returns its path. It runs
/// the service at the next login, or when loaded with `launchctl bootstrap`.
pub fn install_launch_agent(exe: impl AsRef<Path>) -> Result<PathBuf> {
    install_launch_agent_in(launch_agents_dir()?, exe)
}

/// Like [`install_launch_agent`], into the folder `dir` instead of `~/Library/LaunchAgents`.
pub fn install_launch_agent_in(dir: impl AsRef<Path>, exe: impl AsRef<Path>) -> Result<PathBuf> {
    let path = dir.as_ref().join(format!("{LAUNCH_AGENT_LABEL}.plist"));
    write_file(&path, &launch_agent_plist(&absolute_exe(exe.as_ref())?))?;
    Ok(path)
}

/// Removes the launch agent. Returns whether there was one. Unload it first with
/// `launchctl bootout` if it is running.
pub fn uninstall_launch_agent() -> Result<bool> {
    uninstall_launch_agent_in(launch_agents_dir()?)
}

/// Like [`uninstall_launch_agent`], from the folder `dir`.
pub fn uninstall_launch_agent_in(dir: impl AsRef<Path>) -> Result<bool> {
    remove_file(&dir.as_ref().join(format!("{LAUNCH_AGENT_LABEL}.plist")))
}

/// The systemd user unit that runs `<exe> service run` and restarts it when it stops.
pub fn systemd_unit(exe: &Path) -> String {
    let exe = systemd_quote(&exe.to_string_lossy());
    format!(
        "[Unit]
Description=Knode Service
Documentation=https://knode.md

[Service]
Type=simple
ExecStart={exe} service run
Restart=always
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

/// Writes the systemd user unit for the `knode` executable at `exe` and returns its path.
/// Enable it with `systemctl --user enable --now knode.service`.
pub fn install_systemd_unit(exe: impl AsRef<Path>) -> Result<PathBuf> {
    install_systemd_unit_in(systemd_user_dir()?, exe)
}

/// Like [`install_systemd_unit`], into the folder `dir`.
pub fn install_systemd_unit_in(dir: impl AsRef<Path>, exe: impl AsRef<Path>) -> Result<PathBuf> {
    let path = dir.as_ref().join(SYSTEMD_UNIT);
    write_file(&path, &systemd_unit(&absolute_exe(exe.as_ref())?))?;
    Ok(path)
}

/// Removes the systemd user unit. Returns whether there was one.
pub fn uninstall_systemd_unit() -> Result<bool> {
    uninstall_systemd_unit_in(systemd_user_dir()?)
}

/// Like [`uninstall_systemd_unit`], from the folder `dir`.
pub fn uninstall_systemd_unit_in(dir: impl AsRef<Path>) -> Result<bool> {
    remove_file(&dir.as_ref().join(SYSTEMD_UNIT))
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
    use super::*;

    #[test]
    fn launch_agent_runs_the_service_at_login() {
        let dir = tempfile::tempdir().unwrap();
        let path = install_launch_agent_in(dir.path(), "/opt/Knode & Co/bin/knode").unwrap();
        assert_eq!(path, dir.path().join("md.knode.service.plist"));
        let plist = fs::read_to_string(&path).unwrap();
        assert!(plist.contains("<string>md.knode.service</string>"));
        assert!(plist.contains("<string>/opt/Knode &amp; Co/bin/knode</string>"));
        assert!(plist.contains("<string>service</string>\n\t\t<string>run</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n\t<true/>"));
        assert!(uninstall_launch_agent_in(dir.path()).unwrap());
        assert!(!path.exists());
        assert!(!uninstall_launch_agent_in(dir.path()).unwrap());
    }

    #[test]
    fn systemd_unit_runs_the_service() {
        let dir = tempfile::tempdir().unwrap();
        let path = install_systemd_unit_in(dir.path(), "/home/me/100% \"knode\"/knode").unwrap();
        let unit = fs::read_to_string(&path).unwrap();
        assert!(unit.contains("ExecStart=\"/home/me/100%% \\\"knode\\\"/knode\" service run\n"));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(uninstall_systemd_unit_in(dir.path()).unwrap());
        assert!(!uninstall_systemd_unit_in(dir.path()).unwrap());
    }

    #[test]
    fn relative_executables_become_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let path = install_launch_agent_in(dir.path(), "knode").unwrap();
        let plist = fs::read_to_string(path).unwrap();
        let exe = std::env::current_dir().unwrap().join("knode");
        assert!(plist.contains(&format!("<string>{}</string>", exe.display())));
    }
}
