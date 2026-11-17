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

- **Let your agent bring your notes in**: Get started offers a prompt to paste into Claude Code, Codex or
  any agent that runs commands. The agent follows the guide Knode ships (`knode onboard guide`),
  asks you only what needs your judgment, imports a copy, connects itself and moves your
  instruction files to Knode. `knode onboard prompt` prints the prompt.
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
- **Mail import**: `knode import --from mbox` reads Gmail through a Google Takeout `.mbox` (or any
  other `mbox` file), and `knode import --from apple-mail` reads the Apple Mail store on the Mac,
  which needs Full Disk Access (Knode says how to allow it when it is missing). Each thread
  becomes one node with its messages in order, sender, recipients and date, the text part
  preferred and HTML turned into Markdown; everyone in a thread becomes a person node, one per
  address, linked from it. `--label`/`--mailbox` and `--since`/`--until` import only some
  folders and dates, spam and trash are left out, and `--attachments` attaches the files (off by
  default). Importing again, or another date range, adds only the new messages, and never
  overwrites what you changed or added to a thread in Knode. Knode never signs
  in to a mail account: it reads only the files you point it at. Knode Service does the same with
  `import.run`.
- **Chat exports in `knode import`**: WhatsApp chat exports (the `.zip`, its folder or the
  chat's `.txt`, from iOS or Android, in the date formats WhatsApp writes in each language)
  become one `chat` node per
  chat, with the messages grouped by day, the sender and time of each, and the photos, voice
  notes and documents of the export attached. Media the export left out or does not have are
  marked in the text. Importing the same chat again, or a newer export of it, adds only the new
  messages. When a chat's dates could be day or month first, the summary says how they were
  read, and later imports of the chat read them the same way. Telegram Desktop exports in JSON
  (one chat, or the whole account with every chat in it) are read the same way, with bold,
  italic, code, links and quotes kept as Markdown and replies naming the message they answer.
  Slack workspace exports become one node per channel and direct message, each thread its own
  node linked to its channel, and the members who wrote or were mentioned `person` nodes that
  mentions link to; files are kept as links, or attached when the export includes them.
  `--from whatsapp`, `--from telegram` or `--from slack` says what it is; Knode Service imports
  them through `import.run`.
- **The live Markdown folder works both ways**: edits made in the `.md` files come back into the
  space as edits of the folder's owner, new files become notes, renamed or moved files rename or
  move their note, and edits made while Knode was not running come in at start. A deleted file
  never deletes its note: the note is marked and kept, and the Mac app does not ask about it yet.
- **A project folder adds its project**: adding a project folder, in the Mac app or with
  `knode folders add`, adds the project of the same name to the space, or uses the one of that
  name it already has (upper and lower case and accent encoding aside). `[`, `]` and `|` in the
  folder's name become `(`, `)` and `-`. The folder's row and `knode folders list` name it, and
  removing the folder keeps the project and its notes. A folder added before this gets its
  project when it is added again.

#### Changed

- **Guided import report**: links to documents you left out in review are counted apart as
  expected, instead of listed with the links that found nothing.
- **Guided import names the new space after its source folder** (`Proyectos.knode`) instead of
  "Imported memory" for every import; the name stays editable.
- **The `.knode` format, schema version 4**: link kinds, graph and canvas views, attachments,
  embeddings and the documents used for editing together. Spaces are upgraded when they are
  opened.
- **Markdown export** keeps the kind of each link and the attached files.
- **Installing the Mac app**: opened from its disk image, Downloads or any other folder, Knode offers
  to move itself to Applications. When the `knode` command points to another copy of Knode, the app
  names it and offers to point the command to itself, so agents do not keep running an old copy.
- **Guided import of a vault that holds its repositories**: documents tracked by git in those
  repositories (`README`, `CHANGELOG`, `AGENTS.md` and the like) are proposed unchecked, since they
  live with their code and a copy would not follow their changes. Your own notes, including
  untracked ones inside a repository, stay checked, and you can check any document again.

#### Fixed

- **Archived copies** (`knode onboard archive`) are listed as archived and last by `knode spaces`,
  skipped when a space is named, and no longer appear in the Mac sidebar, also when archived from
  the command line.
- **Spaces made outside the Mac app** (with `knode init`, `knode onboard apply` or by an agent),
  including the default space, now appear in the sidebar the next time the app opens, unless you
  took them off the list.
- **Obsidian links with a relative path**: `[[../Research/Insights]]` and `[[./Glossary]]`, as Obsidian
  writes them with "relative path to file", now link to the file they name, even when it is
  imported after the note.
- **Obsidian links inside tables**: `[[Projects/web/README\|web]]`, where Obsidian escapes the
  label's `|` in a table cell, now links to its note instead of staying unresolved, and an embed
  written that way finds its attachment on import.
- **Importing one Markdown file** reads its frontmatter as properties instead of text, takes its
  `title` when it has one, and updates the same note when imported again instead of adding a copy.
- **Search imported notes by file name**: words and hybrid search also index `markdown.path`,
  so `DEPLOY.md` finds a note imported from `Projects/DEPLOY.md` even when its title and text
  use other words. Existing spaces rebuild their search index on opening (schema version 5).
- **Obsidian links written as paths**: `[[Work/Projects/Plan]]`, `[[Projects/Plan.md]]` and other
  endings of a file's path now link to the note imported from that file, as in Obsidian, so
  vaults with several notes of the same name, such as a `README` in each folder, keep their
  links. Titles are tried first, as before.
- **Search by meaning on large spaces**: a search by meaning or hybrid search took minutes in a
  space of a few thousand notes (170 s on 2,700 notes and 20,000 chunks); it takes about 40 ms.
- **A deep `KNODE_HOME`**: when the Knode Service socket would be longer than a Unix socket path
  may be, commands and `knode service run` say so, with the length and the limit, and ask for a
  shorter folder, instead of only "could not start".

### v0.2 - Spaces and permissions

#### Added

- **Connected clients and spaces**: Knode Service tracks the last space successfully used by
  each authenticated connection and removes it on disconnect. `knode service status` shows
  those connections to the person; agents never receive the connection inventory.
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
  it gets instructions instead. Capture also reads Cursor conversations (chats and agent
  sessions, in the layouts of current and older Cursor versions), Cline tasks (from the VS Code
  family extension, and the sessions of the Cline CLI and of Cline's SDK in `~/.cline/data`) and
  Roo Code tasks. Windsurf's Cascade conversations are not captured: Windsurf keeps them
  encrypted, with no export to read them from.

#### Changed

- `knode spaces` shows each space's icon, which one is the default and where capture is off.
- **The install script** takes `--prefix <dir>` to put `knode` in `<dir>/bin` instead of
  `~/.local/bin` (`curl -fsSL https://knode.md/install.sh | sh -s -- --prefix /opt/knode`), on
  a Mac it says that Homebrew and the Mac app also install `knode`, and it says plainly when no
  release is published yet.

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
