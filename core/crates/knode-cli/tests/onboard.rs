//! A normal-user tutorial with a disposable home; never reads or connects a real agent.
mod common;
use common::Sandbox;
use serde_json::Value;
use std::fs;

#[test]
fn guided_import_recovers_sources_copy_and_previous_codex_configuration() {
    let sb = Sandbox::new();
    sb.ok(&["init", "Existing"]);
    let source = sb.path("vault");
    fs::create_dir_all(source.join("Projects/One")).unwrap();
    fs::create_dir_all(source.join("Projects/Two")).unwrap();
    fs::write(source.join("Projects/One/README.md"), "Original one").unwrap();
    fs::write(source.join("Projects/Two/README.md"), "Original two").unwrap();
    let raw = sb.ok(&[
        "onboard",
        "analyze",
        "--mode",
        "vault",
        "--source",
        source.to_str().unwrap(),
    ]);
    let mut plan: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(plan["groups"].as_array().unwrap().len(), 2);
    for group in plan["groups"].as_array_mut().unwrap() {
        group["name"] = "Merged".into();
    }
    let file = sb.path("plan.json");
    fs::write(&file, serde_json::to_vec(&plan).unwrap()).unwrap();
    let target = sb.path("Imported.knode");
    let raw = sb.ok(&[
        "onboard",
        "apply",
        "--plan",
        file.to_str().unwrap(),
        "--destination",
        target.to_str().unwrap(),
        "--name",
        "Imported",
    ]);
    let receipt: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(receipt["contexts"].as_object().unwrap().len(), 1);
    let config = sb.path("codex/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "model = \"synthetic-test\"\n").unwrap();
    sb.ok(&[
        "onboard",
        "connect-codex",
        "--target",
        target.to_str().unwrap(),
    ]);
    assert!(
        fs::read_to_string(&config)
            .unwrap()
            .contains("mcp_servers.knode")
    );
    sb.ok(&[
        "onboard",
        "connect-codex",
        "--target",
        target.to_str().unwrap(),
    ]);

    // Knode Service runs before archiving: it is stopped to move the package, then started again.
    sb.ok(&["service", "start"]);
    let raw = sb.ok(&["onboard", "archive", "--target", target.to_str().unwrap()]);
    let archived: Value = serde_json::from_str(&raw).unwrap();
    assert!(!target.exists());
    assert!(
        sb.ok(&["service", "status"]).contains("is running"),
        "Knode Service did not start again after archiving"
    );
    sb.ok(&["service", "stop"]);
    assert!(std::path::Path::new(archived["archived"].as_str().unwrap()).exists());
    assert_eq!(
        fs::read_to_string(config).unwrap(),
        "model = \"synthetic-test\"\n"
    );
    assert_eq!(
        fs::read_to_string(source.join("Projects/One/README.md")).unwrap(),
        "Original one"
    );
    let recovered = knode_format::Space::open(archived["archived"].as_str().unwrap()).unwrap();
    assert_eq!(recovered.node_count().unwrap(), 3);
}

#[test]
fn empty_start_and_later_config_edits_are_preserved_on_archive() {
    let sb = Sandbox::new();
    let raw = sb.ok(&["onboard", "analyze", "--mode", "empty"]);
    let plan = sb.path("plan.json");
    fs::write(&plan, raw).unwrap();
    let target = sb.path("Fresh.knode");
    sb.ok(&[
        "onboard",
        "apply",
        "--plan",
        plan.to_str().unwrap(),
        "--destination",
        target.to_str().unwrap(),
        "--name",
        "Fresh",
    ]);
    sb.ok(&[
        "onboard",
        "connect-codex",
        "--target",
        target.to_str().unwrap(),
    ]);
    let config = sb.path("codex/config.toml");
    let mut current = fs::read_to_string(&config).unwrap();
    current.push_str("\n# User edited configuration after setup\n");
    fs::write(&config, &current).unwrap();
    let out = sb.ok(&["onboard", "archive", "--target", target.to_str().unwrap()]);
    assert!(out.contains("left untouched"));
    assert_eq!(fs::read_to_string(config).unwrap(), current);
}
