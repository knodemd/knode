# Knode Service protocol

Knode Service is the one process that opens `.knode` spaces. The Knode app, the `knode` CLI, the
MCP server and capture all talk to it, so a person and several agents can write to the same space
at the same time and everyone sees each change as it happens.

This document describes the protocol version that ships with Knode v0.2. Rust programs can use
`knode_service::Client` instead of speaking it by hand.

## Transport

- **Socket:** a Unix domain socket at `$KNODE_HOME/service.sock`, readable and writable only by its
  owner (mode `0600`).
- **Knode home:** the `KNODE_HOME` environment variable; else `~/Library/Application Support/Knode`
  on macOS; else `$XDG_DATA_HOME/knode` (by default `~/.local/share/knode`).
- **Framing:** [JSON-RPC 2.0](https://www.jsonrpc.org/specification), one JSON message per line,
  UTF-8, each line ended by `\n`. Blank lines are ignored. A line may be at most 16 MiB; a longer
  one closes the connection.
- **Order:** the requests of one connection run one after another, in the order they arrive, and
  their answers come back in the same order. Open more connections to run requests side by side.
- **Batches** (JSON arrays) are not supported: send one request per line.
- **Notifications from the client** (requests without an `id`) run, but get no answer.

## Authentication

Every connection authenticates first, with `auth`, and only once. Until it does, every other
method fails with `unauthenticated`. A connection acts either for the person who owns the Knode
home or for one agent:

- **The person** (the Knode app, the `knode` CLI) sends the owner token: the text of
  `$KNODE_HOME/owner.token`, a file of mode `0600` that the service makes the first time it
  starts. The person is not limited by grants.
- **An agent** (such as `knode mcp`, run by Claude Code) sends its key, `knk_` and 64 lower case
  hexadecimal digits. `knode connect <agent>` makes the key and keeps it in the machine's
  credential store; the service only knows its fingerprint. An unknown or revoked key is
  refused.

```json
→ {"jsonrpc":"2.0","id":1,"method":"auth","params":{"owner":"5f0c…"}}
← {"jsonrpc":"2.0","id":1,"result":{"as":"person"}}
→ {"jsonrpc":"2.0","id":1,"method":"auth","params":{"key":"knk_…"}}
← {"jsonrpc":"2.0","id":1,"result":{"as":"agent","id":"01K83N6V4W5D2XQJ0Y7RZ9T1AB","name":"claude-code","kind":"claude-code"}}
```

For an agent, every call is checked, not just the connection, so revoking a key or lowering a
grant takes effect on the next call:

- Its key must still be valid on this machine, and listed for the agent and not revoked in the
  space the call names. Otherwise its level in that space is `none`.
- Its level in the space is `none`, `read` or `write` (section 16 of the
  [.knode specification](https://github.com/knodemd/format)).
- With `none` the space is **invisible**: a call that names it fails exactly as for a space that
  does not exist (`not_a_space`, with the same message), it is left out of `service.status`, and
  its changes are not sent to the agent.
- With `read`, `space.open`, `settings.get`, `folders.list`, `node.get`, `node.find_by_title`, `links.from`, `links.back`,
  `links.unresolved`, `search`, `recent`, `history`, `changes`, `change`, `vault.export`,
  `markdown.folder.get` and `subscribe` are allowed; writes fail with `forbidden`.
- With `write`, `node.create`, `node.update`, `node.delete`, `link.create`, `link.delete`,
  `undo`, `restore` and `vault.import` are allowed too. An agent may only `undo` a change made by
  the agent its key belongs to, and never with `force`; any other undo fails with `forbidden`.
- Creating and renaming spaces, changing settings and folders, managing agents, keys and grants,
  `purge`, `markdown.folder.set` and every other method are for people only and fail with
  `forbidden`.
- An agent's changes are recorded with `kind` `agent` and, as `actor`, the name of the agent its
  key belongs to, whatever `by` says. `session` and `source` are kept.

Each call an agent makes is written to the access log of the machine, `$KNODE_HOME/access.log`:
one JSON line with the time (`at`, ms), the agent's id, the space's id, the method and the
outcome (`allowed`, `denied` or `invisible`). It never holds keys, node content or search
queries.

## One writer

Only Knode Service opens a space. Inside it, each open space is used by one request at a time:
writes to a space never interleave, and every write is one SQLite transaction. Reads see every
write that answered before them.

Only one service runs per Knode home. The running service holds an exclusive lock on
`$KNODE_HOME/service.lock`, which also holds its process id. A second service fails to start
with "Knode Service is already running". The lock is released when the process ends, however it
ends, and a socket file left by a service that crashed is removed by the next one.

The service stops cleanly on `SIGTERM` or `SIGINT`: it stops accepting connections, removes the
socket, lets writes in progress finish, closes the spaces and releases the lock.

## Requests

Every method that works on a space takes `space`: the **absolute** path of the `.knode` package.
The service opens a space the first time it is named and keeps it open. Paths are compared after
resolving symbolic links; the path the service uses is returned by `space.open` and appears in
`changed` notifications.

Every write takes `by`, the provenance of the change:

```json
{ "kind": "person", "actor": "Ada" }
{ "kind": "agent", "actor": "claude-code", "session": "8f14e45f", "source": "~/code/app" }
```

`kind` is `person`, `agent` or `system`; `session` and `source` are optional.

Node ids are [ULIDs](https://github.com/ulid/spec). Timestamps are milliseconds since the Unix
epoch, in UTC. The shapes of nodes, links, history entries and search hits are those of the
[.knode specification](https://github.com/knodemd/format).

### Example

```json
→ {"jsonrpc":"2.0","id":1,"method":"node.create","params":{"space":"/Users/ada/Knode/Personal.knode","title":"Plan","body":"Ship v0.1.","by":{"kind":"person","actor":"Ada"}}}
← {"jsonrpc":"2.0","id":1,"result":{"id":"01K83N6V4W5D2XQJ0Y7RZ9T1AB","kind":"note","title":"Plan","body":"Ship v0.1.","tags":[],"created_at":1792489800123,"updated_at":1792489800123,"created_by":{"kind":"person","actor":"Ada"},"updated_by":{"kind":"person","actor":"Ada"}}}
```

### Methods

| Method | Params | Result |
| --- | --- | --- |
| `space.create` | `space`, `name` | Space info. `space` must end in `.knode` and must not exist. |
| `space.open` | `space` | Space info. |
| `space.rename` | `space`, `name`, `by` | Space info, with the new name. Changes `name` in `manifest.json`; the package keeps its folder name. People only. |
| `settings.get` | `space` | Every setting of the space, by key: `{ "space.id": "01K8…", "space.icon": "🌱" }`. A key left out has its default. |
| `settings.set` | `space`, `key`, `value` (`null` for the default), `by` | Every setting, as `settings.get`. `space.icon` is one emoji; `capture.enabled` is `on` or `off`, and `on` is stored as no value. Other keys fail with `invalid`; the live Markdown folder is changed with `markdown.folder.set`. People only. |
| `folders.list` | `space` | The folders that feed the space, oldest first. See below. |
| `folders.set` | `space`, `kind`, `path`, `capture`? (default `true`), `rules`? (default `{}`), `id`?, `by` | The folder. Adds it, or changes the folder with this `id`, or with the same kind and path. `kind` is `project`, `notes` or `agent`; `path` is absolute or starts with `~/`, and may not be inside the space. People only. |
| `folders.remove` | `space`, `id`, `by` | `true` if there was such a folder. Nothing captured or imported from it is removed. People only. |
| `node.create` | `space`, `title`, `body`?, `kind`? (default `note`), `tags`?, `by` | The node. |
| `node.get` | `space`, `id` | The node, or `null` when no node has this id. |
| `node.find_by_title` | `space`, `title` | Node summaries with this title, oldest first. Titles are compared without regard to case or extra spaces; a `[[link]]` with this title points at the first one. |
| `node.update` | `space`, `id`, `title`?, `body`?, `kind`?, `tags`?, `by` | The node as it is now. Fields left out stay as they are. |
| `node.delete` | `space`, `id`, `by` | `null`. |
| `link.create` | `space`, `from`, `to`, `kind`? (default `related`), `by` | The link. |
| `link.delete` | `space`, `from`, `to`, `kind`? (default `related`), `by`? | `true` if there was such a link. `by` is recorded as who removed it; left out (by clients older than v0.2), the change is recorded as Knode's own, `system`. |
| `links.from` | `space`, `id` | The links that start from the node. |
| `links.back` | `space`, `id` | The links that point at the node. |
| `links.unresolved` | `space`, `id` | The `[[links]]` in the node's text that name no node yet, as written. |
| `search` | `space`, `query`, `limit`? (default 20, at most 1000) | Search hits, best first. |
| `recent` | `space`, `limit`? (default 20, at most 1000) | Node summaries, newest change first. |
| `history` | `space`, `id` | Every version of the node, oldest first. |
| `changes` | `space`, `subject`?, `limit`? (default 20, at most 1000), `before`? | Changes, newest first: of the whole space, or of one `subject` (a node id, a setting key). With `before` (a change id), only older changes, to read a long history page by page; `before` and `subject` do not go together. |
| `change` | `space`, `id` (a change id) | `{ "change", "versions", "links" }`: the change, the versions of nodes it wrote and the links it made or removed, or `null`. |
| `undo` | `space`, `change`, `force`? (default `false`), `by` | The new change that undoes it. Fails with `undo_conflict` when later changes touched the same nodes or links, unless `force` is set; confirm with the person before forcing. |
| `restore` | `space`, `id`, `version` (the `seq` of a row of its history), `by` | The node as it is now. Also recovers a deleted node: restore it to its `delete` version. |
| `purge` | `space`, `id`, `by` | The change that records it. Erases the node and every version of it for good. Only a person can purge. |
| `vault.export` | `space`, `dir` (absolute) | `{ "written", "unchanged", "removed", "files" }`. Writes the space to `dir` as Markdown files; see below. |
| `vault.import` | `space`, `dir` (absolute), `by` | `{ "created", "updated", "unchanged", "skipped", "attachments", "links" }`. Reads every `.md` file under `dir`, written by `vault.export` or from an Obsidian vault. `skipped` lists `{ "path", "reason" }`. |
| `markdown.folder.get` | `space` | The live Markdown folder: `{ "folder", "path", "warning"? }`. |
| `markdown.folder.set` | `space`, `folder` (absolute, or `null` to turn it off), `by` | The live Markdown folder as it is now. Only a person can change it. |
| `subscribe` | `space`? | `true`. Starts `changed` notifications; see below. |
| `unsubscribe` | none | `true`. Stops them. |
| `service.status` | none | `{ "version", "pid", "spaces", "clients", "capture" }`. `capture` is the default space while capture runs (where conversations of no folder go), or `null`. An agent only sees the spaces it may read. |
| `auth` | `owner` or `key` | `{ "as": "person" }`, or `{ "as": "agent", "id", "name", "kind" }`. See Authentication. |
| `agents.list` | none | The agents of this machine. People only. |
| `agents.add` | `name`, `kind`, `fingerprint`, `by` | The agent. Adds it with the fingerprint of its new key, or gives an agent of that name the new key and revokes its old ones, in every space it was given. People only. |
| `agents.revoke` | `agent` (its name), `by` | The agent. Revokes all its keys and sets its level to `none` in every space it was given. People only. |
| `grants.set` | `agent` (its name), `space`, `level` (`none`, `read` or `write`), `by` | `{ "space", "level" }`. Writes the agent and its key fingerprints into the space, then its level. People only. |

An agent, as `agents.list`, `agents.add` and `agents.revoke` return it, is:

```json
{ "id": "01K83N6V4W5D2XQJ0Y7RZ9T1AB", "name": "claude-code", "kind": "claude-code",
  "created_at": 1792489800123,
  "keys": [ { "fingerprint": "sha256:2d8e…", "created_at": 1792489800123, "revoked_at": null } ],
  "spaces": [ { "space": "/Users/ada/Knode/Work.knode", "level": "write" } ] }
```

`level` is `null` for a space that cannot be opened now. The agents of a machine, with the
fingerprints of their keys, are kept in `$KNODE_HOME/agents.json`.

A folder, as `folders.list` and `folders.set` return it, is the row of the `folders` table of the
[.knode specification](https://github.com/knodemd/format) (section 15.2), with where it is on this
machine:

```json
{ "id": "01K83N6V4W5D2XQJ0Y7RZ9T1AB", "kind": "project", "path": "~/Developer/site",
  "capture": true, "rules": { "exclude": ["node_modules/**"] },
  "created_at": 1792489800123, "created_by": { "kind": "person", "actor": "Ada" },
  "updated_at": 1792489800123, "updated_by": { "kind": "person", "actor": "Ada" },
  "local": "/Users/ada/Developer/site", "missing": false }
```

`path` is recorded as the specification says (section 15.3): relative to the home folder as `~/...`
when it is inside it, so the same space finds the same folders on another machine. `local` is
`null` when the path cannot be resolved here; `missing` is `true` when the folder does not exist
on this machine, where it is kept and skipped. `rules` holds `exclude` (patterns of folders left
out, for `project` and `notes`), `include` (for `notes`) and `agent` (required for `agent`
folders: the agent whose sessions it holds, such as `claude-code`).

Space info is:

```json
{ "path": "/Users/ada/Knode/Personal.knode", "id": "01K83N6V4W5D2XQJ0Y7RZ9T1AB", "name": "Personal",
  "format": 0, "created_at": "2026-10-20T09:30:00.000Z", "nodes": 42 }
```

## Changes

After `subscribe`, the connection receives a `changed` notification after each write, in the
order the writes were made:

```json
{"jsonrpc":"2.0","method":"changed","params":{"space":"/Users/ada/Knode/Personal.knode","node_id":"01K83N6V4W5D2XQJ0Y7RZ9T1AB","op":"update"}}
```

- `subscribe` with `space` adds that space; without `space`, the connection hears about every
  space. Every change made after the answer to `subscribe` is delivered.
- When a space is renamed, or its settings or folders change, the connection receives a `space`
  notification instead, with `what` set to `renamed` (and the new `name`), `settings` (and the
  `key`) or `folders` (and the `folder` id):

  ```json
  {"jsonrpc":"2.0","method":"space","params":{"space":"/Users/ada/Knode/Personal.knode","what":"settings","key":"space.icon"}}
  ```
- `op` is `create`, `update`, `delete`, `link` (a link from `node_id` was made) or `unlink` (one
  was removed). Renaming a node also sends `update` for each node whose `[[links]]` were rewritten
  to the new title.
- A write that changes nothing sends nothing.
- A connection that falls more than 1024 changes behind receives
  `{"jsonrpc":"2.0","method":"lagged","params":{"missed":12}}` and should reload what it shows.

Changes have the shape of the `changes` table of the [.knode specification](https://github.com/knodemd/format)
(section 17): `{ "id", "changed_at", "changed_by", "action", "subject", "undoes"?, "detail" }`.
A write that touches many nodes, such as an undo or an import, sends a `changed` notification for
each node it wrote.

## Markdown

`vault.export` writes one `.md` file per node, with YAML frontmatter for its id, kind, tags,
dates, provenance, links and properties. Exporting again to the same folder rewrites only the files
that changed, and never touches files Knode did not write. `vault.import` reads such a folder, or an
Obsidian vault, into a space: a file whose id is already in the space updates that node; any other
file becomes a new node that keeps the id, dates and provenance of its frontmatter. Importing again
never makes duplicates.

**The live Markdown folder** of a space is a folder the service keeps in step with it, one way,
from the space to the folder. It is off by default; `markdown.folder.set` turns it on and stores
the folder in the space setting `markdown.folder` (relative to the home folder as `~/...` when it
is inside it). While it is on, the service writes the whole space to it when it opens the space,
writes each change within a few seconds, and writes what is left when it stops. A folder inside
iCloud Drive, Dropbox, OneDrive or Google Drive gets a `warning`: two programs syncing the same
files can make conflicted copies. A write that fails is logged and tried again a second later.

## Capture

Knode Service captures agent conversations (Claude Code, Codex and the other agents Knode reads)
into spaces: one `conversation` node per session, kept current while the agent writes it. Where
each conversation goes follows section 15.5 of the
[.knode specification](https://github.com/knodemd/format):

1. The `project` folders of every space the service knows are matched against the folder the
   conversation was held in. The most specific folder wins, and the same folder may feed more
   than one space.
2. The conversation is written to each of those spaces where the folder's `capture` is on, its
   `exclude` rules do not leave the conversation's folder out, and the space's
   `capture.enabled` is `on`. If none qualifies, it is **not stored anywhere**: capture off for a
   project means nothing of it is kept, not that it goes somewhere else.
3. A conversation of no project goes to the space of the `agent` folder it was read from, or
   else to the default space, if that space's `capture.enabled` is `on`.

The service knows the spaces named in `$KNODE_HOME/config.toml` (`default_space` and `spaces`,
which `knode init` and the app keep) and every space it has open. It reads the file and the
folders of those spaces when it starts, again every few seconds, and at once when a client
changes folders or settings: a change applies to conversations written after the call answers.

- `default_space` is the space conversations of no folder go to. With none, only conversations
  of the spaces' folders are captured.
- `spaces = ["/path/to/Work.knode"]` lists the other spaces Knode knows.
- `capture = false` turns capture off everywhere. It is on when left out.
- `capture_exclude = ["/path/to/project"]` leaves out the conversations held in these projects
  and in every folder inside them, whatever the folders of the spaces say.

Capture follows the agents' own session folders (such as `~/.claude/projects`, or
`$CLAUDE_CONFIG_DIR/projects`, and `~/.codex/sessions`, or `$CODEX_HOME/sessions`) and the
`agent` folders of the spaces. What it has read is saved in `$KNODE_HOME/capture/state.json`, so
after a restart it only reads what is new; the text of a conversation that goes nowhere is not
kept there either. Its writes are made with the provenance
`{ "kind": "agent", "actor": "claude-code" | "codex" | ..., "session": "<session id>" }`, go
through the same space as every client's, one at a time, and are announced with `changed` like
any other write. `service.status` gives the default space as `capture` while capture runs.

## Errors

Errors are JSON-RPC error objects. `message` is written for people; `data.kind` is a stable name to
match on.

| Code | `data.kind` | Meaning |
| --- | --- | --- |
| -32700 | | The line is not valid JSON. The answer has `"id": null`. |
| -32600 | | Not a valid JSON-RPC 2.0 request, or a batch. |
| -32601 | | No method has this name. |
| -32602 | `invalid_params` | Params missing or of the wrong type, a bad node id, or a relative `space` path. |
| -32603 | | Something went wrong inside the service, or it is stopping. |
| -32001 | `not_found` | No node, or no change, has this id. |
| -32002 | `not_a_space` | The path is not a `.knode` package. |
| -32003 | `already_exists` | A space already exists at this path. |
| -32004 | `invalid` | A value breaks a rule of the format, such as an empty title or a bad kind. |
| -32005 | `unsupported` | The space was written by a newer version of Knode, or its files do not agree. |
| -32006 | `storage` | The files of the space could not be read or written. |
| -32010 | `unauthenticated` | The connection has not authenticated, or its key is unknown or was revoked. |
| -32011 | `forbidden` | An agent wrote where it may only read, or called a method for people only. |
| -32020 | `cannot_undo` | The change is not one that is undone (settings, folders, agents and grants are changed again instead), or the versions it needs were purged. |
| -32021 | `already_undone` | The change was already undone; `data.undone_by` is the undo. Undo that change to redo it. |
| -32022 | `undo_conflict` | Later changes touched what the change touched; `data.later` lists their ids. Send `force` to overwrite them, only after the person confirms. |
| -32024 | `folder` | A folder of Markdown files could not be read or written. |

```json
{"jsonrpc":"2.0","id":7,"error":{"code":-32001,"message":"node 01K83N6V4W5D2XQJ0Y7RZ9T1AB not found","data":{"kind":"not_found"}}}
```

## Starting the service

- `knode service run` runs it in the foreground. If a service is already running in the same
  Knode home it says so and exits successfully.
- `knode service start` starts it in the background, and every `knode` command that uses a space
  (including `knode mcp`) does the same when it is not running. A service started this way writes
  its messages to `$KNODE_HOME/service.log`.
- `knode service stop` stops it, however it was started; `knode service status` shows its
  version, process id, open spaces, clients and capture.
- `knode service install` starts it at login and now: on macOS with a launch agent,
  `~/Library/LaunchAgents/md.knode.service.plist`, loaded with `launchctl bootstrap`; on Linux
  with a systemd user unit, `knode.service`, enabled with `systemctl --user enable --now`. The
  service is started again if it crashes, but not after `knode service stop`.
  `knode service uninstall` undoes it.
