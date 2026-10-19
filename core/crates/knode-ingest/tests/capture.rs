use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use knode_ingest::{
    Agent, Error, Part, ProjectFilter, Role, SessionReader, TitleSource, discover, read_session,
    to_markdown,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn claude_root() -> PathBuf {
    fixtures().join("claude/projects")
}

fn codex_root() -> PathBuf {
    fixtures().join("codex/sessions")
}

const CLAUDE_SESSION: &str = "0f8e2c1a-1111-4222-8333-444455556666";
const CODEX_SESSION: &str = "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee";

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("knode-ingest");
    fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn discovers_claude_code_sessions_but_not_subagents() {
    let files = discover(Agent::ClaudeCode, &claude_root()).unwrap();
    let ids: Vec<&str> = files.iter().map(|f| f.session_id.as_str()).collect();
    assert_eq!(
        ids,
        ["7a7a7a7a-1111-4222-8333-444455556666", CLAUDE_SESSION]
    );
    assert!(
        files
            .iter()
            .all(|f| f.agent == Agent::ClaudeCode && f.len > 0)
    );
}

#[test]
fn discovers_codex_sessions_by_date_folder() {
    let files = discover(Agent::Codex, &codex_root()).unwrap();
    let ids: Vec<&str> = files.iter().map(|f| f.session_id.as_str()).collect();
    assert_eq!(ids, ["0195aaaa-bbbb-7ccc-8ddd-ffffffffffff", CODEX_SESSION]);
}

#[test]
fn a_missing_root_has_no_sessions() {
    let files = discover(Agent::Codex, &fixtures().join("does-not-exist")).unwrap();
    assert!(files.is_empty());
}

#[test]
fn reads_a_claude_code_session() {
    let file = discover(Agent::ClaudeCode, &claude_root())
        .unwrap()
        .into_iter()
        .find(|f| f.session_id == CLAUDE_SESSION)
        .unwrap();
    let conversation = read_session(&file).unwrap();

    assert_eq!(conversation.session_id, CLAUDE_SESSION);
    assert_eq!(
        conversation.project.as_deref(),
        Some(Path::new("/Users/example/project"))
    );
    assert_eq!(conversation.git_branch.as_deref(), Some("main"));
    assert_eq!(conversation.model.as_deref(), Some("claude-example-2"));
    assert_eq!(
        conversation.started_at.unwrap().to_rfc3339(),
        "2026-01-02T10:00:00+00:00"
    );
    assert_eq!(
        conversation.ended_at.unwrap().to_rfc3339(),
        "2026-01-02T10:02:03+00:00"
    );
    let title = conversation.recorded_title.as_ref().unwrap();
    assert_eq!(title.text, "Server health check");
    assert_eq!(title.source, TitleSource::User);

    let roles: Vec<Role> = conversation.messages.iter().map(|m| m.role).collect();
    use Role::{Assistant, User};
    assert_eq!(roles, [User, Assistant, User, Assistant, User, Assistant]);

    let first_reply = &conversation.messages[1];
    let tools: Vec<(&str, &str, bool)> = first_reply
        .tool_calls()
        .map(|c| (c.name.as_str(), c.summary.as_str(), c.failed))
        .collect();
    assert_eq!(
        tools,
        [
            ("Bash", "ls src", false),
            ("Edit", "/Users/example/project/src/server.rs", true),
            ("mcp__example__search", "health check", false),
        ]
    );
    assert_eq!(conversation.messages[2].parts.last(), Some(&Part::Image));
    assert_eq!(conversation.messages[4].text(), "/review 12");

    let markdown = to_markdown(&conversation);
    assert_eq!(
        markdown,
        include_str!("fixtures/claude/expected.md"),
        "rendered:\n{markdown}"
    );
}

#[test]
fn reads_a_codex_session() {
    let file = discover(Agent::Codex, &codex_root())
        .unwrap()
        .into_iter()
        .find(|f| f.session_id == CODEX_SESSION)
        .unwrap();
    let conversation = read_session(&file).unwrap();

    assert_eq!(conversation.session_id, CODEX_SESSION);
    assert_eq!(
        conversation.project.as_deref(),
        Some(Path::new("/Users/example/project"))
    );
    assert_eq!(conversation.git_branch.as_deref(), Some("feat/build"));
    assert_eq!(conversation.model.as_deref(), Some("gpt-example"));
    assert_eq!(conversation.recorded_title, None);
    assert_eq!(conversation.title(), "Why does the build fail?");
    assert_eq!(
        conversation.ended_at.unwrap().to_rfc3339(),
        "2026-01-02T11:01:06+00:00"
    );

    let markdown = to_markdown(&conversation);
    assert_eq!(
        markdown,
        include_str!("fixtures/codex/expected.md"),
        "rendered:\n{markdown}"
    );
}

#[test]
fn reads_older_codex_files_without_the_payload_wrapper() {
    let file = discover(Agent::Codex, &codex_root()).unwrap().remove(0);
    let conversation = read_session(&file).unwrap();
    assert_eq!(
        conversation.session_id,
        "0195aaaa-bbbb-7ccc-8ddd-ffffffffffff"
    );
    assert_eq!(conversation.messages.len(), 2);
    let call = conversation.messages[1].tool_calls().next().unwrap();
    assert_eq!(
        (call.name.as_str(), call.summary.as_str()),
        ("shell", "bash -lc ls")
    );
    assert_eq!(conversation.messages[1].text(), "Two files.");
}

#[test]
fn never_copies_tool_output_reasoning_or_injected_context() {
    for (agent, root) in [
        (Agent::ClaudeCode, claude_root()),
        (Agent::Codex, codex_root()),
    ] {
        for file in discover(agent, &root).unwrap() {
            let markdown = to_markdown(&read_session(&file).unwrap());
            for hidden in [
                "TOOL OUTPUT",
                "private reasoning",
                "SIDECHAIN",
                "injected",
                "LOCAL OUTPUT",
                "No response requested",
                "AGENTS CONTENT",
                "environment_context",
                "BASE INSTRUCTIONS",
                "ENCRYPTED",
                "DEV",
                "iVBOR",
            ] {
                assert!(!markdown.contains(hidden), "{hidden} leaked from {file:?}");
            }
        }
    }
}

#[test]
fn leaves_out_excluded_projects() {
    let filter = ProjectFilter::excluding(["/Users/example/private"]);
    let files = discover(Agent::ClaudeCode, &claude_root()).unwrap();
    let kept: Vec<&str> = files
        .iter()
        .filter(|f| filter.may_allow_file(f))
        .map(|f| f.session_id.as_str())
        .collect();
    assert_eq!(kept, [CLAUDE_SESSION]);

    let private = read_session(&files[0]).unwrap();
    assert!(!filter.allows(&private));
    let nested = ProjectFilter::excluding(["/Users/example"]);
    let project = read_session(&files[1]).unwrap();
    assert!(!nested.allows(&project));
    assert!(ProjectFilter::new().allows(&project));
}

/// Writes the fixture in pieces, cutting lines in half, and checks that reading in steps gives
/// the same conversation as reading it all at once.
fn reads_incrementally(agent: Agent, fixture: PathBuf, name: &str) {
    let content = fs::read(&fixture).unwrap();
    let whole = SessionReader::open(agent, &fixture, "id");
    let mut whole = whole;
    whole.poll().unwrap();

    let path = scratch(name);
    fs::write(&path, b"").unwrap();
    let mut reader = SessionReader::open(agent, &path, "id");
    let mut written = 0;
    for chunk in content.chunks(97) {
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(chunk).unwrap();
        written += chunk.len();
        let progress = reader.poll().unwrap();
        assert!(progress.offset <= written as u64);
        assert_eq!(progress.offset, reader.offset());

        // Stop and resume from the saved state, as Knode Service would after a restart.
        let saved = serde_json::to_string(reader.conversation()).unwrap();
        reader = SessionReader::resume(
            &path,
            serde_json::from_str(&saved).unwrap(),
            reader.offset(),
        );
    }
    assert_eq!(reader.offset(), content.len() as u64);
    assert_eq!(reader.conversation(), whole.conversation());
}

#[test]
fn reads_a_claude_code_session_while_it_is_written() {
    let fixture = claude_root().join(format!("-Users-example-project/{CLAUDE_SESSION}.jsonl"));
    reads_incrementally(Agent::ClaudeCode, fixture, "claude-incremental.jsonl");
}

#[test]
fn reads_a_codex_session_while_it_is_written() {
    let fixture = codex_root().join(format!(
        "2026/01/02/rollout-2026-01-02T11-00-00-{CODEX_SESSION}.jsonl"
    ));
    reads_incrementally(Agent::Codex, fixture, "codex-incremental.jsonl");
}

#[test]
fn waits_for_an_unfinished_last_line() {
    let path = scratch("partial.jsonl");
    let first = r#"{"type":"user","timestamp":"2026-01-02T10:00:00Z","message":{"role":"user","content":"Hello"}}"#;
    let second = r#"{"type":"assistant","timestamp":"2026-01-02T10:00:01Z","message":{"role":"assistant","model":"m","content":[{"type":"text","text":"Hi"}]}}"#;
    let (head, tail) = second.split_at(40);
    fs::write(&path, format!("{first}\n{head}")).unwrap();

    let mut reader = SessionReader::open(Agent::ClaudeCode, &path, "id");
    let progress = reader.poll().unwrap();
    assert_eq!(
        (progress.records, progress.skipped, progress.new_messages),
        (1, 0, 1)
    );
    assert_eq!(progress.offset, first.len() as u64 + 1);

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(tail.as_bytes()).unwrap();
    let progress = reader.poll().unwrap();
    assert_eq!((progress.records, progress.new_messages), (1, 1));
    assert_eq!(reader.conversation().messages[1].text(), "Hi");

    // The newline arrives later and changes nothing.
    file.write_all(b"\n").unwrap();
    assert_eq!(reader.poll().unwrap().records, 0);
    assert_eq!(reader.offset(), fs::metadata(&path).unwrap().len());
}

#[test]
fn reports_a_file_that_was_cut() {
    let path = scratch("cut.jsonl");
    fs::write(&path, "{}\n{}\n").unwrap();
    let mut reader = SessionReader::open(Agent::Codex, &path, "id");
    reader.poll().unwrap();
    fs::write(&path, "{}\n").unwrap();
    assert!(matches!(
        reader.poll(),
        Err(Error::Truncated { offset: 6, len: 3 })
    ));
}
