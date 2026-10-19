# Changelog

Changes people notice in Knode: the app, the `knode` command, the MCP server and the `.knode`
format. Every version follows [semantic versioning](https://semver.org), and the format of this file
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

The first release, v0.1: one memory that Claude Code and Codex share, a `knode` command, Knode
Service and a first Mac app.

### Added

- **The `.knode` format, version 0** ([specification](https://github.com/knodemd/format)): a space is a
  package with `manifest.json`, `knode.db` (SQLite), `blobs/` and `sync/`. Every node is Markdown;
  `[[links]]` are stored by id, so renaming a node never breaks a link. Every change is recorded
  with who made it and when, and can be undone or restored.
- **Full-text search** in every space, safe for anything typed into it.
- **The `knode` command**:
  - `knode init <name>` creates a space in `~/Knode` and makes it the default.
  - `knode search`, and `knode import` for folders of Markdown and Claude Code or Codex sessions.
  - `knode connect claude-code` (also `codex`, `gemini` and `claude-desktop`) gives an agent this
    memory through MCP, editing its settings safely and only once.
  - `knode spaces` lists your spaces.
  - `knode service run`, `start`, `stop`, `status`, `install` and `uninstall` run Knode Service
    and start it at login, with a launch agent on macOS or a systemd user unit on Linux.
- **The MCP server** (`knode mcp`) with the tools `search`, `read_node`, `write_node`,
  `link_nodes` and `recent`. Writes are recorded as the agent that made them.
- **Knode Service**: the one process that opens spaces. The app, the CLI and every agent write
  through it at once and see each other's changes live. `knode mcp`, `knode search`, `knode import`
  and `knode init` start it in the background when it is not running.
- **Capture**: every Claude Code and Codex conversation becomes a node in the default space and
  stays current while the agent writes. Gemini CLI, Copilot CLI, OpenCode, Amp and Aider sessions
  are read too. `capture = false` in `config.toml` turns it off, and
  `capture_exclude = ["/path/to/project"]` leaves projects out.
- **The Mac app** (macOS 26 or later): open, create and switch between spaces, a list of notes
  newest first, a Markdown editor with live preview where `[[links]]` open the note they name,
  search, and recent spaces. A menu bar item shows Knode Service and starts or stops it, and the app
  offers to install the `knode` command.
- **Builds for macOS and Linux**: Apple silicon, Intel and a universal binary, and Linux x86_64
  and arm64, with checksums. On Linux, `curl -fsSL https://knode.md/install.sh | sh` installs it.
