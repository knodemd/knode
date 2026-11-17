//! Importing chat exports. The fixtures in `fixtures/chats` are synthetic: invented chats
//! between invented people, written by hand in the structure of each export.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use knode_format::{ChangeOp, Node, Provenance, Space};
use knode_ingest::{
    CHAT_KIND, ChatFormat, ChatImportError, chat_keys, detect_chat_export, import_chat_export,
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
    let families: [(&str, &str); 8] = [
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
            .contains("is not a WhatsApp or Telegram Desktop chat export")
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
