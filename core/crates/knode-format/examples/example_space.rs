//! Writes the example space of the `.knode` specification.
//!
//! ```sh
//! cargo run -p knode-format --example example_space -- path/to/Example.knode
//! ```

use knode_format::{
    NewNode, NodeUpdate, Properties, Provenance, Space, kinds, link_kinds, property_keys,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: example_space <path/to/Example.knode>")?;
    let mut space = Space::create(&path, "Example")?;

    let me = Provenance::person("Ada");
    let claude = Provenance::agent("claude-code")
        .with_session("3f1c9a52-6d0e-4b8e-9a51-2c7d1e0b4f6a")
        .with_source("~/Projects/garden-planner");
    let codex = Provenance::agent("codex")
        .with_session("rollout-2026-10-20-0915")
        .with_source("~/Projects/garden-planner");

    let project = space.create_node(
        NewNode::note(
            "Garden planner",
            "A small app to plan a vegetable garden by week.\n\n\
             - Data lives in [[SQLite for local data|SQLite]]\n\
             - Planting dates come from the [[Frost dates]]\n\
             - Open questions are in [[Ideas for v2]]\n",
        )
        .with_tags(["project"]),
        &me,
    )?;

    let decision = space.create_node(
        NewNode::note(
            "SQLite for local data",
            "## Decision\n\nKeep all data in one SQLite file, in WAL mode, so the app and the \
             sync helper can read while the other writes.\n\n## Why\n\n\
             - No server to run\n- Full-text search with FTS5\n- One file to back up\n\n\
             Discussed in [[Conversation: choosing the database]].\n",
        )
        .with_kind(kinds::DECISION)
        .with_tags(["architecture", "database"]),
        &me,
    )?;

    let conversation_properties = Properties::from(
        [
            (property_keys::CONVERSATION_AGENT, "claude-code"),
            (
                property_keys::CONVERSATION_SESSION,
                "3f1c9a52-6d0e-4b8e-9a51-2c7d1e0b4f6a",
            ),
            (
                property_keys::CONVERSATION_PROJECT,
                "~/Projects/garden-planner",
            ),
            (property_keys::CONVERSATION_BRANCH, "main"),
            (
                property_keys::CONVERSATION_STARTED_AT,
                "2026-10-20T09:02:11.000Z",
            ),
            (
                property_keys::CONVERSATION_ENDED_AT,
                "2026-10-20T09:08:45.000Z",
            ),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string())),
    );
    let conversation = space.create_node_with_properties(
        NewNode::note(
            "Conversation: choosing the database",
            "**Ada:** Should the garden planner use Postgres or SQLite?\n\n\
             **claude-code:** For a single-user app that runs on a laptop, SQLite. It needs no \
             server, and WAL mode lets one writer and many readers work at once. Postgres only \
             pays off with several users writing to a shared server.\n\n\
             **Ada:** And search?\n\n\
             **claude-code:** SQLite's FTS5 extension gives ranked full-text search with \
             snippets. See [[SQLite for local data]] for the decision we wrote down.\n",
        )
        .with_kind(kinds::CONVERSATION)
        .with_tags(["database"]),
        &conversation_properties,
        &claude,
    )?;

    let frost = space.create_node(
        NewNode::note(
            "Frost dates",
            "| Region | Last spring frost | First autumn frost |\n\
             |---|---|---|\n\
             | Madrid | 15 April | 10 November |\n\
             | Oslo | 15 May | 25 September |\n\n\
             Source: national weather services. Used by [[Garden planner]].\n",
        )
        .with_kind(kinds::SOURCE)
        .with_tags(["reference"]),
        &codex,
    )?;

    let ideas = space.create_node(
        NewNode::note(
            "Ideas for v2",
            "- [ ] Reminders a week before each sowing date\n\
             - [ ] Share a plan with a neighbour\n\
             - [x] Show the [[Frost dates]] for the chosen region\n\
             - [ ] Companion planting tips, see [[Companion planting]]\n",
        )
        .with_kind(kinds::IDEA),
        &me,
    )?;

    // Typed links made by hand.
    space.link(
        &decision.id,
        &conversation.id,
        link_kinds::DERIVED_FROM,
        &claude,
    )?;
    space.link(&ideas.id, &project.id, link_kinds::PART_OF, &me)?;
    space.link(&frost.id, &project.id, link_kinds::PART_OF, &codex)?;

    // A later edit, so the history has more than one version.
    space.update_node(
        &ideas.id,
        NodeUpdate {
            body: Some(
                "- [ ] Reminders a week before each sowing date\n\
                 - [ ] Share a plan with a neighbour\n\
                 - [x] Show the [[Frost dates]] for the chosen region\n\
                 - [ ] Companion planting tips, see [[Companion planting]]\n\
                 - [ ] Export the plan as a calendar\n"
                    .into(),
            ),
            ..Default::default()
        },
        &codex,
    )?;

    println!(
        "Wrote {} with {} nodes.",
        space.path().display(),
        space.node_count()?
    );
    Ok(())
}
