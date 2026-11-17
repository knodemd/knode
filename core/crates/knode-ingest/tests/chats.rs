//! Importing chat exports. The fixtures in `fixtures/chats` are synthetic: invented chats
//! between invented people, written by hand in the structure of each export.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use knode_format::{ChangeOp, Node, NodeId, Provenance, Space, link_kinds};
use knode_ingest::{
    CHAT_KIND, ChatFormat, ChatImportError, PERSON_KIND, chat_keys, detect_chat_export,
    import_chat_export, person_keys,
};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/chats")
        .join(name)
}

fn by() -> Provenance {
    Provenance::person("example")
}

fn space(dir: &TempDir) -> Space {
    Space::create(dir.path().join("Test.knode"), "Test").unwrap()
}

/// A zip of `entries`.
fn zip(out: &Path, entries: &[(&str, &[u8])]) {
    let mut writer = zip::ZipWriter::new(File::create(out).unwrap());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

/// A zip of the files of a fixture folder, at the top of the archive.
fn zip_fixture(dir: &Path, out: &Path) {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        entries.push((name, fs::read(&path).unwrap()));
    }
    let borrowed: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    zip(out, &borrowed);
}

fn chat_node(space: &Space, app: &str, id: &str) -> Node {
    let found = space.find_by_property(chat_keys::ID, id).unwrap();
    let found: Vec<_> = found
        .into_iter()
        .filter(|s| space.property(&s.id, chat_keys::APP).unwrap().as_deref() == Some(app))
        .collect();
    assert_eq!(found.len(), 1, "one {app} chat {id}");
    space.get_node(&found[0].id).unwrap().unwrap()
}

fn prop(space: &Space, node: &Node, key: &str) -> String {
    space.property(&node.id, key).unwrap().unwrap_or_default()
}

const IOS: &str = "whatsapp-ios/WhatsApp Chat - Book Club";

#[test]
fn imports_an_ios_export_folder() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = import_chat_export(&mut space, fixture(IOS), None, &by()).unwrap();
    assert_eq!(report.format, ChatFormat::WhatsApp);
    assert_eq!(report.chats.len(), 1);
    assert_eq!(report.chats[0].op, ChangeOp::Create);
    assert_eq!(report.chats[0].title, "WhatsApp - Book Club");
    assert_eq!(report.messages, 8);
    assert_eq!(report.attached, 1);
    assert_eq!(report.missing, 2, "the voice note and the omitted image");
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    let node = chat_node(&space, "whatsapp", "book club");
    assert_eq!(node.kind, CHAT_KIND);
    assert_eq!(node.title, "WhatsApp - Book Club");
    assert_eq!(
        node.body,
        "## 2026-03-04\n\n\
         *(09:14) Messages and calls are end-to-end encrypted. Only people in this chat can read, listen to, or share them.*\n\n\
         *(09:14) Ada created group \"Book Club\"*\n\n\
         **Ada** (09:15): Shall we read the lighthouse novel next?\n\n\
         **Grace** (09:16): Yes!\nIt is long though.\nMaybe two months?\n\n\
         **Ada** (09:17): ![[00000003-PHOTO-2026-03-04-09-17-00.jpg]]\n\n\
         **Grace** (09:18): *[missing from the export: 00000004-AUDIO-2026-03-04-09-18-00.opus]*\n\n\
         ## 2026-03-13\n\n\
         **Linus** (21:40): *[image left out of the export]*\n\n\
         **Linus** (21:41): See you on Friday at the library\n"
    );
    assert_eq!(prop(&space, &node, chat_keys::TYPE), "group");
    assert_eq!(
        prop(&space, &node, chat_keys::PARTICIPANTS),
        "Ada, Grace, Linus"
    );
    assert_eq!(
        prop(&space, &node, chat_keys::STARTED_AT),
        "2026-03-04T09:14:00"
    );
    assert_eq!(
        prop(&space, &node, chat_keys::ENDED_AT),
        "2026-03-13T21:41:00"
    );
    assert_eq!(prop(&space, &node, chat_keys::MESSAGES), "8");
    let attachments = space.attachments(&node.id).unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(
        attachments[0].name,
        "00000003-PHOTO-2026-03-04-09-17-00.jpg"
    );
    assert_eq!(attachments[0].media_type, "image/jpeg");
    // Written by the person who imported it, from the export.
    assert_eq!(node.created_by.actor, "example");
    assert!(
        node.created_by
            .source
            .as_deref()
            .is_some_and(|s| s.ends_with("Book Club"))
    );
}

#[test]
fn importing_again_adds_only_new_messages() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    import_chat_export(&mut space, fixture(IOS), None, &by()).unwrap();
    let before = chat_node(&space, "whatsapp", "book club");

    // The same export again: nothing changes.
    let again = import_chat_export(&mut space, fixture(IOS), None, &by()).unwrap();
    assert!(again.chats.is_empty(), "{again:?}");
    assert_eq!(again.unchanged, 1);
    assert_eq!(again.messages, 0);
    assert_eq!(again.attached, 0);
    let same = chat_node(&space, "whatsapp", "book club");
    assert_eq!(same.body, before.body);
    assert_eq!(same.updated_at, before.updated_at);
    assert_eq!(space.node_count().unwrap(), 1);

    // A newer export of the same chat, zipped this time: the old messages, one more on the
    // last day, one in the same minute as the last, and a new day.
    let newer = dir.path().join("newer");
    fs::create_dir_all(&newer).unwrap();
    let mut text = fs::read_to_string(fixture(IOS).join("_chat.txt")).unwrap();
    text.push_str("[13/03/2026, 21:41:30] Ada: See you there\n");
    text.push_str("[13/03/2026, 21:41:59] Ada: See you there\n");
    text.push_str("[14/03/2026, 08:00:00] Grace: \u{200e}<attached: 00000009-PHOTO-2026-03-14-08-00-00.jpg>\nThe cover\n");
    let photo = fs::read(fixture(IOS).join("00000003-PHOTO-2026-03-04-09-17-00.jpg")).unwrap();
    let zipped = dir.path().join("WhatsApp Chat - Book Club.zip");
    zip(
        &zipped,
        &[
            ("_chat.txt", text.as_bytes()),
            ("00000003-PHOTO-2026-03-04-09-17-00.jpg", &photo),
            (
                "00000009-PHOTO-2026-03-14-08-00-00.jpg",
                b"\xff\xd8\xff cover",
            ),
        ],
    );
    let report = import_chat_export(&mut space, &zipped, None, &by()).unwrap();
    assert_eq!(report.chats.len(), 1);
    assert_eq!(report.chats[0].op, ChangeOp::Update);
    assert_eq!(report.messages, 3);
    assert_eq!(report.attached, 1);
    let node = chat_node(&space, "whatsapp", "book club");
    assert_eq!(node.id, before.id);
    assert!(node.body.starts_with(before.body.trim_end()));
    assert_eq!(
        &node.body[before.body.trim_end().len()..],
        "\n\n**Ada** (21:41): See you there\n\n**Ada** (21:41): See you there\n\n\
         ## 2026-03-14\n\n**Grace** (08:00): The cover\n![[00000009-PHOTO-2026-03-14-08-00-00.jpg]]\n"
    );
    assert_eq!(node.body.matches("## 2026-03-13").count(), 1);
    assert_eq!(prop(&space, &node, chat_keys::MESSAGES), "11");
    assert_eq!(space.attachments(&node.id).unwrap().len(), 2);

    // And that export once more: nothing new.
    let last = import_chat_export(&mut space, &zipped, None, &by()).unwrap();
    assert_eq!((last.unchanged, last.messages), (1, 0));
}

#[test]
fn imports_an_android_export_as_zip_folder_or_text_file() {
    let android = fixture("whatsapp-android");
    let dir = tempfile::tempdir().unwrap();
    let zipped = dir.path().join("WhatsApp Chat with Garden Plot.zip");
    zip_fixture(&android, &zipped);
    let text = android.join("WhatsApp Chat with Garden Plot.txt");

    let expected = "## 2026-03-04\n\n\
         *(09:14) Messages and calls are end-to-end encrypted. No one outside of this chat can read or listen to them.*\n\n\
         **Ada** (09:15): Morning! Did you see the garden plan?\n\n\
         **Mira** (09:16): The whiteboard from yesterday\n![[IMG-20260304-WA0001.jpg]]\n\n\
         **Mira** (09:17): *[media left out of the export]*\n\n\
         *(09:20) Mira changed this group's icon*\n\n\
         ## 2026-03-14\n\n\
         **Ada** (18:05): Thanks, the tomatoes go on the left\n";
    for path in [&zipped, &android, &text] {
        assert_eq!(detect_chat_export(path), Some(ChatFormat::WhatsApp));
        let dir = tempfile::tempdir().unwrap();
        let mut space = space(&dir);
        let report = import_chat_export(&mut space, path, Some(ChatFormat::WhatsApp), &by())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(report.messages, 6, "{}", path.display());
        assert_eq!(report.attached, 1, "{}", path.display());
        let node = chat_node(&space, "whatsapp", "garden plot");
        assert_eq!(node.title, "WhatsApp - Garden Plot");
        assert_eq!(node.body, expected, "{}", path.display());
        assert_eq!(prop(&space, &node, chat_keys::TYPE), "group");
        assert_eq!(prop(&space, &node, chat_keys::PARTICIPANTS), "Ada, Mira");
    }
}

#[test]
fn reads_every_locale_family() {
    // The same three messages as phones in different languages write them.
    let families: [(&str, &str); 10] = [
        (
            "ios-day-month",
            "[04/03/2026, 09:15:02] Ada: one\n[04/03/2026, 21:40:00] Grace: two\n[13/03/2026, 07:05:00] Ada: three\n",
        ),
        (
            "ios-month-day-12h",
            "[3/4/26, 9:15:02\u{202f}AM] Ada: one\n[3/4/26, 9:40:00\u{202f}PM] Grace: two\n[3/13/26, 7:05:00\u{202f}AM] Ada: three\n",
        ),
        (
            "ios-dots",
            "[04.03.26, 09:15:02] Ada: one\n[04.03.26, 21:40:00] Grace: two\n[13.03.26, 07:05:00] Ada: three\n",
        ),
        (
            "android-day-month",
            "04/03/2026, 09:15 - Ada: one\n04/03/2026, 21:40 - Grace: two\n13/03/2026, 07:05 - Ada: three\n",
        ),
        (
            "android-month-day-12h",
            "3/4/26, 9:15 AM - Ada: one\n3/4/26, 9:40 PM - Grace: two\n3/13/26, 7:05 AM - Ada: three\n",
        ),
        (
            "android-dots",
            "04.03.26, 09:15 - Ada: one\n04.03.26, 21:40 - Grace: two\n13.03.26, 07:05 - Ada: three\n",
        ),
        (
            "android-year-first",
            "2026-03-04 09:15 - Ada: one\n2026-03-04 21:40 - Grace: two\n2026-03-13 07:05 - Ada: three\n",
        ),
        (
            "android-spanish-12h",
            "4/3/26, 9:15 a. m. - Ada: one\n4/3/26, 9:40 p. m. - Grace: two\n13/3/26, 7:05 a. m. - Ada: three\n",
        ),
        (
            "android-french-a",
            "04/03/2026 à 09:15 - Ada: one\n04/03/2026 à 21:40 - Grace: two\n13/03/2026 à 07:05 - Ada: three\n",
        ),
        (
            "android-finnish-klo",
            "4.3.2026 klo 9.15 - Ada: one\n4.3.2026 klo 21.40 - Grace: two\n13.3.2026 klo 7.05 - Ada: three\n",
        ),
    ];
    for (family, text) in families {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join(format!("WhatsApp Chat - {family}"));
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("_chat.txt"), text).unwrap();
        let mut space = space(&dir);
        import_chat_export(&mut space, &folder, None, &by())
            .unwrap_or_else(|e| panic!("{family}: {e}"));
        let node = chat_node(&space, "whatsapp", family);
        assert_eq!(
            node.body,
            "## 2026-03-04\n\n**Ada** (09:15): one\n\n**Grace** (21:40): two\n\n\
             ## 2026-03-13\n\n**Ada** (07:05): three\n",
            "{family}"
        );
    }
}

#[test]
fn refuses_what_is_not_a_chat_export() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes");
    fs::create_dir_all(&notes).unwrap();
    fs::write(notes.join("_chat.txt"), "Just some notes\nabout nothing\n").unwrap();
    assert_eq!(detect_chat_export(&notes), None);
    assert_eq!(detect_chat_export(dir.path().join("missing")), None);
    let mut space = space(&dir);
    let error = import_chat_export(&mut space, &notes, None, &by()).unwrap_err();
    assert!(matches!(error, ChatImportError::NotRecognized { .. }));
    assert!(
        error
            .to_string()
            .contains("is not a WhatsApp, Telegram Desktop or Slack chat export")
    );
    assert_eq!(space.node_count().unwrap(), 0);
}

#[test]
fn imports_a_telegram_chat_export() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let path = fixture("telegram-chat");
    assert_eq!(detect_chat_export(&path), Some(ChatFormat::Telegram));
    assert_eq!(
        detect_chat_export(path.join("result.json")),
        Some(ChatFormat::Telegram)
    );
    let report = import_chat_export(&mut space, &path, None, &by()).unwrap();
    assert_eq!(report.format, ChatFormat::Telegram);
    assert_eq!(report.chats.len(), 1);
    assert_eq!(report.messages, 7);
    assert_eq!(report.attached, 2);
    assert_eq!(report.missing, 1, "the PDF the export left out");
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    let node = chat_node(&space, "telegram", "1000000001");
    assert_eq!(node.kind, CHAT_KIND);
    assert_eq!(node.title, "Telegram - Book Club");
    let opening = "\"Shall we read the lighthouse novel next?…\"";
    assert_eq!(
        node.body,
        format!(
            "## 2026-03-04\n\n\
             *(09:14) Ada created \"Book Club\"*\n\n\
             **Ada** (09:15): Shall we read **the lighthouse novel** next? Notes are [here](<https://example.com/notes>).\n\n\
             **Grace** (09:16, replying to Ada at 09:15: {opening}): @ada yes! Run `make list` first.\n\n\
             **Ada** (09:17): The cover\n![[photo_1@04-03-2026_09-17-00.jpg]]\n\n\
             **Grace** (09:18): [[audio_1@04-03-2026_09-18-00.ogg]]\n\n\
             ## 2026-03-05\n\n\
             **Grace** (20:00): *[reading-list.pdf left out of the export]*\n\n\
             **Linus** (20:01, replying to Ada at 2026-03-04 09:15: {opening}): *Finally*\n```rust\nfn main() {{}}\n```\n"
        )
    );
    assert_eq!(prop(&space, &node, chat_keys::TYPE), "group");
    assert_eq!(
        prop(&space, &node, chat_keys::PARTICIPANTS),
        "Ada, Grace, Linus"
    );
    assert_eq!(prop(&space, &node, chat_keys::LAST), "00000000000000000007");
    let names: Vec<String> = space
        .attachments(&node.id)
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(
        names,
        [
            "photo_1@04-03-2026_09-17-00.jpg",
            "audio_1@04-03-2026_09-18-00.ogg"
        ]
    );

    // Again, zipped: nothing new. Then a newer export with one more message.
    let zipped = dir.path().join("ChatExport.zip");
    let photo = fs::read(path.join("photos/photo_1@04-03-2026_09-17-00.jpg")).unwrap();
    let result = fs::read_to_string(path.join("result.json")).unwrap();
    zip(
        &zipped,
        &[
            ("ChatExport_2026-03-06/result.json", result.as_bytes()),
            (
                "ChatExport_2026-03-06/photos/photo_1@04-03-2026_09-17-00.jpg",
                &photo,
            ),
        ],
    );
    let again = import_chat_export(&mut space, &zipped, None, &by()).unwrap();
    assert_eq!(
        (again.unchanged, again.messages, again.chats.len()),
        (1, 0, 0)
    );

    let newer = result.replacen(
        "\"messages\": [",
        "\"messages\": [\n{\"id\": 8, \"type\": \"message\", \"date\": \"2026-03-05T21:00:00\", \"from\": \"Ada\", \"text\": \"Night all\", \"text_entities\": [{\"type\": \"plain\", \"text\": \"Night all\"}]},",
        1,
    );
    assert_ne!(newer, result);
    let newer_dir = dir.path().join("newer");
    fs::create_dir_all(&newer_dir).unwrap();
    fs::write(newer_dir.join("result.json"), newer).unwrap();
    let report =
        import_chat_export(&mut space, &newer_dir, Some(ChatFormat::Telegram), &by()).unwrap();
    assert_eq!(report.messages, 1);
    let updated = chat_node(&space, "telegram", "1000000001");
    assert_eq!(
        &updated.body[node.body.len()..],
        "\n**Ada** (21:00): Night all\n"
    );
    assert_eq!(prop(&space, &updated, chat_keys::MESSAGES), "8");
}

#[test]
fn imports_every_chat_of_a_telegram_account_export() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let path = fixture("telegram-account");
    let report = import_chat_export(&mut space, &path, None, &by()).unwrap();
    assert_eq!(report.format, ChatFormat::Telegram);
    let titles: Vec<&str> = report.chats.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Telegram - Grace",
            "Telegram - Garden News",
            "Telegram - Old Team"
        ]
    );
    assert_eq!(report.empty, 1, "the chat with no messages");
    assert_eq!(report.messages, 4);

    let grace = chat_node(&space, "telegram", "102");
    assert_eq!(
        grace.body,
        "## 2026-03-04\n\n**Grace** (08:00): Morning\n\n**Ada** (08:01): ~~Tea~~ Coffee?\n"
    );
    assert_eq!(prop(&space, &grace, chat_keys::TYPE), "personal");
    let news = chat_node(&space, "telegram", "1000000002");
    assert_eq!(
        news.body,
        "## 2026-03-06\n\n**Garden News** (12:00): #tomatoes go in <https://example.com/plot>\n"
    );
    assert_eq!(prop(&space, &news, chat_keys::TYPE), "channel");
    chat_node(&space, "telegram", "1000000003");

    let again = import_chat_export(&mut space, &path, None, &by()).unwrap();
    assert!(again.chats.is_empty(), "{again:?}");
    assert_eq!(again.unchanged, 3);
    assert_eq!(space.node_count().unwrap(), 3);
}

#[test]
fn refuses_an_export_of_another_kind() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let error = import_chat_export(
        &mut space,
        fixture("telegram-chat"),
        Some(ChatFormat::WhatsApp),
        &by(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("is not a WhatsApp export"),
        "{error}"
    );
    let broken = dir.path().join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("result.json"), "{\"messages\": [ {\"id\": }").unwrap();
    let error = import_chat_export(&mut space, &broken, None, &by()).unwrap_err();
    assert!(matches!(error, ChatImportError::Json { .. }), "{error}");
}

/// A synthetic Slack workspace export: two channels, a direct message, a thread, a file kept as
/// a link and one downloaded into `__uploads`.
fn slack_export() -> Vec<(String, Vec<u8>)> {
    let users = r#"[
 {"id": "U01", "team_id": "T01", "name": "ada", "real_name": "Ada", "profile": {"real_name": "Ada", "display_name": "ada"}},
 {"id": "U02", "team_id": "T01", "name": "grace", "real_name": "Grace", "profile": {"real_name": "Grace", "display_name": "grace"}},
 {"id": "U03", "team_id": "T01", "name": "linus", "real_name": "Linus", "profile": {"real_name": "Linus"}},
 {"id": "U04", "team_id": "T01", "name": "mira", "real_name": "Mira", "profile": {"real_name": "Mira"}}
]"#;
    let channels = r#"[
 {"id": "C01", "name": "general", "created": 1772600000, "members": ["U01", "U02", "U03"]},
 {"id": "C02", "name": "random", "created": 1772600000, "members": ["U01"]}
]"#;
    let dms = r#"[{"id": "D01", "created": 1772600000, "members": ["U01", "U02"]}]"#;
    let general_day1 = r#"[
 {"type": "message", "subtype": "channel_join", "user": "U03", "text": "<@U03> has joined the channel", "ts": "1772622000.000100"},
 {"type": "message", "user": "U01", "text": "Plan for *Friday*: see <https://example.com/plan|the plan> and <#C02|random>", "ts": "1772625600.000100", "thread_ts": "1772625600.000100", "reply_count": 2},
 {"type": "message", "user": "U02", "text": "Works for me <@U01>", "ts": "1772625660.000200", "thread_ts": "1772625600.000100", "parent_user_id": "U01"},
 {"type": "message", "user": "U02", "text": "Here is the map", "ts": "1772625720.000300",
  "files": [{"id": "F01", "name": "map.png", "title": "map", "mimetype": "image/png", "permalink": "https://example.slack.com/files/U02/F01/map.png"},
            {"id": "F02", "name": "notes.txt", "title": "notes", "mimetype": "text/plain", "permalink": "https://example.slack.com/files/U02/F02/notes.txt"}]}
]"#;
    let general_day2 = r#"[
 {"type": "message", "user": "U03", "text": "Late reply, sorry", "ts": "1772712000.000100", "thread_ts": "1772625600.000100", "parent_user_id": "U01"},
 {"type": "message", "user": "U03", "text": "Morning _all_", "ts": "1772712060.000200"}
]"#;
    let random_day = r#"[{"type": "message", "user": "U01", "text": "Cake in the kitchen", "ts": "1772625600.000500"}]"#;
    let dm_day =
        r#"[{"type": "message", "user": "U02", "text": "Lunch?", "ts": "1772625600.000600"}]"#;
    [
        ("users.json", users.as_bytes()),
        ("channels.json", channels.as_bytes()),
        ("dms.json", dms.as_bytes()),
        ("general/2026-03-04.json", general_day1.as_bytes()),
        ("general/2026-03-05.json", general_day2.as_bytes()),
        ("random/2026-03-04.json", random_day.as_bytes()),
        ("D01/2026-03-04.json", dm_day.as_bytes()),
        (
            "__uploads/F01/map.png",
            &b"\x89PNG\r\n\x1a\n synthetic map"[..],
        ),
    ]
    .into_iter()
    .map(|(n, b)| (n.to_string(), b.to_vec()))
    .collect()
}

fn zip_entries(out: &Path, entries: &[(String, Vec<u8>)]) {
    let borrowed: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    zip(out, &borrowed);
}

/// The time Slack's `ts` shows here, as the importer shows it: in this computer's time zone.
fn local(secs: i64, format: &str) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(secs, 0)
        .unwrap()
        .format(format)
        .to_string()
}

#[test]
fn imports_a_slack_workspace_export() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let zipped = dir.path().join("Example Slack export.zip");
    zip_entries(&zipped, &slack_export());
    assert_eq!(detect_chat_export(&zipped), Some(ChatFormat::Slack));

    let report = import_chat_export(&mut space, &zipped, None, &by()).unwrap();
    assert_eq!(report.format, ChatFormat::Slack);
    let titles: Vec<&str> = report.chats.iter().map(|c| c.title.as_str()).collect();
    let thread_title = format!(
        "Slack - general - Plan for Friday: see the plan and random ({})",
        local(1_772_625_600, "%Y-%m-%d %H:%M")
    );
    assert_eq!(
        titles,
        [
            "Slack - general",
            thread_title.as_str(),
            "Slack - random",
            "Slack - Ada, Grace"
        ]
    );
    // Only the members who wrote or are mentioned, not Mira.
    let people: Vec<&str> = report.people.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(people, ["Ada", "Grace", "Linus"]);
    assert_eq!(report.attached, 1);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    let general = chat_node(&space, "slack", "C01");
    let t = |secs, f| local(secs, f);
    let mut expected = format!(
        "## {}\n\n*({}) [[Linus]] has joined the channel*\n\n\
         **Ada** ({}): Plan for **Friday**: see [the plan](<https://example.com/plan>) and [[Slack - random]]\n\
         Thread: [[{thread_title}]]\n\n\
         **Grace** ({}): Here is the map\n![[map.png]]\n\
         [notes.txt](<https://example.slack.com/files/U02/F02/notes.txt>) *(not in the export)*\n\n",
        t(1_772_622_000, "%Y-%m-%d"),
        t(1_772_622_000, "%H:%M"),
        t(1_772_625_600, "%H:%M"),
        t(1_772_625_720, "%H:%M"),
    );
    expected.push_str(&format!(
        "## {}\n\n**Linus** ({}): Morning *all*\n",
        t(1_772_712_060, "%Y-%m-%d"),
        t(1_772_712_060, "%H:%M")
    ));
    if t(1_772_622_000, "%Y-%m-%d") != t(1_772_625_720, "%Y-%m-%d") {
        // Far from UTC the first day splits; the rest of the test still holds.
        eprintln!("time zone splits the first day; body not compared");
    } else {
        assert_eq!(general.body, expected);
    }
    assert_eq!(prop(&space, &general, chat_keys::TYPE), "channel");

    // The thread: its own node, part of the channel, with the reply from the next day.
    let thread = chat_node(&space, "slack", "C01/1772625600.000100");
    assert_eq!(thread.title, thread_title);
    assert_eq!(prop(&space, &thread, chat_keys::TYPE), "thread");
    assert!(thread.body.contains("**Grace** ("), "{}", thread.body);
    assert!(
        thread.body.contains("Works for me [[Ada]]"),
        "{}",
        thread.body
    );
    assert!(thread.body.contains("Late reply, sorry"), "{}", thread.body);
    assert!(!thread.body.contains("Thread: "), "{}", thread.body);
    let links = space.links_from(&thread.id).unwrap();
    assert!(
        links
            .iter()
            .any(|l| l.kind == link_kinds::PART_OF && l.to == general.id),
        "{links:?}"
    );

    // Members are person nodes; mentions link to them, and chats are related to who wrote.
    let ada = space
        .find_by_property(person_keys::USER, "T01/U01")
        .unwrap();
    assert_eq!(ada.len(), 1);
    let ada = space.get_node(&ada[0].id).unwrap().unwrap();
    assert_eq!(ada.kind, PERSON_KIND);
    let back = space.backlinks(&ada.id).unwrap();
    assert!(
        back.iter()
            .any(|l| l.kind == link_kinds::MENTIONS && l.from == thread.id),
        "{back:?}"
    );
    let from_general = space.links_from(&general.id).unwrap();
    let related: BTreeSet<NodeId> = from_general
        .iter()
        .filter(|l| l.kind == link_kinds::RELATED)
        .map(|l| l.to)
        .collect();
    assert_eq!(related.len(), 3, "Ada, Grace and Linus wrote in #general");

    let dm = chat_node(&space, "slack", "D01");
    assert_eq!(prop(&space, &dm, chat_keys::TYPE), "personal");

    // Again: nothing new, no copies of people.
    let nodes = space.node_count().unwrap();
    let again = import_chat_export(&mut space, &zipped, Some(ChatFormat::Slack), &by()).unwrap();
    assert!(again.chats.is_empty(), "{again:?}");
    assert!(again.people.is_empty());
    assert_eq!(again.unchanged, 4);
    assert_eq!(space.node_count().unwrap(), nodes);

    // A newer export, as a folder: one more reply in the thread and a message in #random.
    let mut entries = slack_export();
    for (name, bytes) in &mut entries {
        if name == "random/2026-03-04.json" {
            *bytes = br#"[{"type": "message", "user": "U01", "text": "Cake in the kitchen", "ts": "1772625600.000500"},
              {"type": "message", "user": "U04", "text": "Thanks <@U01>!", "ts": "1772625900.000100"}]"#
                .to_vec();
        }
    }
    entries.push((
        "general/2026-03-06.json".into(),
        br#"[{"type": "message", "user": "U02", "text": "Done", "ts": "1772798400.000100", "thread_ts": "1772625600.000100"}]"#
            .to_vec(),
    ));
    let folder = dir.path().join("newer");
    for (name, bytes) in &entries {
        let path = folder.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let report = import_chat_export(&mut space, &folder, None, &by()).unwrap();
    assert_eq!(report.messages, 2);
    let people: Vec<&str> = report.people.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(people, ["Mira"]);
    let thread = chat_node(&space, "slack", "C01/1772625600.000100");
    assert!(thread.body.trim_end().ends_with("Done"), "{}", thread.body);
    assert_eq!(prop(&space, &thread, chat_keys::MESSAGES), "4");
    let general_again = chat_node(&space, "slack", "C01");
    assert_eq!(general_again.body, general.body);
}

/// A WhatsApp export folder named as WhatsApp names it, with `text` as its `_chat.txt`.
fn whatsapp_folder(dir: &Path, name: &str, text: &str) -> PathBuf {
    let folder = dir.join(name).join(format!("WhatsApp Chat - {name}"));
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("_chat.txt"), text).unwrap();
    folder
}

#[test]
fn keeps_the_date_order_of_a_chat() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);

    // A United States export on a 24-hour clock with no day over 12: it reads either way. It
    // is read day first, the report says so, and the order is kept on the node.
    let first = whatsapp_folder(
        &dir.path().join("1"),
        "Porch",
        "3/4/26, 10:00 - Ada: a\n3/5/26, 10:00 - Grace: b\n",
    );
    let report = import_chat_export(&mut space, &first, None, &by()).unwrap();
    assert_eq!(report.messages, 2);
    assert_eq!(report.notes.len(), 1, "{report:?}");
    assert!(report.notes[0].contains("could be day/month or month/day"));
    let node = chat_node(&space, "whatsapp", "porch");
    assert_eq!(prop(&space, &node, chat_keys::DATE_ORDER), "day-month");

    // A later export that can only be month first: nothing is written, and the report says
    // why and what to do, instead of dropping its messages.
    let later = whatsapp_folder(
        &dir.path().join("2"),
        "Porch",
        "3/4/26, 10:00 - Ada: a\n3/5/26, 10:00 - Grace: b\n3/14/26, 10:00 - Ada: c\n",
    );
    let report = import_chat_export(&mut space, &later, None, &by()).unwrap();
    assert_eq!((report.messages, report.chats.len()), (0, 0));
    assert_eq!(report.skipped.len(), 1, "{report:?}");
    assert!(
        report.skipped[0]
            .reason
            .contains("writes its dates month/day")
    );
    assert!(
        report.skipped[0]
            .reason
            .contains("Delete the node \"WhatsApp - Porch\"")
    );
    assert_eq!(chat_node(&space, "whatsapp", "porch").body, node.body);

    // The order kept is used again where the new export alone would be read the other way.
    let day_first = whatsapp_folder(
        &dir.path().join("3"),
        "Shed",
        "13/03/26, 9:00 AM - Ada: a\n",
    );
    import_chat_export(&mut space, &day_first, None, &by()).unwrap();
    let shed = chat_node(&space, "whatsapp", "shed");
    assert_eq!(prop(&space, &shed, chat_keys::DATE_ORDER), "day-month");
    let truncated = whatsapp_folder(
        &dir.path().join("4"),
        "Shed",
        "01/04/26, 9:00 AM - Ada: b\n02/04/26, 9:00 AM - Grace: c\n",
    );
    let report = import_chat_export(&mut space, &truncated, None, &by()).unwrap();
    assert_eq!(report.messages, 2);
    assert!(report.notes.is_empty(), "{report:?}");
    let shed = chat_node(&space, "whatsapp", "shed");
    assert!(shed.body.contains("## 2026-04-01"), "{}", shed.body);
    assert!(shed.body.contains("## 2026-04-02"), "{}", shed.body);
}

#[test]
fn reports_what_is_not_added() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    // First an export of the last messages only, then one with older messages too.
    let recent = whatsapp_folder(
        &dir.path().join("1"),
        "Lab",
        "13/03/2026, 10:00 - Ada: c\n14/03/2026, 10:00 - Grace: d\n",
    );
    import_chat_export(&mut space, &recent, None, &by()).unwrap();
    let full = whatsapp_folder(
        &dir.path().join("2"),
        "Lab",
        "11/03/2026, 10:00 - Ada: a\n12/03/2026, 10:00 - Grace: b\n\
         13/03/2026, 10:00 - Ada: c\n14/03/2026, 10:00 - Grace: d\n\
         31/02/2026, 10:00 - Ada: not a date\n15/03/2026, 10:00 - Ada: e\n",
    );
    let report = import_chat_export(&mut space, &full, None, &by()).unwrap();
    assert_eq!(report.messages, 1, "{report:?}");
    let reasons: Vec<&str> = report.skipped.iter().map(|s| s.reason.as_str()).collect();
    assert_eq!(
        reasons,
        [
            "its date is not a date read day/month",
            "2 messages older than the last one imported were not added"
        ]
    );
}

#[test]
fn names_every_ios_chat_or_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let text = "[04/03/2026, 09:15:02] Ada: Hello\n";
    // Two iOS exports, each `_chat.txt` in a .zip named after its chat: two nodes.
    for name in ["Book Club", "Garden"] {
        let zipped = dir.path().join(format!("WhatsApp Chat - {name}.zip"));
        zip(&zipped, &[("_chat.txt", text.as_bytes())]);
        import_chat_export(&mut space, &zipped, None, &by()).unwrap();
    }
    chat_node(&space, "whatsapp", "book club");
    chat_node(&space, "whatsapp", "garden");

    // A loose `_chat.txt` in a folder WhatsApp did not name has no name: refused.
    let loose = dir.path().join("Downloads");
    fs::create_dir_all(&loose).unwrap();
    fs::write(loose.join("_chat.txt"), text).unwrap();
    let error = import_chat_export(&mut space, loose.join("_chat.txt"), None, &by()).unwrap_err();
    assert!(matches!(error, ChatImportError::NoChatName(_)), "{error}");
    assert!(
        error.to_string().contains("WhatsApp Chat - <name>.zip"),
        "{error}"
    );
    assert_eq!(space.node_count().unwrap(), 2);
}

#[test]
fn keeps_slack_replies_whose_thread_starts_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let users = br#"[{"id": "U01", "name": "ada", "real_name": "Ada"}, {"id": "U02", "name": "grace", "real_name": "Grace"}]"#;
    let channels = br#"[{"id": "C01", "name": "general", "members": ["U01", "U02"]}]"#;

    // An export that starts after a thread did: its replies are kept, and counted.
    let first = dir.path().join("first.zip");
    zip(
        &first,
        &[
            ("users.json", users),
            ("channels.json", channels),
            (
                "general/2026-03-05.json",
                br#"[{"type": "message", "user": "U02", "text": "Agreed", "ts": "1772712000.000100", "thread_ts": "1772625600.000100"},
                     {"type": "message", "user": "U01", "text": "Hello", "ts": "1772712060.000200"}]"#,
            ),
        ],
    );
    let report = import_chat_export(&mut space, &first, None, &by()).unwrap();
    assert_eq!(report.messages, 3, "{report:?}");
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("1 reply to threads that start before")),
        "{report:?}"
    );
    let general = chat_node(&space, "slack", "C01");
    let orphan = chat_node(&space, "slack", "C01/1772625600.000100");
    assert!(
        orphan.body.contains("is not in this export"),
        "{}",
        orphan.body
    );
    assert!(orphan.body.contains("**Grace** ("), "{}", orphan.body);
    let part_of = |space: &Space, thread: &Node| {
        space
            .links_from(&thread.id)
            .unwrap()
            .iter()
            .any(|l| l.kind == link_kinds::PART_OF && l.to == general.id)
    };
    assert!(part_of(&space, &orphan));

    // A later export where a message imported before gets its first reply: the thread's node
    // is new, and still linked to its channel.
    let later = dir.path().join("later.zip");
    zip(
        &later,
        &[
            ("users.json", users),
            ("channels.json", channels),
            (
                "general/2026-03-05.json",
                br#"[{"type": "message", "user": "U02", "text": "Agreed", "ts": "1772712000.000100", "thread_ts": "1772625600.000100"},
                     {"type": "message", "user": "U01", "text": "Hello", "ts": "1772712060.000200", "thread_ts": "1772712060.000200", "reply_count": 1}]"#,
            ),
            (
                "general/2026-03-06.json",
                br#"[{"type": "message", "user": "U02", "text": "Hi Ada", "ts": "1772798400.000100", "thread_ts": "1772712060.000200"}]"#,
            ),
        ],
    );
    let report = import_chat_export(&mut space, &later, None, &by()).unwrap();
    assert_eq!(report.chats.len(), 1, "{report:?}");
    let thread = chat_node(&space, "slack", "C01/1772712060.000200");
    assert!(thread.body.contains("Hi Ada"), "{}", thread.body);
    assert!(part_of(&space, &thread));
    assert_eq!(chat_node(&space, "slack", "C01").body, general.body);
}

/// A folder of five Markdown notes.
fn vault(root: &Path) {
    fs::create_dir_all(root).unwrap();
    for i in 1..=5 {
        fs::write(
            root.join(format!("Note {i}.md")),
            format!("# Note {i}\n\nBody {i}\n"),
        )
        .unwrap();
    }
}

fn write_file(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

const SLACK_USERS: &str = r#"[{"id":"U01","name":"ada","real_name":"Ada"}]"#;
const SLACK_CHANNELS: &str = r#"[{"id":"C01","name":"general"}]"#;
const TELEGRAM_CHAT: &str = r#"{"name":"x","type":"personal_chat","id":1,"messages":[{"id":1,"type":"message","date":"2026-03-04T09:00:00","from":"Ada","text":"hi"}]}"#;

#[test]
fn a_folder_of_notes_with_a_chat_export_inside_is_not_that_chat() {
    let cases: [(&str, &[(&str, &str)], ChatFormat); 3] = [
        (
            "whatsapp",
            &[(
                "Notes/Archive/WhatsApp Chat with Ada.txt",
                "3/4/26, 9:15 AM - Ada: Morning!\n3/4/26, 9:16 AM - Mira: Hi\n",
            )],
            ChatFormat::WhatsApp,
        ),
        (
            "telegram",
            &[("deep/x/result.json", TELEGRAM_CHAT)],
            ChatFormat::Telegram,
        ),
        (
            "slack",
            &[
                ("Projects/old/slack/channels.json", SLACK_CHANNELS),
                ("Projects/old/slack/users.json", SLACK_USERS),
            ],
            ChatFormat::Slack,
        ),
    ];
    for (name, files, format) in cases {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Vault");
        vault(&root);
        for (file, text) in files {
            write_file(&root, file, text);
        }
        assert_eq!(detect_chat_export(&root), None, "{name}");
        // Without the notes, an export further down is still not what was given.
        for i in 1..=5 {
            fs::remove_file(root.join(format!("Note {i}.md"))).unwrap();
        }
        assert_eq!(detect_chat_export(&root), None, "{name} without notes");
        // Named, the export is looked for at any depth.
        assert!(knode_ingest::is_chat_export(&root, format), "{name}");
    }
}

#[test]
fn an_export_is_told_at_the_top_or_in_the_one_folder_at_the_top() {
    let dir = tempfile::tempdir().unwrap();
    let chat = "[04/03/2026, 09:15:02] Ada: one\n[04/03/2026, 09:16:00] Grace: two\n";
    // A zip of the export's folder, as macOS makes it.
    let zipped = dir.path().join("WhatsApp Chat - Ada.zip");
    zip(
        &zipped,
        &[
            ("WhatsApp Chat - Ada/_chat.txt", chat.as_bytes()),
            ("__MACOSX/WhatsApp Chat - Ada/._chat.txt", b"x"),
        ],
    );
    assert_eq!(detect_chat_export(&zipped), Some(ChatFormat::WhatsApp));
    // Two folders at the top: neither is the export.
    let two = dir.path().join("two.zip");
    zip(
        &two,
        &[
            ("WhatsApp Chat - Ada/_chat.txt", chat.as_bytes()),
            ("Other/readme.txt", b"x"),
        ],
    );
    assert_eq!(detect_chat_export(&two), None);
    // Markdown notes beside the export: a folder of notes.
    let folder = dir.path().join("WhatsApp Chat - Grace");
    write_file(&folder, "_chat.txt", chat);
    assert_eq!(detect_chat_export(&folder), Some(ChatFormat::WhatsApp));
    write_file(&folder, "Plan.md", "# Plan\n");
    assert_eq!(detect_chat_export(&folder), None);
    assert!(knode_ingest::is_chat_export(&folder, ChatFormat::WhatsApp));
    // The chat's own file, given, is read whatever sits beside it.
    assert_eq!(
        detect_chat_export(folder.join("_chat.txt")),
        Some(ChatFormat::WhatsApp)
    );
}

/// Every title, body and property of every node in the space, as one text.
fn everything(space: &Space) -> String {
    let mut all = String::new();
    for summary in space.recent(10_000).unwrap() {
        let node = space.get_node(&summary.id).unwrap().unwrap();
        all.push_str(&node.title);
        all.push('\n');
        all.push_str(&node.body);
        for (key, value) in space.properties(&node.id).unwrap() {
            all.push_str(&format!("\n{key}={value}"));
        }
        all.push('\n');
    }
    all
}

// Every token below is made up for the test; none is a real credential. They are put together
// from parts, so the source holds no whole token.
const BOT_TOKEN: &str = concat!("xox", "b-1234567890-", "abcdefghijklmnop");
const FILE_TOKEN: &str = concat!("xox", "e-1-", "abcdefghijklmnopqrstuvwxyz0123456789");
const API_KEY: &str = concat!("sk-", "ant-api03-", "abcdefghijklmnopqrstuvwxyz");

#[test]
fn no_token_in_a_chat_reaches_a_node() {
    // Slack: in the text, and in a file's private URL.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("slack");
    write_file(&root, "users.json", SLACK_USERS);
    write_file(&root, "channels.json", SLACK_CHANNELS);
    write_file(
        &root,
        "general/2026-03-04.json",
        &format!(
            r#"[{{"type":"message","user":"U01","ts":"1772625600.000100","thread_ts":"1772625600.000100",
                "text":"the bot token is {BOT_TOKEN}",
                "files":[{{"id":"F01","name":"map.png","title":"map","mimetype":"image/png",
                          "url_private":"https://files.slack.com/files-pri/T01-F01/map.png?t={FILE_TOKEN}"}}]}},
               {{"type":"message","user":"U01","ts":"1772625660.000100","thread_ts":"1772625600.000100","text":"ok"}}]"#
        ),
    );
    let mut space = self::space(&dir);
    let report = import_chat_export(&mut space, &root, None, &by()).unwrap();
    let all = everything(&space);
    assert!(!all.contains("xoxb-") && !all.contains("xoxe-"), "{all}");
    assert!(
        all.contains("[map.png](<https://files.slack.com/files-pri/T01-F01/map.png>)"),
        "{all}"
    );
    assert!(all.contains("the bot token is [redacted]"), "{all}");
    assert!(
        report.notes.iter().any(|n| n.starts_with("1 secret ")),
        "{:?}",
        report.notes
    );

    // WhatsApp.
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("WhatsApp Chat - Ada");
    write_file(
        &folder,
        "_chat.txt",
        &format!("[04/03/2026, 09:15:02] Ada: key {API_KEY}\n[04/03/2026, 09:16:00] Grace: ok\n"),
    );
    let mut space = self::space(&dir);
    import_chat_export(&mut space, &folder, None, &by()).unwrap();
    let all = everything(&space);
    assert!(
        !all.contains(API_KEY) && all.contains("key [redacted]"),
        "{all}"
    );
    // Imported again, the redacted message is the same message.
    let again = import_chat_export(&mut space, &folder, None, &by()).unwrap();
    assert_eq!(again.messages, 0);

    // Telegram: in the text, its parts, a link and a reply's quote.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ChatExport");
    write_file(
        &root,
        "result.json",
        &format!(
            r#"{{"name":"Ada","type":"personal_chat","id":7,"messages":[
             {{"id":1,"type":"message","date":"2026-03-04T09:00:00","from":"Ada",
               "text":["use ",{{"type":"code","text":"{BOT_TOKEN}"}}],
               "text_entities":[{{"type":"plain","text":"use "}},{{"type":"code","text":"{BOT_TOKEN}"}}]}},
             {{"id":2,"type":"message","date":"2026-03-04T09:01:00","from":"Ada","reply_to_message_id":1,
               "text":"",
               "text_entities":[{{"type":"text_link","text":"here","href":"https://example.com/?token={API_KEY}"}}]}}]}}"#
        ),
    );
    let mut space = self::space(&dir);
    import_chat_export(&mut space, &root, None, &by()).unwrap();
    let all = everything(&space);
    assert!(!all.contains("xoxb-") && !all.contains(API_KEY), "{all}");
}

#[test]
fn whatsapp_android_french_date_with_a_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("WhatsApp Chat with Ada.txt");
    fs::write(
        &file,
        "04/03/2026 à 09:15 - Ada: Bonjour\n04/03/2026 à 09:16 - Mira: Salut\n",
    )
    .unwrap();
    assert_eq!(detect_chat_export(&file), Some(ChatFormat::WhatsApp));
    let mut space = space(&dir);
    let report = import_chat_export(&mut space, &file, Some(ChatFormat::WhatsApp), &by()).unwrap();
    assert_eq!(report.messages, 2);
    let node = chat_node(&space, "whatsapp", "ada");
    assert_eq!(
        node.body,
        "## 2026-03-04\n\n**Ada** (09:15): Bonjour\n\n**Mira** (09:16): Salut\n"
    );
}
