//! A sandbox for running `knode`: a temporary home, Knode home, agent folders and PATH, so tests
//! never touch the real settings of the person running them.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct Sandbox {
    pub dir: tempfile::TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["home", "bin"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        Sandbox { dir }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn home(&self) -> PathBuf {
        self.path("home")
    }

    pub fn knode_home(&self) -> PathBuf {
        self.path("knode-home")
    }

    pub fn space(&self, name: &str) -> PathBuf {
        self.home().join("Knode").join(format!("{name}.knode"))
    }

    /// The `knode` command with a clean environment pointing into the sandbox.
    pub fn cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_knode"));
        cmd.env_clear()
            .env("HOME", self.home())
            .env("USER", "tester")
            .env("KNODE_HOME", self.knode_home())
            .env("CODEX_HOME", self.path("codex"))
            .env("CLAUDE_CONFIG_DIR", self.path("claude"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("PATH", self.path("bin"))
            // Agent keys go to files in the sandbox, never to the Keychain of the machine.
            .env("KNODE_KEY_STORE", "file")
            .current_dir(self.dir.path());
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    /// Runs `knode` and expects success; returns its standard output.
    pub fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "knode {args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Runs `knode` and expects failure; returns its standard error.
    pub fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "knode {args:?} should fail:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        String::from_utf8(out.stderr).unwrap()
    }

    /// Puts a fake program on the sandbox PATH that logs its arguments to `<name>.log` and
    /// exits with `code` for `mcp get` and 0 otherwise.
    #[cfg(unix)]
    pub fn fake_program(&self, name: &str, get_code: i32) {
        use std::os::unix::fs::PermissionsExt;
        let log = self.path(&format!("{name}.log"));
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1 $2\" = \"mcp get\" ]; then exit {get_code}; fi\nexit 0\n",
            log.display()
        );
        let path = self.path("bin").join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Connects Gemini CLI (its settings are a file in the sandbox) and returns the agent name
    /// `knode mcp --agent` takes.
    pub fn connect_agent(&self) -> &'static str {
        self.ok(&["connect", "gemini"]);
        "gemini-cli"
    }

    pub fn log(&self, name: &str) -> String {
        fs::read_to_string(self.path(&format!("{name}.log"))).unwrap_or_default()
    }
}

impl Drop for Sandbox {
    /// Stops the Knode Service a command may have started in the background.
    fn drop(&mut self) {
        let _ = self.cmd().args(["service", "stop"]).output();
    }
}

pub fn ingest_fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../knode-ingest/tests/fixtures")
        .join(rel)
}
