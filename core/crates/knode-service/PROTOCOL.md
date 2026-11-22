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
- With `read`, `space.open`, `space.set_up`, `settings.get`, `folders.list`, `node.get`, `node.find_by_title`, `links.from`, `links.back`,
  `links.unresolved`, `search`, `recent`, `history`, `changes`, `change`, `vault.export`,
  `markdown.folder.get`, `doc.open`, `doc.updates` and `subscribe` are allowed; writes fail with
  `forbidden`.
- With `write`, `node.create`, `node.update`, `node.delete`, `link.create`, `link.delete`,
  `undo`, `restore`, `vault.import` and `doc.edit` are allowed too. An agent may only `undo` a change made by
  the agent its key belongs to, and never with `force`; any other undo fails with `forbidden`.
- The methods for link kinds, views, canvases and attachments follow the same levels: with
  `read`, `link_kinds.list`, `links.of`, `links.back_labeled`, `views.list`, `view.get`, `graph`,
  `canvas.board`, `positions.list`, `groups.list`, `attachments.list` and `attachment.read`; with
  `write`, `link_kind.set`, `link_kind.remove`, `view.set`, `view.remove`, `view.viewport.set`,
  `canvas.create`, `positions.set`, `positions.remove`, `group.set`, `group.remove`,
  `attachment.add` and `attachment.remove`. `attachment.add_file`, `attachment.path`,
  `conversations.import` and `import.run` name files on this machine (or fetch a web page) and
  are for people only.
- With `read`, `recordings.get`, `recordings.pending` and `recordings.failed` are allowed; the
  other `recordings.*`
  methods store files of this machine and what the app made on the device, and are for people
  only.
- Creating and renaming spaces, changing settings and folders, managing agents, keys and grants,
  `purge`, `markdown.folder.set`, `doc.peer`, `doc.merge` and every other method are for people
  only and fail with `forbidden`. (The authorship of merged Loro updates comes from their peers,
  not from the key, so agents edit with `doc.edit` and `node.update` instead.)
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
| `space.set_up` | `space` | `{ "set_up" }`: whether the space has been set up, so an agent should not offer to set Knode up on its own. `false` only when the space has no onboarding receipt (`onboarding.json`), no `project` node and fewer than 3 nodes created by a person, not counting `conversation` nodes and titles starting with `Example:`. |
| `onboarding.apply_into` | `space`, `plan` (a reviewed `knode onboard analyze` plan), `by` | The onboarding receipt. Applies the plan into this space instead of a new one, only while `space.set_up` is `false`: the source is checked against the plan, the plan is rehearsed in a throwaway space, then its documents, a `project` node per project with the documents `part-of` it and `onboarding.json` are written into the space, and `capture.enabled` is set to `off`. What the space held stays. Fails with `invalid` when the space is set up, the source changed or a note of the source has the id of a node already in the space, and then writes nothing. People only. |
| `space.rename` | `space`, `name`, `by` | Space info, with the new name. Changes `name` in `manifest.json`; the package keeps its folder name. People only. |
| `settings.get` | `space` | Every setting of the space, by key: `{ "space.id": "01K8…", "space.icon": "🌱" }`. A key left out has its default. Agents never get the settings of sources (`source.<id>`), whose exclusions name chats and mailboxes; `changes` and `change` give agents those changes without their values. |
| `settings.set` | `space`, `key`, `value` (`null` for the default), `by` | Every setting, as `settings.get`. `space.icon` is one emoji; `capture.enabled` and `recordings.summaries` are `on` or `off`, all stored as the person's choice (`null` puts back the default, stored as no value); turning it on for a space whose capture was off or waiting for setup also records the time in `capture.since` (below). Other keys fail with `invalid`; the live Markdown folder is changed with `markdown.folder.set`. People only. |
| `folders.list` | `space` | The folders that feed the space, oldest first. See below. |
| `folders.set` | `space`, `kind`, `path`, `capture`? (default `true`), `rules`? (default `{}`), `id`?, `by` | The folder. Adds it, or changes the folder with this `id`, or with the same kind and path. `kind` is `project`, `notes` or `agent`; `path` is absolute or starts with `~/`, and may not be inside the space. People only. |
| `folders.remove` | `space`, `id`, `by` | `true` if there was such a folder. Nothing captured or imported from it is removed. People only. |
| `folders.overlaps` | `spaces` (paths) | The project folders of these spaces that overlap, each pair once: `{ "kind": "same" or "nested", "outer": f, "inner": f, "message": "..." }`, where `f` is `{ "space", "space_name", "folder" (id), "path", "capture", "space_capture" }` (`capture`: the folder's own switch; `space_capture`: the space's `capture.enabled`; a folder's conversations are captured only when both are on). `same`: one folder is a project folder of two spaces, so its conversations are captured into each of them where capture is on (`outer` is the first space). `nested`: `inner` is inside `outer`, so conversations held in it go to `inner`'s space only, and nowhere when capture is off there. `message` says so in a sentence, naming the space or folder whose capture is off. People only. |
| `node.create` | `space`, `title`, `body`?, `kind`? (default `note`), `tags`?, `by` | The node. |
| `node.get` | `space`, `id` | The node, or `null` when no node has this id. |
| `node.find_by_title` | `space`, `title` | Node summaries with this title, oldest first. Titles are compared without regard to case or extra spaces; a `[[link]]` with this title points at the first one. |
| `node.update` | `space`, `id`, `title`?, `body`?, `kind`?, `tags`?, `version`?, `by` | The node as it is now. Fields left out stay as they are. With `version` (from `doc.open`), the new `body` is merged with what others wrote since; see Editing together. When the body was written through its document, the node also has `version` and `seen`. |
| `node.delete` | `space`, `id`, `by` | `null`. |
| `link.create` | `space`, `from`, `to`, `kind`? (default `related`), `by` | The link. |
| `link.delete` | `space`, `from`, `to`, `kind`? (default `related`), `by`? | `true` if there was such a link. `by` is recorded as who removed it; left out (by clients older than v0.2), the change is recorded as Knode's own, `system`. |
| `links.from` | `space`, `id` | The links that start from the node. |
| `links.back` | `space`, `id` | The links that point at the node. |
| `links.unresolved` | `space`, `id` | The `[[links]]` in the node's text that name no node yet, as written. |
| `search` | `space`, `query`, `limit`? (default 20, at most 1000), `mode`? (`words`, `meaning` or `hybrid`) | With `mode`: `{ "mode", "hits", "note"? }`, where `mode` is the mode that ran. Without it: the bare list of hits, ranked as `hybrid`, as clients before `mode` expect. See Search. |
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
| `markdown.deleted.list` | `space` | The notes whose file was deleted from the live Markdown folder, newest first: `[{ "id", "title", "path" }]`, where `path` is the path the file had. People only. |
| `markdown.deleted.keep` | `space`, `id` | `true` if the note was marked. Removes its `markdown.deleted` property, so the folder writes its file again. To delete the note instead, use `node.delete`. People only. |
| `doc.open` | `space`, `id` | `{ "text", "version" }`: the body and the version of its document, made from the body the first time. See Editing together. |
| `doc.edit` | `space`, `id`, `version`?, `splices` or `text`, `by` | `{ "version", "seen", "text"? }`. Edits the body at `version`, merged with what others wrote since. |
| `doc.peer` | `space`, `by` | `{ "peer" }`: a new Loro peer id, in decimal, for an editor that keeps its own copy of documents. People only. |
| `doc.merge` | `space`, `id`, `update` (base64) | `{ "version" }`. Merges Loro updates made as recorded peers. People only. |
| `doc.updates` | `space`, `id`, `since`? | `{ "update", "version" }`: the Loro updates made since the version `since` (the whole document without it), in base64. |
| `subscribe` | `space`?, `updates`? (default `false`) | `true`. Starts `changed` notifications; see below. With `updates`, those of body writes carry the Loro updates. |
| `unsubscribe` | none | `true`. Stops them. |
| `service.status` | none | `{ "version", "pid", "spaces", "clients", "connections"?, "capture", "index" }`. `capture` is the default space while capture runs (where conversations of no folder go), or `null`. `index` is as `index.status` answers it, from what the index last saw. An agent only sees the spaces it may read. `connections` is returned to the person only: authenticated connections as `{ "id", "identity", "space" }`, with the identity from `auth` and the last space successfully used, or `null` before using one. A successful request updates its space; a refused request does not. Entries disappear on disconnect; ids are unique until restart. Older clients may ignore the field and older servers omit it. |
| `index.status` | none | How far the open spaces are indexed for search by meaning, and the model. See Search. People only. |
| `index.run` | `space`? | As `index.status`. Opens `space` when given, then indexes every open space now, even on battery, and tries a failed download again. People only. |
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

### Link kinds, views, canvases and attachments

These methods read and write what schema version 4 of the
[.knode specification](https://github.com/knodemd/format) adds: kinds of link with labels (section
18.1), graph views and canvases with the positions and groups of their nodes (section 18.2), and
files attached to nodes (section 20). Their shapes are those of the specification.

| Method | Params | Result |
| --- | --- | --- |
| `link_kinds.list` | `space` | Every kind of link a link can be made with: the built-in kinds, then the space's own, by name. Each is `{ "kind", "label", "inverse_label", "direction", "description", "built_in" }`, with `created_at`, `created_by`, `updated_at` and `updated_by` for a kind the space defines. `direction` is `directed` or `symmetric`. |
| `link_kind.set` | `space`, `kind`, `label`, `inverse_label`?, `direction`? (default `directed`), `description`?, `by` | The kind. Defines it or changes its labels. Built-in kinds cannot be changed, and the direction of a kind cannot change while links have it (`invalid`). `link.create` takes only built-in and defined kinds. |
| `link_kind.remove` | `space`, `kind`, `by` | `true` if the space defined such a kind. Fails with `invalid` while links have it. |
| `links.of` | `space`, `id` | The links of the node as a reader shows them: going out, then coming in, each `{ "link", "outgoing", "node", "title", "label", "direction" }`, where `node` and `title` are the other end and `label` is how the link reads from this node (the kind's label going out, its inverse label coming in). A symmetric link between two nodes is shown once. |
| `links.back_labeled` | `space`, `id` | The backlinks of the node, of every kind, in the shape of `links.of`. |
| `views.list` | `space` | The graph views and the canvases whose node exists, oldest first: `{ "id", "kind", "name", "node_id"?, "settings", "created_at", "created_by", "updated_at", "updated_by" }`. `kind` is `graph` or `canvas`; a canvas has the `node_id` of its node and is named by its title. `settings` holds `viewport` (`{ "x", "y", "zoom" }`) and, for a graph view, `filter`. |
| `view.get` | `space`, `id` | The view, or `null`. |
| `view.set` | `space`, `id`?, `name`, `filter`?, `by` | The graph view: created without `id`, renamed or filtered with it. `filter` is an object with any of `node_kinds`, `tags`, `actors` (lists of text), `since`, `until` (RFC 3339) and `link_kinds`; a node is shown when it passes every key given. Left out, the view shows the whole space. |
| `view.remove` | `space`, `id`, `by` | `true` if there was such a graph view. A canvas is removed by deleting its node. |
| `view.viewport.set` | `space`, `id`, `x`, `y`, `zoom`, `by` | `null`. Saves the part of the view on screen. Adds no change and is not announced. |
| `graph` | `space`, `view`?, `filter`? | `{ "nodes", "edges", "positions", "link_kinds" }`: the nodes `filter` keeps (`{ "id", "kind", "title", "tags", "created_at", "updated_at", "created_by", "updated_by" }`, newest change first), the links between them (`{ "from", "to", "kind" }`), the positions saved in `view` for the nodes shown, and every kind of link. Without `filter` the view's own applies; with neither, the whole space. |
| `canvas.create` | `space`, `title`, `body`?, `tags`?, `by` | The canvas view. Creates a node of kind `canvas` (its title names the canvas, its body says what it is for) and its view, as one `node.create` change. |
| `canvas.board` | `space`, `id` (the canvas node) | `{ "view", "title", "body", "items", "groups", "edges", "link_kinds" }`, or `null` when the node is not a canvas. `items` are the nodes on it, back to front: `{ "position", "kind", "title", "excerpt" }`; `edges` the links between them. |
| `positions.list` | `space`, `view` | The positions of the nodes of the view that exist, back to front: `{ "view_id", "node_id", "x", "y", "width"?, "height"?, "z", "display", "group_id"?, "updated_at", "updated_by" }`. `x` and `y` are the centre, in points, `y` growing down; `display` is `open` or `collapsed`. |
| `positions.set` | `space`, `view`, `positions` (a list of `{ "node_id", "x", "y", "width"?, "height"?, "z"?, "display"?, "group_id"? }`), `by` | `null`. Places the nodes in one write. `width` and `height` go together. Groups are only on a canvas. |
| `positions.remove` | `space`, `view`, `nodes` (node ids) | How many of the nodes were in the view. The nodes are not deleted. |
| `groups.list` | `space`, `view` | The groups of a canvas: `{ "id", "view_id", "label", "x", "y", "width", "height", "display", "updated_at", "updated_by" }`. |
| `group.set` | `space`, `view`, `id`?, `label`?, `x`, `y`, `width`, `height`, `display`?, `by` | The group: added without `id`, changed with it. |
| `group.remove` | `space`, `view`, `id` | `true` if the canvas had such a group. Its nodes stay on the canvas, outside any group. |
| `attachments.list` | `space`, `id` | The files attached to the node, oldest first: `{ "node_id", "name", "hash", "size", "media_type", "created_at", "created_by" }`. `![[name]]` in the node's body shows the file. |
| `attachment.read` | `space`, `id`, `name` | `{ "attachment", "content" }` with the bytes in base64, or `null` when the node has no attachment with this name. Files larger than 10 MiB fail with `invalid`: a person opens them from `attachment.path`. |
| `attachment.add` | `space`, `id`, `name`, `content` (base64, at most 10 MiB of bytes), `by` | The attachment. Stores the bytes once, by their SHA-256, and attaches them under `name`; a name the node already uses for other content gets ` 2`, ` 3`... Attaching the same content under the same name again changes nothing. |
| `attachment.add_file` | `space`, `id`, `path` (absolute), `by` | The attachment of the file at `path`, under its file name, up to 100 MB. People only. |
| `attachment.remove` | `space`, `id`, `name`, `by` | `true` if there was such an attachment. The file stays in the space until a person purges unused files. |
| `attachment.path` | `space`, `id`, `name` | Where the attachment's file is on this machine, or `null`. Never write to it. People only. |

Link kinds, attachments added and removed, and graph views created, renamed, filtered or removed
are changes in the history (`link_kind.set`, `link_kind.remove`, `attachment.add`,
`attachment.remove`, `view.set`, `view.remove`). Viewports, positions and groups carry their own
provenance and add no change.

### Recordings

Meetings recorded on the device, voice memos and audio files added by hand, with their transcript
and their summary. Transcribing and summarising happen in the app, on the device; these methods
store what it produces and say what is still to do. A recording is a node of kind `meeting`,
`voice-memo` or `recording` with its audio files attached, its properties under `recording.`
(`started_at`, `ended_at`, `duration_ms`, `source`, `key`, `language`, `transcript`, `summary`,
`error`, `app`, `tracks`) and a `part-of` link to its project. Its transcript is a node of kind
`transcript`, `derived-from` it.

Every method returns a recording, or a list of them:

```json
{
  "id": "01J…", "kind": "meeting", "title": "Weekly sync",
  "started_at": 1795343400000, "ended_at": 1795345200000, "duration_ms": 1800000,
  "source": "recorder", "key": null, "language": "es-ES",
  "transcript": "done", "summary": "pending", "error": null, "app": "zoom.us",
  "project": "01J…", "transcript_id": "01J…",
  "tracks": [{ "name": "microphone.m4a", "track": "microphone" }, { "name": "system.m4a", "track": "system" }]
}
```

`kind` is `meeting`, `voice-memo` or `recording`; `source` follows it: `recorder`, `voice-memos`
or `file`. `track` is `microphone` (the person), `system` (the others in an online meeting) or
`mixed` (everyone in one file). `transcript` is `pending`, `done` or `failed`; `summary` is
`pending`, `done`, `off`, `unavailable` or `failed`; `error` says why a stage failed or is
unavailable. Times are Unix milliseconds.

| Method | Params | Result |
| --- | --- | --- |
| `recordings.create` | `space`, `kind`, `title`, `started_at`, `ended_at`, `project`?, `audio` (`[{ "path" (absolute), "track" }]`), `key`?, `app`?, `summaries`? (bool, `true` when omitted), `by` | The recording. Every file is checked first (it must exist, up to 100 MB), then the node is created with a header saying when and for how long, an `![[name]]` embed per file, the files attached (copied) and a `part-of` link to `project`; `recording.key` is written last, and a node left half made by a failed step is deleted, so a key the space has always names a whole recording. `transcript` is `pending`; `summary` is `pending` with `summaries`, else `off`. A `key` the space already has returns that recording, after attaching (and embedding) each file of `audio` whose `track` it has no attached file for and adding the `part-of` link to `project` if missing; its `tracks` list only what is attached. People only. |
| `recordings.pending` | `space` | The recordings whose transcript is `pending`, or whose summary is `pending` while the transcript is `done`, oldest first (by `started_at`). A recording that cannot be read is left out (and logged) rather than failing the list. |
| `recordings.failed` | `space` | The recordings whose transcript is `failed`, or whose summary is `failed` or `unavailable`, newest first (by `started_at`): those the app offers to try again with `recordings.retry`. A recording that cannot be read is left out (and logged). |
| `recordings.get` | `space`, `id` | The recording. Fails with `not_found` without such a node and `invalid` for a node that is not a recording. |
| `recordings.transcript` | `space`, `id`, `transcript` (`{ "language"?, "engine", "segments": [{ "start_ms", "end_ms", "speaker", "text" }] }`), `by` | The recording. Creates its transcript node, `Transcript: <title> (<YYYY-MM-DD HH:MM>)` with the local time the recording started (followed by ` 2`, ` 3`... when another node already has that title, so the link finds this transcript), or replaces the one written before (keeping its title while no other node takes it): a line naming the engine, then one paragraph per turn (a run of segments by the same speaker), `**Speaker** (HH:MM:SS): text`. Sets `transcript` to `done` and `language`, removes `error` (unless the summary failed) and adds a `[[Transcript: …]]` line to the recording's body, after its embeds. People only. |
| `recordings.summary` | `space`, `id`, `summary` (`{ "model", "overview", "key_points", "decisions", "action_items": [{ "task", "owner"? }], "open_questions" }`), `by` | The recording. Writes the minutes into its body between a `<!-- knode:minutes -->` line and a `<!-- /knode:minutes -->` line (HTML comments: hidden in Markdown, kept as they are in Markdown files): `## Summary`, `## Key points`, `## Decisions`, `## Action items` (`- [ ] task — owner`) and `## Open questions`, leaving out empty ones, and a line saying `model` wrote them on this Mac from the transcript. When the body has the two lines once, in order, only what is between them is replaced; otherwise the block is added after the header, embeds and transcript link. Text outside the markers is never changed. Sets `summary` to `done`. People only. |
| `recordings.fail` | `space`, `id`, `stage` (`transcript` or `summary`), `state` (`failed`, or `unavailable` for a summary), `error`, `by` | The recording, with the stage in that state and `error` set. People only. |
| `recordings.retry` | `space`, `id`, `stage`, `by` | The recording, with the stage back to `pending` and `error` removed. A summary fails with `invalid` until the transcript is `done`. People only. |

The writes announce the recording and its transcript as changes. Whether the app summarises
recordings is the space setting `recordings.summaries` (`on`, the default also when it is not
set, or `off`; only `off` turns it off), read and
changed with `settings.get` and `settings.set`; the app passes it as `summaries` when it stores a
recording. Agents with `read` may call `recordings.get`, `recordings.pending` and
`recordings.failed`; the other
methods are for people only.

### Data exports

| Method | Params | Result |
| --- | --- | --- |
| `conversations.import` | `space`, `path` (absolute: a `.zip` as downloaded, or the folder it was extracted to), `by` | `{ "format", "agent", "created", "updated", "projects", "unchanged", "empty", "skipped" }`. Imports a Claude, ChatGPT or Google Takeout (Gemini Apps) data export: one `conversation` node per conversation, found again by its agent and session, so importing the same export twice writes nothing new. `agent` is `claude-ai`, `chatgpt` or `gemini-web`; `skipped` lists `{ "item", "reason" }`. A path that is not such an export fails with `invalid`. People only. |

### Importing

| Method | Params | Result |
| --- | --- | --- |
| `import.run` | `space`, `path` (absolute) or `url` (`http` or `https`), `from`?, `include_deleted`?, `by` | The report below. Imports what is at `path`, or the web page at `url`, which is fetched then and only then. People only. |

`from` names what it is; without it, the service tells it from the path and the start of the
file. With `from` `apple-notes` or `bear` and no `path`, the app's own notes on this Mac are read.

| `from` | What `path` is |
| --- | --- |
| `export` | A Claude, ChatGPT or Google Takeout (Gemini Apps) data export: the `.zip` or its folder, as in `conversations.import` |
| `markdown` | A folder of `.md` files (an export or an Obsidian vault, as in `vault.import`), or one `.md` file: a note titled by its first heading |
| `session` | A Claude Code or Codex session file (`.jsonl`): a `conversation` node written by that agent |
| `pdf` | A PDF: one `source` node with its text, page by page, and the PDF attached |
| `web` | A saved web page (`.html`), or the page at `url`: one `source` node with the readable article |
| `kindle` | `My Clippings.txt`, or a notebook a Kindle app exported (`.html` with `bookTitle` and `noteText`): one `source` node per book |
| `readwise` | A Readwise CSV export (`Highlight` and `Book Title` columns), or its Markdown export (a `.md` file or the unzipped folder) |
| `apple-notes` | The Apple Notes group container or its `NoteStore.sqlite`, read only, or notes it exported as Markdown |
| `bear` | A Bear backup (`.bear2bk`, `.bearbk`), TextBundles (`.textbundle`, `.textpack`), its database, or notes it exported as Markdown |
| `notion` | Notion's "Markdown & CSV" export, zipped or extracted |
| `evernote` | An `.enex` file, or a folder of them |
| `whatsapp` | A WhatsApp chat export: the `.zip`, its folder, or the chat's `.txt` (`_chat.txt` on iOS, `WhatsApp Chat with <name>.txt` on Android) with the media beside it. One `chat` node per chat, its messages grouped by day; the media attached to it. Importing it again adds only the new messages |
| `telegram` | A Telegram Desktop export in JSON: the folder with `result.json` (one chat, or the whole account), a `.zip` of it, or the `result.json`. One `chat` node per chat, as for `whatsapp`; formatted text becomes Markdown and replies name the message they answer |
| `slack` | A Slack workspace export: the `.zip` or its folder (`users.json`, `channels.json`, and a folder of day files per conversation). One `chat` node per channel, group or direct message; each thread its own `chat` node, `part-of` its channel; the members who wrote or were mentioned `person` nodes, found again by `slack.user`, mentions `[[links]]` to them and chats `related` to who wrote in them. Files are links, or attached when the export has them in `__uploads/` |
| `claude-code-memory` | Claude Code's `.claude` folder or its `projects` folder; without `path`, `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`, of the service. Each `<folder>/memory/*.md` but `MEMORY.md` becomes a note (`markdown.path` `claude-code-memory/<folder>/<file>`), `part-of` the project its folder belongs to; folders that are symbolic links are left out |

```json
{ "source": "kindle", "format": "Kindle clippings", "from": "/Users/ada/Downloads/My Clippings.txt",
  "created": 1, "updated": 1, "unchanged": 0, "edited": 0, "highlights": 3, "attached": 0,
  "nodes": [ { "id": "01K83N6V4W5D2XQJ0Y7RZ9T1AB", "title": "Walden", "outcome": "created", "highlights": 2 },
             { "id": "01K83N6V4W5D2XQJ0Y7RZ9T1AC", "title": "On Liberty", "outcome": "updated", "highlights": 1 } ],
  "skipped": [], "notes": [] }
```

- `source` is the `from` it was read as, and `format` what it was, for people.
- `nodes` lists the nodes created or changed, in order (for reading, every document read), each
  with `outcome` `created`, `updated`, `unchanged` or `edited` (the source changed, but so did the
  node, in Knode, since the last import, so it was left as it is), and `highlights` added. The
  nodes of a Markdown folder are named by their path.
- `report`, for a folder of Markdown files only, is the migration report
  (`knode_vault::migration::MigrationReport`, `version` 1): every file of the folder with its
  `status` (`created`, `updated`, `unchanged`, `edited`, `attached`, `skipped` or `failed`), its
  `node` and a `reason`; `counts`; `unresolved_links`, `ambiguous_links`, `missing_attachments`
  and `failed_links`; and `limits`, what it does not check. A note imported before whose node
  was deleted in Knode is not imported again: it is `skipped`, with the deleted `node` and a
  reason saying so, and counted in `counts.deleted`. `include_deleted: true`, only for a folder
  of Markdown files, imports such notes again as new nodes.
- `skipped` lists `{ "item", "reason" }` for what could not be imported; the rest was. `notes`
  lists what else is worth knowing, such as files left out or parts of a note read only in part.
- `linked` (`{ "<project title>": <count> }`) and `unmatched` (a list), only when not empty, say
  how many nodes were linked `part-of` each project and what matched no project, for
  `claude-code-memory`, where `unmatched` lists the encoded folders whose notes have no project.
- Importing the same thing again finds the same nodes: it writes only what changed.
- Every node written is announced with `changed`, as for any write, even when the import fails
  part way.
- A path that is not something Knode imports, or not the kind `from` names, fails with
  `invalid`; files that cannot be read fail with `folder`; another app's notes that macOS keeps
  from Knode Service fail with `no_access`.

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
  `key`) or `folders` (and the `folder` id). When a kind of link is defined, changed or removed,
  `what` is `link_kinds` (and the `link_kind`); when a graph view or a canvas is created, changed
  or removed, or nodes or groups are placed on it, `what` is `views` (and the `view` id). Saving
  a viewport is not announced:

  ```json
  {"jsonrpc":"2.0","method":"space","params":{"space":"/Users/ada/Knode/Personal.knode","what":"settings","key":"space.icon"}}
  ```
- `op` is `create`, `update`, `delete`, `link` (a link from `node_id` was made) or `unlink` (one
  was removed). Attaching a file to a node or removing one sends `update` for the node; creating
  a canvas sends `create` for its node. Renaming a node also sends `update` for each node whose `[[links]]` were rewritten
  to the new title.
- A body written through its document (see Editing together) sends `changed` with `op`
  `update` and also `version`, the version of the document now, and `by`, who wrote it; with
  `updates` set in `subscribe`, also `update`, the Loro updates of the write in base64:

  ```json
  {"jsonrpc":"2.0","method":"changed","params":{"space":"/Users/ada/Knode/Personal.knode","node_id":"01K83N6V4W5D2XQJ0Y7RZ9T1AB","op":"update","version":"{\"7318201936571142\":52}","by":{"kind":"agent","actor":"claude-code"}}}
  ```
- A write that changes nothing sends nothing.
- A connection that falls more than 1024 changes behind receives
  `{"jsonrpc":"2.0","method":"lagged","params":{"missed":12}}` and should reload what it shows.
- When an agent reads a space the connection subscribed to, a person's connection receives an
  `activity` notification, so the app can show what the agent looks at. It follows a successful
  read call made with an agent key (`node.get`, a list, `search` and the like) and names the
  node asked for by `id` and the nodes the answer listed, at most 25:

  ```json
  {"jsonrpc":"2.0","method":"activity","params":{"space":"/Users/ada/Knode/Personal.knode","agent":"codex","method":"search","nodes":["01K83N6V4W5D2XQJ0Y7RZ9T1AB"]}}
  ```

  Only the name of the agent, the method and node ids are sent: never a title, a body or a
  query. Nothing is stored (the access log keeps the calls themselves, see Authentication).
  Agents' connections never receive `activity`, and the person's own reads are not announced.
  A node the same agent read less than a second ago is left out, and a notice with no node left
  is not sent. `activity` has a feed of its own: a connection that falls behind on it, or whose
  outgoing messages are full, misses some of these notices, never a `changed` one, and is
  never sent `lagged` for them.

Changes have the shape of the `changes` table of the [.knode specification](https://github.com/knodemd/format)
(section 17): `{ "id", "changed_at", "changed_by", "action", "subject", "undoes"?, "detail" }`.
A write that touches many nodes, such as an undo or an import, sends a `changed` notification for
each node it wrote.

## Editing together

A person and agents can edit the same body at once. Their edits merge with
[Loro](https://loro.dev), as section 21 of the [.knode specification](https://github.com/knodemd/format)
describes, instead of one overwriting the other. Titles, kinds and tags are short values written
whole, as before.

- **Versions.** `doc.open` returns the body and the `version` of its document, a string to pass
  back unchanged (a JSON object from Loro peer ids to counts, such as `{"7318201936571142":52}`).
  An edit says which version it was made on, and what others wrote since is kept.
- **An editor** opens the body with `doc.open` and sends what the person types as `doc.edit`
  `splices`, `[{ "at", "delete", "insert" }]`, applied in order to the text at `version`.
  Positions and lengths count Unicode scalar values (Rust `char`s), not bytes or UTF-16 units.
  The answer has `version`, the version of the document now, and `seen`, the version of the text
  the editor has: what it had, with its edit, without what others wrote since. The editor bases
  its next edit on `seen`. When others wrote meanwhile, `version` differs from `seen` and `text`
  is the body now: an editor with no edit waiting shows it and goes on from `version`; one with
  edits waiting sends them first, based on `seen`, and catches up from that answer. A `doc.edit`
  with no splices only catches up.
- **An agent** that rewrites the whole text sends `node.update` with the `body` and the `version`
  it read, or `doc.edit` with `text`: the new text is diffed from the text at `version`, so edits
  made since are kept.
- **Old clients.** `node.update` without `version` writes the body as before. When the node already
  has a document, the body is written through it, replacing the current text, and open editors
  follow.
- **Typing.** Every `doc.edit` is one change, with its own history row. Editors gather keystrokes
  for a moment (the Knode app sends them 300 ms after the last one, at most every second while
  typing goes on) and send them before showing anyone else's edit.
- **Editors with their own copy** of a document, such as one made with the Loro library, ask for
  a peer with `doc.peer`, load the document with `doc.updates`, make their edits as that peer and
  send them with `doc.merge`; `subscribe` with `updates` gives them everyone else's.
- **Errors.** An edit based on a version the document does not have fails with
  `unknown_version`: open it again. Undo and restore write the body without Loro, and the
  document is brought in step before the next edit (section 21.5). `purge` removes the node's
  document; `node.delete` keeps it, for an undo.

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

A file deleted from the folder never deletes its note. The note is marked with the property
`markdown.deleted` (the path the file had), is no longer written to the folder, and waits for a
person: `markdown.deleted.list` lists those notes, `markdown.deleted.keep` keeps one and writes
its file again, and `node.delete` deletes it, which can be undone.

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

A new space nobody has set up yet receives nothing, so a space that `knode init` or the app has
just created, and that `knode connect` starts the service for, does not fill with every earlier
agent conversation of the machine before the person was asked. Such a space has no
`capture.enabled` value, no `onboarding.json` receipt, no folders and no nodes. Capture into it
starts as soon as the person turns capture on (`settings.set` `capture.enabled` `on`,
`knode spaces capture <space> on`), onboarding sets it up, a folder is added, or anything is
written into it. A space capture has already written to has nodes, so it never waits.

When the person turns capture on for a space whose capture was off or waiting for setup, the
service records the time in the setting `capture.since` (RFC 3339, UTC), and that space receives
only conversations started at or after it: "from now on" does not bring every earlier one. When
the wait ends any other way (onboarding, a folder, a note), earlier sessions are caught up as
before.

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

## Sources

A *source* brings things into a space: an *importer* runs once on what the person gives it
(`import.run`), a *connector* keeps running in Knode Service and brings in what is new (a chat
app, a mail store, a messaging API). Every connector is off until the person connects it to a
space. All `sources.*` methods are for people only; `space` is the space's path and `source` the
source's id (`whatsapp`, `apple-mail`...).

| Method | Params | Result |
| --- | --- | --- |
| `sources.list` | `space` | Every source of the space, connectors then importers, as below |
| `sources.status` | `space`?, `source`? | `[{ "space", "name", "sources": [...] }]`: the connectors connected to the space, or with no `space` to every space the service knows that has one |
| `sources.connect` | `space`, `source`, `history`?, `on`?, `by` | The source. Connects it with `history` `from-today` (the default) or `everything`, and turns it on. With `on` `false` it is connected paused: its history and the day it was connected are fixed and `sources.containers` and `sources.exclude` work, but nothing is read until `sources.resume`, so what to leave out can be chosen first (connecting paused a source already connected pauses it). The history is chosen once: asking for another one for a connected source fails with `invalid` |
| `sources.pause` / `sources.resume` | `space`, `source`, `by` | The source, off or on again |
| `sources.exclude` | `space`, `source`, `id`, `name`?, `by` | The source. The chat, channel or folder `id` is never read again; what came from it stays |
| `sources.include` | `space`, `source`, `id`, `by` | The source, reading `id` again from now on |
| `sources.containers` | `space`, `source` | `[{ "id", "name", "excluded" }]`: the chats, channels or folders the connector has now, to choose what to exclude |
| `sources.run` | `space`, `source` | The source. Runs it now, without waiting for it to finish; for a connector the app runs (`calendar`), sets its `next_run` to now so the app runs it |
| `sources.disconnect` | `space`, `source`, `by` | `true`. Removes its settings and forgets its state on this machine; what it brought in stays. A connector signed in to an account is signed out, and its session forgotten, once no space the service knows has it connected (a space it cannot read counts as having it) |
| `sources.account` | `source` | Whether the connector is signed in to the account it reads, on this machine: `{ "state": "signed-out" }`, `{ "state": "code-sent", "via"? }`, `{ "state": "password-needed", "hint"? }`, `{ "state": "open-browser", "url" }` or `{ "state": "signed-in", "account" }`. `unavailable` for a connector with no account |
| `sources.sign_in` | `source`, `step` | The state after one step of signing in. `step` is `{ "step": "start", "account" }` (a phone number), then `{ "step": "code", "code" }`, then, when the account has one, `{ "step": "password", "password" }`; for `imap` the account (and the server, `you@example.com imap.example.com`) then its app password; for `gmail`, `start` (with the address or `""`) answers `open-browser` and `{ "step": "browser" }` is sent once the person signed in on that page. A wrong step fails with `sign-in`; the session is kept only in the system keychain |
| `sources.sign_out` | `source` | Whether it was signed in. Ends the session with the service and forgets it |

```json
{ "id": "whatsapp", "name": "WhatsApp", "kind": "connector", "reads": "The chats of WhatsApp for Mac",
  "permissions": ["full-disk-access"], "container": "chat", "state": "idle",
  "setting": { "on": true, "history": "from-today", "connected_at": 1763372400000,
               "since": 1763337600000, "exclude": [{ "id": "123@g.us", "name": "Family" }] },
  "last_run": 1763380000000, "last_success": 1763380000000, "last_items": 4, "items": 120,
  "next_run": 1763380300000 }
```

- `kind` is `importer` or `connector`. `permissions` are among `full-disk-access`, `sign-in`,
  `network`, `calendar`, `contacts`, `microphone` and `photos`.
- `state` is `importer`, `off` (not connected), `paused`, `unavailable` (connected, but this
  machine has no such connector), `stopped` (`sources` or `capture` is off in `config.toml`),
  `running`, `idle`, `failing` (its last run failed; it tries again later, waiting twice as long
  after each error in a row, up to an hour), `needs-update` (the other app's database changed:
  nothing is written until Knode is updated) or `needs-attention` (a permission or a sign-in is
  missing). `last_error` is `{ "kind", "message", "at" }`. `notice`, when there is one, is
  what the connector said after its last run that went well, such as what it could not bring
  in yet.
- Times are milliseconds since 1970. With `from-today`, nothing older than `since`, the start of
  the day it was connected, is brought in.

The settings of a connector are the person's and belong to the space: they are kept in its
`source.<id>` setting, as the JSON of `setting`, and changing them is announced with a `space`
notification (`settings`, `key` `source.<id>`). What a connector did on this machine (where it
stopped, when it ran, its errors) is kept in `$KNODE_HOME/sources/state.json`, which only its
owner can read; it never holds what was read. The cursor is saved only after what was read is
written, and the importers find what they wrote by the service's own ids, so a restart, a lost
state file or a second device running the same connector writes nothing twice.

A connector runs when the files it watches change (a local database and its `-wal`), at its
interval, and when `sources.run` asks. A file event runs it only when the size or modification
time of the files or their `-wal` changed since its last run, so its own reads of them (which
macOS reports as events) do not run it again. macOS reports writes to a file another app keeps
open late, so a connector that watches files is also looked at once a minute, and runs then
when those files changed, or when its last run did not read everything (it ran long enough, it
failed, or it waits for a file the app has not written yet). A pause, an exclusion or a
disconnect made while a connector runs counts from its next batch: a batch read before it is
not written, and nothing of a source disconnected meanwhile is saved. Connectors that read
another app's database read a copy of it, never the live file: a copy made while the app wrote
the database or its `-wal` is made again, and that copy is checked with SQLite's `quick_check`
and not read when it fails (the run fails and is tried again later). Only a file that is not a
database this SQLite reads is reported as `needs-update`; other errors opening the copy fail
the run, which is tried again. They check its tables and columns first. Their writes are made with
the provenance `{ "kind": "system", "actor": "<source id>" }`. Secrets connectors need are kept
in the system credential store (the Keychain on macOS, the Secret Service on Linux), never in a
space, a settings file or a log. Where Linux has no Secret Service, a connector that needs a
secret cannot sign in and says to install one (GNOME Keyring or KeePassXC), unless
`source_secrets_in_files = true` in `config.toml` lets it keep them in a file only its owner can
read in `$KNODE_HOME/agent-keys/`. `sources.containers` does not keep the space locked while the
connector lists its chats, which may take long over the network. `sources = false` in `config.toml` stops every connector; so does `capture = false`, which
stops everything Knode Service brings in by itself.

The `telegram` connector reads Telegram through its official API, as Knode's own client. It is
signed in once with `sources.sign_in`; while the service runs it keeps a connection open and
brings in new messages as they arrive, and when the service starts it catches up from the last
message it brought in of each chat. Messages go through the Telegram Desktop export importer
and find the same `chat` nodes by Telegram's chat id. Chats are excluded by that id (as
`sources.containers` lists them); secret chats never reach the API and are not read. Photos and
files up to 20 MB are attached; larger ones are named in the text. When Telegram asks to wait
(a flood wait), the connector waits up to 10 seconds within a run, and otherwise asks nothing
until the wait is over. A build of Knode without a Telegram app id lists the connector, which
reports `unavailable`.

The `imap` connector reads any mail account over IMAP with TLS (port 993; never plain text or
`STARTTLS`), signed in with the address and an app password, checked by signing in once. Every
minute it asks each mailbox for its `UIDVALIDITY` and `UIDNEXT` and reads only what is new, from
the last UID read, without marking anything read; a changed `UIDVALIDITY` reads the mailbox
again. On Gmail (`X-GM-EXT-1`) it reads only `All Mail`, with each message's labels as its
folders. Mailboxes are excluded by their name on the server; spam and trash are never read.

The `gmail` connector reads Gmail through the Gmail API after Sign in with Google (OAuth with
PKCE and a loopback redirect to `http://127.0.0.1:<port>`, scope `gmail.readonly`). The first
run lists the messages (from the day it was connected with `from-today`), later runs follow
`users.history.list`; when Google no longer has the history it lists again what arrived from a
day before the last good run, at most 2000 messages. Labels are excluded by their id (`INBOX`,
`Label_12`); spam and trash are never read. A build of Knode without a Google client id
(`KNODE_GOOGLE_CLIENT_ID` when it is built, or at run time) lists the connector, which reports
`unavailable`.

Both write through the mail importer: one `email` node per thread, and a message already in the
space from a Google Takeout `.mbox`, Apple Mail or the other connector is found by its
`Message-ID` and not added again. Their secrets (the app password, the refresh token) are kept
only in the system keychain, and are forgotten when the connector signs out or is disconnected
from the last space that had it.

The `calendar` connector is run by the Knode app, not by the service (see Calendar, below).

## Calendar

The calendars of a Mac are reached only through EventKit, which only the app can use. So the
`calendar` connector is *app-driven*: Knode Service lists it with the other connectors
(`permissions` `["calendar"]`, `container` `calendar`), keeps its settings in each space
(`source.calendar`: on, the history, the calendars excluded) and its state on this machine, and
never runs it itself. Instead it asks the app to run it by setting its `next_run` (after a good
run, 15 minutes later; at once after `sources.run` or `calendar.configure`). The app asks each
space what it wants (`calendar.plan`), reads those calendars and days with EventKit, and hands the
events to `calendar.sync`, which writes them with the provenance
`{ "kind": "system", "actor": "calendar" }`. Without calendar access the app says so with
`calendar.fail`, and the source shows `needs-attention`. `sources.containers` lists the calendars
the app last reported with `calendar.calendars`. All `calendar.*` methods are for people only.

| Method | Params | Result |
| --- | --- | --- |
| `calendar.plan` | `space` | `{ "on", "start", "end", "exclude", "attendee_emails", "next_run"? }`: whether to read anything for the space (off when paused or when `sources` or `capture` is off in `config.toml`), the days to read (Unix milliseconds), the ids of the calendars excluded (never read), whether attendees' addresses are written, and when the service asks for the next run. Fails with `invalid` when the source is not connected. |
| `calendar.calendars` | `calendars` (`[{ "id", "name" }]`) | `true`. The calendars this Mac has, for `sources.containers`. Kept in memory. |
| `calendar.sync` | `space`, `calendars`, `start`?, `end`?, `events` | `{ "written": { "created", "updated", "unchanged", "skipped" }, "missing", "source" }`, `source` as `sources.list` shows it. `calendars` are the calendars read for this request (`[{ "id", "name" }]`, never the excluded ones): only their events can be missing. Writes nothing when the source is paused or sources are off. The window is the plan's, narrowed to `start`/`end` when given. `missing` lists the events the space has, of a calendar read and in the window, that are not among `events` (`[{ "key", "external_id", "occurrence"?, "calendar_id" }]`); nothing is marked for them (see below). Records the run (`last_run`, `items`, the cursor `{ "start", "end" }`) and asks for the next one in 15 minutes. |
| `calendar.remove` | `space`, `keys` | `{ "removed", "source" }`. Marks the events with these keys `removed`: those of `missing` the app found gone from their calendar. Events of excluded calendars, events already removed and unknown keys are left alone; nothing is marked when the source is paused or sources are off. Adds the events marked to the run's `items`. |
| `calendar.fail` | `space`, `kind` (`permission`, `unavailable` or `failed`), `message` | The source, with the run failed: `permission` and `unavailable` show `needs-attention`. |
| `calendar.current` | `space`, `at`? (now) | The event happening at `at`, or `null`: a timed event (not all-day, cancelled or removed) that starts at most 10 minutes after `at` and has not ended; one with a call link first, then the one that started closest. `{ "id", "title", "start", "end", "all_day", "calendar"?, "call_link"?, "status", "project"? }`. |
| `calendar.link` | `space`, `id` (a recording or any node), `event`, `project`?, `by` | The event. Links `id` to it `related` and, when `project` is given and the event is part of no project, the event `part-of` it. Fails with `invalid` for a node that is not an event. |
| `calendar.options` | `space` | `{ "days_back", "days_ahead", "attendee_emails" }`: 30, 14 and `false` unless changed. Kept in the space setting `calendar.options` (JSON; absent for the defaults). |
| `calendar.configure` | `space`, `days_back`?, `days_ahead`?, `attendee_emails`?, `by` | The options, after the change. `days_back` is at most 3650, `days_ahead` at most 365. |

An event in `calendar.sync`:

```json
{ "external_id": "040000008200E00074C5B7101A82E008…", "occurrence": 1795510800000,
  "calendar_id": "8F2C…", "calendar": "Work", "title": "Weekly sync",
  "start": 1795510800000, "end": 1795514400000, "all_day": false,
  "location": "Room 3", "url": "https://zoom.us/j/123", "notes": "Agenda…",
  "attendees": [{ "name": "Grace Hopper", "email": "grace@example.com" }], "status": "confirmed" }
```

`external_id` is EventKit's `calendarItemExternalIdentifier`, the same on every device;
`occurrence`, only for an occurrence of a recurring event, is when it was planned to start (it
does not move when that occurrence is moved). `status` is `confirmed`, `tentative` or
`cancelled`.

Each event is one node of kind `event`, found again by its key, `event.key`: `external_id`, then
`@<occurrence>` for an occurrence. Its title is the event's title and the local day it starts,
`Weekly sync (2026-11-24)`; its body says when, where, the call link, who and the calendar, then
the notes, cut at 1500 characters. Its properties: `event.key`, `event.external_id`,
`event.title`, `event.calendar_id`, `event.calendar`, `event.start`, `event.end`, `event.day`
(`YYYY-MM-DD`), `event.all_day`, `event.recurring`, `event.status` (`confirmed`, `tentative`,
`cancelled` or `removed`), `event.location`, `event.call_link` (a Zoom, Google Meet, Teams,
Webex, Whereby, Jitsi, FaceTime or similar link found in the event's link, place or notes),
`event.attendees` and, only when the space asks, `event.attendee_emails` (JSON lists). Attendees
are named; one known only by address is left out unless the space asks for addresses, and one
named like a `person` node of the space is written as a `[[link]]` to it.

Writing the same events again changes nothing; an event that changed is brought in step. Events
outside the window, of excluded calendars, or cancelled before they were ever written are left
out. A cancelled event is marked (`**Cancelled.**` and its status), never deleted. When two events
would have the same title (same title, same day), the later gets `Weekly sync (2026-11-24) 2`,
then ` 3`, so a `[[link]]` finds one node. Events of a calendar excluded or not read are never
changed.

An event the space has, of a calendar read and inside the window, that is no longer among the
events read is either gone from its calendar or was moved outside the window (the window is
checked against where the space last saw it). So `calendar.sync` does not mark it: it lists it in
`missing`, and the app asks EventKit (`calendarItems(withExternalIdentifier:)`, among the
calendars read) whether the event is still there: an event that does not repeat is gone when
there is no such item; an occurrence is gone when its series is gone, or when the series is
there but no occurrence planned at `occurrence` is (neither at its planned time nor detached and
moved elsewhere). The app sends the keys found gone to `calendar.remove`, which marks them
`removed` (`**Removed from the calendar.**`); one that comes back is restored. An event that only
moved is left as it was, and brought in step when the window reaches it again.

The app hands a window over in batches of at most 31 days, each a `calendar.sync` with its own
`start` and `end`, so a request stays well below the 16 MB a line may have; it sends at most the
first 1600 characters of an event's notes (the body keeps 1500).

The window: from `days_back` days before the start of today (with the history `from-today`,
never before the day the source was connected) to `days_ahead` days after the end of today.

## Search

`search` finds nodes by words (full-text search), by meaning, or both ranked together with
reciprocal rank fusion, as section 19 of the
[.knode specification](https://github.com/knodemd/format) describes. `mode` picks one: `words`,
`meaning` or `hybrid`, the default. Each hit is
`{ "node", "snippet", "score", "word_rank", "meaning_rank" }`: the node summary, the text around
the words found (matches in `**bold**`) or the start of its nearest passage, the fusion score
(higher is better) and its place in the words and meaning lists, from 1, or `null` when it is
not in one. In `hybrid`, the score of a transcript (a node of kind `conversation`, `chat` or
`email`) is halved, so a long transcript that matches in passing comes after the short note that
answers the query.

Search by meaning runs on this machine, with a small embedding model
(`multilingual-e5-small`, about 490 MB, around 100 languages). Nothing is sent anywhere:

- **The model** is downloaded once, in the background, into `$KNODE_HOME/models`, the first time
  there is something to index, and each file is checked against its SHA-256. It is loaded when
  there is work or a search, and let go after ten minutes without either.
- **Indexing** runs in the background for each open space: after it opens, and a couple of
  seconds after each change. The model runs without holding the space, so writes never wait
  for it; a node that changed meanwhile is indexed again. It pauses while the machine runs on
  battery, unless `index.run` asks for it.
- **`embeddings = "off"`** in `config.toml` (or the environment variable `KNODE_EMBEDDINGS=off`)
  turns it off: nothing is downloaded or indexed, and `search` is by words. Vectors already
  stored stay in the space. It is on when left out.
- **Fallback.** When meaning cannot be used yet (the model is downloading, the space is not
  indexed yet, it is off, or this build has no search by meaning), `meaning` and `hybrid` run as
  `words`, the result's `mode` says `words` and `note` says why. When some nodes are not indexed
  yet, `hybrid` finds them by words only and `note` says how many.
- **Builds.** Search by meaning is the `semantic` feature of `knode-service`, on by default. Without
  it, search is by words.

`index.status` answers:

```json
{ "available": true, "enabled": true, "running": true,
  "model": { "name": "multilingual-e5-small", "state": "downloading",
             "downloaded": 120000000, "size": 487724985 },
  "paused": "on battery",
  "spaces": [ { "space": "/Users/ada/Knode/Work.knode", "nodes": 1200, "waiting": 300,
                "indexing": true } ] }
```

- `available`: whether this build can search by meaning. `enabled`: `embeddings` in
  `config.toml`. `running`: whether this service indexes at all.
- `model.state`: `unavailable`, `missing`, `downloading`, `downloaded`, `loading`, `ready` or
  `failed` (with `error`; it is tried again ten minutes later, or at once with `index.run`).
- `paused`, when indexing waits, says why.
- Each open space: its `nodes`, how many are `waiting` to be indexed, whether it is `indexing`
  now or soon, and the `error` of its last run, if any.

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
| -32024 | `folder` | A folder of Markdown files, a data export or a file to import could not be read or written, or a web page could not be fetched. |
| -32025 | `no_access` | macOS keeps Knode Service from reading another app's notes (Apple Notes, Bear). The message says how to turn on Full Disk Access for the app that runs Knode, or to import an export instead. |
| -32030 | `unknown_version` | An edit was based on a version of a document the service does not have. Open it again with `doc.open`. |
| -32031 | `missing_history` | Loro updates depend on edits the document does not have yet. Send them from an earlier version. |

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
