//! `knode spaces` and `knode folders`: names, icons, the default space and the folders that
//! feed a space, through Knode Service.

mod common;

use std::fs;

use common::Sandbox;

fn line_of(out: &str, name: &str) -> String {
    out.lines()
        .find(|l| l.contains(&format!("{name}.knode")))
        .unwrap_or_else(|| panic!("no {name} in {out}"))
        .to_string()
}

#[test]
fn spaces_are_renamed_given_icons_and_made_the_default() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    sb.ok(&["init", "personal"]);

    let out = sb.ok(&["spaces", "rename", "work", "Day job"]);
    assert!(out.contains("to \"Day job\""), "{out}");
    let out = sb.ok(&["spaces", "icon", "Day job", "💼"]);
    assert!(out.contains("💼"), "{out}");
    let personal = sb.space("personal");
    sb.ok(&["spaces", "icon", personal.to_str().unwrap(), "🌱"]);

    let out = sb.ok(&["spaces"]);
    assert_eq!(
        line_of(&out, "work"),
        format!("* 💼 Day job   {}  (default)", sb.space("work").display())
    );
    assert!(
        line_of(&out, "personal").starts_with("  🌱 personal  "),
        "{out}"
    );

    let out = sb.ok(&["spaces", "default", "personal"]);
    assert!(
        out.contains("\"personal\" is now your default space"),
        "{out}"
    );
    let out = sb.ok(&["spaces", "capture", "Day job", "off"]);
    assert!(out.contains("no longer captured"), "{out}");
    let out = sb.ok(&["spaces", "--json"]);
    let list: serde_json::Value = serde_json::from_str(&out).unwrap();
    let work = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Day job")
        .unwrap();
    assert_eq!(work["icon"], "💼");
    assert_eq!(work["default"], false);
    assert_eq!(work["capture"], false);
    let config = fs::read_to_string(sb.knode_home().join("config.toml")).unwrap();
    assert!(config.contains("personal.knode"), "{config}");
    assert!(config.contains("work.knode"), "{config}");

    sb.ok(&["spaces", "icon", "personal", "--clear"]);
    let out = sb.ok(&["spaces"]);
    assert!(
        line_of(&out, "personal").starts_with("*   personal"),
        "{out}"
    );

    let err = sb.fails(&["spaces", "icon", "personal", "seedling"]);
    assert!(err.contains("one emoji"), "{err}");
    let err = sb.fails(&["spaces", "rename", "nowhere", "X"]);
    assert!(err.contains("no space named"), "{err}");
}

#[test]
fn folders_are_added_listed_and_removed() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    sb.ok(&["init", "personal"]);
    let project = sb.home().join("Developer/site");
    fs::create_dir_all(&project).unwrap();
    let agents = sb.path("team-sessions");
    fs::create_dir_all(&agents).unwrap();

    let out = sb.ok(&["folders", "list"]);
    assert!(out.contains("has no folders"), "{out}");

    let out = sb.ok(&[
        "folders",
        "add",
        project.to_str().unwrap(),
        "--exclude",
        "node_modules/**",
    ]);
    assert!(out.contains("project  ~/Developer/site"), "{out}");
    assert!(out.contains("exclude node_modules/**"), "{out}");
    // Capture off for a project of another space, named by its name.
    let secret = sb.home().join("Developer/secret");
    let out = sb.ok(&[
        "folders",
        "add",
        secret.to_str().unwrap(),
        "--space",
        "personal",
        "--capture",
        "off",
    ]);
    assert!(out.contains("capture off"), "{out}");
    assert!(out.contains("missing here"), "{out}");
    let out = sb.ok(&[
        "folders",
        "add",
        agents.to_str().unwrap(),
        "--kind",
        "agent",
        "--agent",
        "codex",
    ]);
    assert!(out.contains("agent codex"), "{out}");
    let err = sb.fails(&[
        "folders",
        "add",
        agents.to_str().unwrap(),
        "--kind",
        "agent",
    ]);
    assert!(err.contains("--agent"), "{err}");

    let out = sb.ok(&["folders", "list", "--json"]);
    let list: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2, "{out}");
    assert_eq!(list[0]["path"], "~/Developer/site");
    assert_eq!(list[0]["capture"], true);

    let out = sb.ok(&["folders", "remove", project.to_str().unwrap()]);
    assert!(
        out.contains("Removed the project folder ~/Developer/site"),
        "{out}"
    );
    let out = sb.ok(&["folders", "list"]);
    assert!(!out.contains("site"), "{out}");
    let err = sb.fails(&["folders", "remove", project.to_str().unwrap()]);
    assert!(err.contains("has no folder"), "{err}");
    let out = sb.ok(&["--space", "personal", "folders", "list"]);
    assert!(out.contains("~/Developer/secret"), "{out}");
}
