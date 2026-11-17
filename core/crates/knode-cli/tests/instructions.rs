//! `knode instructions`: the managed memory workflow block in a project's `CLAUDE.md` or
//! `AGENTS.md`, end to end, in a sandboxed home. Installing it keeps the person's own rules,
//! running it again changes nothing, an older block is replaced in place, and removal takes out
//! only the block.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::Sandbox;

const START: &str = "<!-- knode:workflow:start -->";
const END: &str = "<!-- knode:workflow:end -->";

/// The person's own rules, with a comment and no newline at the end.
const RULES: &str = "# Team rules\n\n<!-- reviewer: keep this list short -->\nNever push to main.\n- Run `cargo test` before a pull request.";

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn project(sb: &Sandbox, file: &str, text: &str) -> (PathBuf, PathBuf) {
    let dir = sb.path("project");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file);
    fs::write(&path, text).unwrap();
    (dir, path)
}

#[test]
fn user_rules_survive_install_reinstall_update_and_removal() {
    let sb = Sandbox::new();
    let (dir, file) = project(&sb, "CLAUDE.md", RULES);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let dir_arg = dir.to_str().unwrap();
    let run = |extra: &[&str]| {
        let mut args = vec!["instructions", "claude-code", "--project", dir_arg];
        args.extend_from_slice(extra);
        sb.ok(&args)
    };

    // Preview: nothing written, and it says it neither connects nor changes the space.
    let out = run(&[]);
    assert!(out.contains("Preview only"), "{out}");
    assert!(out.contains("does not connect an agent"), "{out}");
    assert_eq!(fs::read_to_string(&file).unwrap(), RULES);
    assert_eq!(files(&dir), ["CLAUDE.md"]);

    // Install: the rules first, byte for byte, then the block; a backup of the original.
    let out = run(&["--apply"]);
    assert!(out.contains("Start a fresh agent session"), "{out}");
    assert!(out.contains("/memory lists the files"), "{out}");
    assert!(out.contains("reference data"), "{out}");
    let installed = fs::read_to_string(&file).unwrap();
    assert!(
        installed.starts_with(&format!("{RULES}\n{START}\n")),
        "{installed}"
    );
    assert!(installed.ends_with(&format!("{END}\n")), "{installed}");
    assert!(installed.contains("Knode memory workflow (v1)"));
    assert_eq!(
        fs::read_to_string(dir.join("CLAUDE.md.knode-backup.0")).unwrap(),
        RULES
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    // Again: already up to date, no new backup, nothing changed.
    let out = run(&["--apply"]);
    assert!(out.contains("Already up to date"), "{out}");
    assert_eq!(fs::read_to_string(&file).unwrap(), installed);
    assert_eq!(files(&dir), ["CLAUDE.md", "CLAUDE.md.knode-backup.0"]);

    // The person adds rules after the block and an older version of the block is there: the
    // block is replaced where it is, and everything around it stays.
    let older = installed
        .replace("Knode memory workflow (v1)", "Knode memory workflow (v0)")
        .replace(
            &format!("{END}\n"),
            &format!("{END}\n\n## Local\nAsk before deleting files.\n"),
        );
    fs::write(&file, &older).unwrap();
    run(&["--apply"]);
    let updated = fs::read_to_string(&file).unwrap();
    assert!(updated.starts_with(&format!("{RULES}\n{START}\n")));
    assert!(updated.contains("Knode memory workflow (v1)"));
    assert!(!updated.contains("(v0)"));
    assert!(updated.ends_with(&format!("{END}\n\n## Local\nAsk before deleting files.\n")));
    assert_eq!(updated.matches(START).count(), 1);
    assert_eq!(
        fs::read_to_string(dir.join("CLAUDE.md.knode-backup.1")).unwrap(),
        older
    );

    // Removal: previewed first, then only the block goes.
    let out = run(&["--remove"]);
    assert!(out.contains("Preview only"), "{out}");
    assert_eq!(fs::read_to_string(&file).unwrap(), updated);
    run(&["--remove", "--apply"]);
    let removed = fs::read_to_string(&file).unwrap();
    assert_eq!(
        removed,
        format!("{RULES}\n\n\n## Local\nAsk before deleting files.\n")
    );
    assert!(!removed.contains("knode:workflow"));
    let out = run(&["--remove", "--apply"]);
    assert!(out.contains("Already up to date"), "{out}");
    assert_eq!(
        files(&dir),
        [
            "CLAUDE.md",
            "CLAUDE.md.knode-backup.0",
            "CLAUDE.md.knode-backup.1",
            "CLAUDE.md.knode-backup.2",
        ]
    );
}

#[test]
fn broken_markers_and_codex_overrides_leave_the_file_alone() {
    let sb = Sandbox::new();
    let broken = format!("{RULES}\n{START}\nhalf a block\n");
    let (dir, file) = project(&sb, "AGENTS.md", &broken);
    let dir_arg = dir.to_str().unwrap();
    for extra in [&["--apply"][..], &["--remove", "--apply"][..]] {
        let mut args = vec!["instructions", "codex", "--project", dir_arg];
        args.extend_from_slice(extra);
        let err = sb.fails(&args);
        assert!(err.contains("markers"), "{err}");
    }
    assert_eq!(fs::read_to_string(&file).unwrap(), broken);

    // Codex reads AGENTS.override.md instead of AGENTS.md: installing would have no effect.
    fs::write(&file, RULES).unwrap();
    fs::write(dir.join("AGENTS.override.md"), "# Override\n").unwrap();
    let err = sb.fails(&["instructions", "codex", "--project", dir_arg, "--apply"]);
    assert!(err.contains("AGENTS.override.md takes precedence"), "{err}");
    assert_eq!(fs::read_to_string(&file).unwrap(), RULES);
    assert_eq!(files(&dir), ["AGENTS.md", "AGENTS.override.md"]);
}
