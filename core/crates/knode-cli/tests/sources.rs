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
    let err = sb.fails(&["sources", "connect", "nope"]);
    assert!(err.contains("no source is named"), "{err}");
    let err = sb.fails(&["sources", "pause", "nope"]);
    assert!(err.contains("not connected"), "{err}");
    let err = sb.fails(&["sources", "connect", "x", "--history", "yesterday"]);
    assert!(err.contains("from-today"), "{err}");
}

#[test]
fn mail_connectors_sign_in_and_out() {
    let sb = Sandbox::new();
    sb.ok(&["init", "personal"]);
    let out = sb.ok(&["sources", "list"]);
    assert!(out.contains("gmail") && out.contains("imap"), "{out}");

    // A build without Knode's Google client says so instead of opening a page.
    if option_env!("KNODE_GOOGLE_CLIENT_ID").is_none() {
        let err = sb.fails(&["sources", "sign-in", "gmail"]);
        assert!(err.contains("not set up"), "{err}");
        let err = sb.fails(&["sources", "connect", "gmail"]);
        assert!(err.contains("not set up"), "{err}");
    }

    // IMAP asks for the account; with nothing typed, signing in stops.
    let out = sb.run(&["sources", "sign-in", "imap"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("IMAP server"), "{err}");
    assert!(err.contains("signing in stopped"), "{err}");

    let out = sb.ok(&["sources", "sign-out", "imap"]);
    assert!(out.contains("was not signed in"), "{out}");
}
