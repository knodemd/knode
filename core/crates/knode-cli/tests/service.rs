//! `knode service`, and capture by the service the CLI starts. launchctl and systemctl are fakes
//! on the sandbox PATH, and the launch agent goes to the sandbox home: the real ones are never
//! touched.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, Instant};

use common::{Sandbox, ingest_fixture};

/// Puts a fake service manager on the sandbox PATH that logs its arguments and runs or stops
/// the service the way launchd or systemd would.
fn fake_service_manager(sb: &Sandbox) -> &'static str {
    let (name, start, stop) = if cfg!(target_os = "macos") {
        ("launchctl", "bootstrap", "bootout")
    } else {
        ("systemctl", "enable", "disable")
    };
    let knode = env!("CARGO_BIN_EXE_knode");
    let log = sb.path(&format!("{name}.log"));
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\n\
         for arg in \"$@\"; do\n\
           if [ \"$arg\" = '{start}' ]; then '{knode}' service run >/dev/null 2>&1 & exit 0; fi\n\
           if [ \"$arg\" = '{stop}' ]; then '{knode}' service stop >/dev/null 2>&1; exit 0; fi\n\
         done\nexit 0\n",
        log = log.display()
    );
    let path = sb.path("bin").join(name);
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    name
}

fn login_file(sb: &Sandbox) -> std::path::PathBuf {
    if cfg!(target_os = "macos") {
        sb.home()
            .join("Library/LaunchAgents/md.knode.service.plist")
    } else {
        sb.path("config/systemd/user/knode.service")
    }
}

fn pid_in(text: &str) -> String {
    let start = text.find("(pid ").expect("a pid") + 5;
    text[start..].split(')').next().unwrap().to_string()
}

#[test]
fn start_status_and_stop() {
    let sb = Sandbox::new();
    let out = sb.ok(&["service", "status"]);
    assert!(out.contains("Knode Service is not running."), "{out}");
    assert!(out.contains("Starts at login: no"), "{out}");

    let out = sb.ok(&["service", "start"]);
    assert!(out.starts_with("Started Knode Service (pid "), "{out}");
    let pid = pid_in(&out);
    let out = sb.ok(&["service", "start"]);
    assert_eq!(
        out,
        format!("Knode Service is already running (pid {pid}).\n")
    );

    let out = sb.ok(&["service", "status"]);
    assert!(out.contains(&format!("is running (pid {pid})")), "{out}");
    assert!(out.contains("Spaces: none open"), "{out}");
    assert!(out.contains("Clients: 0"), "{out}");
    assert!(out.contains("Capture: off"), "{out}");

    // A second service in the same home is not an error: one is running.
    let run = sb.run(&["service", "run"]);
    assert!(run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("already running"), "{stderr}");

    let out = sb.ok(&["service", "stop"]);
    assert_eq!(out, format!("Stopped Knode Service (pid {pid}).\n"));
    assert!(!sb.knode_home().join("service.sock").exists());
    let out = sb.ok(&["service", "stop"]);
    assert_eq!(out, "Knode Service is not running.\n");
}

#[test]
fn install_starts_it_at_login_and_uninstall_stops_that() {
    let sb = Sandbox::new();
    let manager = fake_service_manager(&sb);
    // A service started by hand gives way to the one the service manager runs.
    let manual = pid_in(&sb.ok(&["service", "start"]));

    let out = sb.ok(&["service", "install"]);
    assert!(out.contains("now starts at login"), "{out}");
    let file = login_file(&sb);
    assert!(file.exists(), "{}", file.display());
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains(env!("CARGO_BIN_EXE_knode")), "{text}");
    let pid = pid_in(&out);
    assert_ne!(pid, manual);
    let log = sb.log(manager);
    if cfg!(target_os = "macos") {
        // The sandbox was made by this user.
        let uid = std::os::unix::fs::MetadataExt::uid(&fs::metadata(sb.home()).unwrap());
        assert!(
            log.contains(&format!("bootstrap gui/{uid} {}", file.display())),
            "{log}"
        );
        assert!(text.contains("service.log"), "{text}");
    } else {
        assert!(log.contains("--user enable --now knode.service"), "{log}");
    }

    let out = sb.ok(&["service", "status"]);
    assert!(out.contains(&format!("is running (pid {pid})")), "{out}");
    assert!(
        out.contains("Starts at login: yes, ") && out.contains(&*file.to_string_lossy()),
        "{out}"
    );

    // Stopped on purpose, it stays stopped until the next login.
    let out = sb.ok(&["service", "stop"]);
    assert!(
        out.contains(&format!("Stopped Knode Service (pid {pid})")),
        "{out}"
    );
    assert!(out.contains("starts again at the next login"), "{out}");

    sb.ok(&["service", "start"]);
    let out = sb.ok(&["service", "uninstall"]);
    assert!(out.contains("no longer starts at login"), "{out}");
    assert!(!file.exists());
    assert!(!sb.knode_home().join("service.sock").exists());
    let out = sb.ok(&["service", "uninstall"]);
    assert!(out.contains("was not set to start at login"), "{out}");
    let out = sb.ok(&["service", "status"]);
    assert!(
        out.contains("not running") && out.contains("Starts at login: no"),
        "{out}"
    );
}

#[test]
fn the_service_captures_sessions_into_the_default_space() {
    let sb = Sandbox::new();
    let projects = sb.path("claude/projects/-Users-example-project");
    fs::create_dir_all(&projects).unwrap();
    let session = "0f8e2c1a-1111-4222-8333-444455556666.jsonl";
    fs::copy(
        ingest_fixture(&format!("claude/projects/-Users-example-project/{session}")),
        projects.join(session),
    )
    .unwrap();

    // init starts the service, which picks up the new default space.
    sb.ok(&["init", "work"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let found = sb.ok(&["search", "health", "check"]);
        if found.contains("Server health check") {
            assert!(found.contains("conversation"), "{found}");
            break;
        }
        assert!(Instant::now() < deadline, "nothing captured: {found}");
        thread::sleep(Duration::from_millis(200));
    }
    let status = sb.ok(&["service", "status"]);
    assert!(status.contains("Capture: on, into "), "{status}");
    assert!(sb.knode_home().join("capture/state.json").exists());
}

#[test]
fn capture_can_be_turned_off() {
    let sb = Sandbox::new();
    let projects = sb.path("claude/projects/-Users-example-project");
    fs::create_dir_all(&projects).unwrap();
    let session = "0f8e2c1a-1111-4222-8333-444455556666.jsonl";
    fs::copy(
        ingest_fixture(&format!("claude/projects/-Users-example-project/{session}")),
        projects.join(session),
    )
    .unwrap();
    fs::create_dir_all(sb.knode_home()).unwrap();
    fs::write(sb.knode_home().join("config.toml"), "capture = false\n").unwrap();
    sb.ok(&["init", "work"]);
    // The service reads config.toml again within a few seconds; give it time to (not) capture.
    thread::sleep(Duration::from_secs(6));
    let status = sb.ok(&["service", "status"]);
    assert!(status.contains("Capture: off"), "{status}");
    let found = sb.ok(&["search", "health"]);
    assert!(found.contains("Nothing matches"), "{found}");
    let config = fs::read_to_string(sb.knode_home().join("config.toml")).unwrap();
    assert!(config.contains("capture = false"), "{config}");
}
