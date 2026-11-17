//! `knode sources`: the importers and connectors of a space, through Knode Service.

mod common;

use common::Sandbox;

#[test]
fn sources_list_the_importers_and_refuse_to_connect_them() {
    let sb = Sandbox::new();
    sb.ok(&["init", "personal"]);

    let out = sb.ok(&["sources", "list"]);
    assert!(out.contains("Connectors"), "{out}");
    assert!(
        out.contains("Importers (knode import --from <id>)"),
        "{out}"
    );
    assert!(out.contains("whatsapp"), "{out}");

    let out = sb.ok(&["sources", "list", "--json"]);
    let list: serde_json::Value = serde_json::from_str(&out).unwrap();
    let pdf = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "pdf")
        .unwrap();
    assert_eq!(pdf["kind"], "importer");
    assert_eq!(pdf["state"], "importer");

    let err = sb.fails(&["sources", "connect", "pdf"]);
    assert!(err.contains("is an importer"), "{err}");
    let err = sb.fails(&["sources", "connect", "pdf", "--paused"]);
    assert!(err.contains("is an importer"), "{err}");
    let err = sb.fails(&["sources", "connect", "nope"]);
    assert!(err.contains("no source is named"), "{err}");
    let err = sb.fails(&["sources", "pause", "nope"]);
    assert!(err.contains("not connected"), "{err}");
    let err = sb.fails(&["sources", "connect", "x", "--history", "yesterday"]);
    assert!(err.contains("from-today"), "{err}");
}
