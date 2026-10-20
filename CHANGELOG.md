# Changelog

Changes people notice in Knode: the app, the `knode` command, the MCP server and the `.knode`
format. Every version follows [semantic versioning](https://semver.org), and the format of this file
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

No version is released yet. Everything below is on `main`, so the first release carries all of it.
It is grouped by the version of the [roadmap](https://knode.md/handbook/roadmap) each change
belongs to, newest first.

### v0.3 - Map and import (in progress)

#### Added

- **Map**: typed links with their own labels (`part of` and `has part`, `answers`, `contradicts`,
  or kinds you define), backlinks grouped by label, and in the Mac app a graph of the space with
  filters and saved views, and canvases where notes are placed, grouped and linked by hand.
  `knode links`, `knode link --kind` and `knode kinds` do the same from the command line, and the
  MCP tools `link_kinds` and `read_canvas` let agents use them.
- **Search by meaning**: a small multilingual model, downloaded once and checked, runs on your own
  machine; Knode Service indexes every space in the background and pauses on battery.
  `knode search` ranks by words and meaning together, with `--words` or `--meaning` for one of
  them, and `knode index` shows how far a space is indexed. The MCP `search` tool takes the same
  modes and says when it fell back to words and why. In the Mac app, the search field chooses
  words and meaning, words or meaning, and each result shows how it was found; Settings > Search
  and the menu bar show the model, how many notes are indexed and Index Now. It works on Linux
  too, arm64 included. `embeddings = "off"` in `config.toml`, or the switch in the app, turns it
  off.
- **Editing together**: you and your agents can edit the same note at once. Edits are merged
  instead of one overwriting the other, an agent's edit appears live in the open editor, and the
  history keeps who wrote what.
- **Attachments**: images, PDFs and other files kept inside the space. In the Mac app, drop or
  paste them into a note, open them from an Attachments inspector, and see images and PDFs in the
  preview. `knode attach` and `knode attachments` from the command line, and `attach_file` and
  `read_attachment` for agents. Markdown export writes them next to each note and import brings
  them back.
- **`knode import` for everything you bring in**: besides Markdown and agent sessions, it reads
  Claude and ChatGPT data exports (the `.zip` or its folder, one node per conversation, Claude
  projects as notes), PDFs, saved web pages and `http(s)` URLs (fetched only when you give one),
  Kindle clippings and notebooks, Readwise CSV and Markdown exports, and the notes of Apple Notes
  (its database, with Full Disk Access, or an export), Bear (backups, TextBundles or its
  database), Notion (the zipped export) and Evernote (`.enex`). Knode tells which from the path,
  `--from <kind>` says it, and `--json` prints the report. Importing again updates the same nodes
  instead of copying them. Gemini exports from Google Takeout are read too, but have not been
  checked against a real export yet; Perplexity has no export of all threads, so it is not
  supported; Readwise is read from its exports, not its API. The Mac app has no import screen
  yet.
- **The live Markdown folder works both ways**: edits made in the `.md` files come back into the
  space as edits of the folder's owner, new files become notes, renamed or moved files rename or
  move their note, and edits made while Knode was not running come in at start. A deleted file
  never deletes its note: the note is marked and kept, and the Mac app does not ask about it yet.

#### Changed

- **The `.knode` format, schema version 4**: link kinds, graph and canvas views, attachments,
  embeddings and the documents used for editing together. Spaces are upgraded when they are
  opened.
- **Markdown export** keeps the kind of each link and the attached files.

#### Fixed

- **Search by meaning on large spaces**: a search by meaning or hybrid search took minutes in a
  space of a few thousand notes (170 s on 2,700 notes and 20,000 chunks); it takes about 40 ms.
- **A deep `KNODE_HOME`**: when the Knode Service socket would be longer than a Unix socket path
  may be, commands and `knode service run` say so, with the length and the limit, and ask for a
  shorter folder, instead of only "could not start".

### v0.2 - Spaces and permissions

#### Added

- **Several spaces**: the Mac app lists every space like Apple Music libraries, with an icon, and
  switches with Command-1 to 9 or from the menu bar without restarting Knode Service. Rename a
  space, give it an emoji icon or make it the default from the app or with `knode spaces rename`,
  `icon` and `default`.
- **Folders that feed each space**: `knode folders` and a Folders and Capture screen in the Mac app
  choose which projects, notes folders and agent folders feed a space. Each conversation is
  captured into the space whose project folder matches it best, and capture can be turned off
  per folder or per space.
- **Permissions in the Mac app**: Settings > Agents lists every agent and sets No Access, Read or
  Read and Write for each space, and revokes keys.
- **History and undo**: every change is kept with who made it. `knode history`, `knode undo` and
  `knode restore` show changes, undo one or bring back an earlier version or a deleted note; an
  agent may only undo its own changes. In the Mac app, Command-Y shows the history of a note with a
  comparison, Restore This Version and Undo This Change.
- **Markdown out and back**: `knode export` writes a space as Markdown files with YAML front
  matter; `knode import` brings a folder back, keeping ids, dates and who wrote each note, and
  reads Obsidian vaults with their links, embeds and tags. Notes from a vault come back unchanged
  except for an added `id:` line.
- **Live Markdown folder**: `knode markdown folder <dir>` keeps a folder of `.md` files current
  with every note, for Obsidian, git or anything else (in v0.3 it also reads edits back).
- **More agents**: `knode connect` adds Knode to Cursor, Windsurf, Cline, Roo Code, Copilot CLI,
  OpenCode, Amp, VS Code and Zed (`knode connect --list` shows them); Aider has no MCP support, so
  it gets instructions instead. Capture also reads Cursor, Cline and Roo Code conversations and
  Windsurf chats; Windsurf's Cascade conversations are not captured yet, and the Windsurf, Cline
  and Roo Code formats have not been checked against real sessions.

#### Changed

- `knode spaces` shows each space's icon, which one is the default and where capture is off.

### v0.1 - You use it

The first release, v0.1: one memory that Claude Code and Codex share, a `knode` command, Knode
Service and a first Mac app.

#### Added

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
- **Agent keys and grants**: `knode connect` gives each agent its own key, kept in the Keychain
  (the Secret Service on Linux, or a file only you can read), and the first time write access to
  the default space only. `knode agents list`, `knode agents grant <agent> <space>
  none|read|write` and `knode agents revoke <agent>` change that. A space an agent may not see is
  invisible to it, and its changes carry the name of its key. Agents connected before keys stop
  until `knode connect` is run for them again.
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
