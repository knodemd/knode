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
- With `read`, `space.open`, `node.get`, `node.find_by_title`, `links.from`, `links.back`,
  `links.unresolved`, `search`, `recent`, `history` and `subscribe` are allowed; writes fail with
  `forbidden`.
- With `write`, `node.create`, `node.update`, `node.delete`, `link.create` and `link.delete` are
  allowed too.
- Creating spaces, managing agents, keys and grants, and every other method are for people only
  and fail with `forbidden`.
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
| `node.create` | `space`, `title`, `body`?, `kind`? (default `note`), `tags`?, `by` | The node. |
| `node.get` | `space`, `id` | The node, or `null` when no node has this id. |
| `node.find_by_title` | `space`, `title` | Node summaries with this title, oldest first. Titles are compared without regard to case or extra spaces; a `[[link]]` with this title points at the first one. |
| `node.update` | `space`, `id`, `title`?, `body`?, `kind`?, `tags`?, `by` | The node as it is now. Fields left out stay as they are. |
| `node.delete` | `space`, `id`, `by` | `null`. |
| `link.create` | `space`, `from`, `to`, `kind`? (default `related`), `by` | The link. |
| `link.delete` | `space`, `from`, `to`, `kind`? (default `related`) | `true` if there was such a link. |
| `links.from` | `space`, `id` | The links that start from the node. |
| `links.back` | `space`, `id` | The links that point at the node. |
| `links.unresolved` | `space`, `id` | The `[[links]]` in the node's text that name no node yet, as written. |
| `search` | `space`, `query`, `limit`? (default 20, at most 1000) | Search hits, best first. |
| `recent` | `space`, `limit`? (default 20, at most 1000) | Node summaries, newest change first. |
| `history` | `space`, `id` | Every version of the node, oldest first. |
| `subscribe` | `space`? | `true`. Starts `changed` notifications; see below. |
| `unsubscribe` | none | `true`. Stops them. |
| `service.status` | none | `{ "version", "pid", "spaces", "clients", "capture" }`. `capture` is the space conversations are captured into, or `null`. An agent only sees the spaces it may read. |
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
- `op` is `create`, `update`, `delete`, `link` (a link from `node_id` was made) or `unlink` (one
  was removed). Renaming a node also sends `update` for each node whose `[[links]]` were rewritten
  to the new title.
- A write that changes nothing sends nothing.
- A connection that falls more than 1024 changes behind receives
  `{"jsonrpc":"2.0","method":"lagged","params":{"missed":12}}` and should reload what it shows.

## Capture

Knode Service captures every Claude Code and Codex conversation into the default space: one
`conversation` node per session, kept current while the agent writes it. It reads
`$KNODE_HOME/config.toml` when it starts and again every few seconds:

- `default_space` is the space conversations go to. With none, nothing is captured.
- `capture = false` turns capture off. It is on when left out.
- `capture_exclude = ["/path/to/project"]` leaves out the conversations held in these projects
  and in every folder inside them.

Capture follows `~/.claude/projects` (or `$CLAUDE_CONFIG_DIR/projects`) and `~/.codex/sessions`
(or `$CODEX_HOME/sessions`). What it has read is saved in `$KNODE_HOME/capture/state.json`, so
after a restart it only reads what is new. Its writes are made with the provenance
`{ "kind": "agent", "actor": "claude-code" | "codex", "session": "<session id>" }`, go through
the same space as every client's, one at a time, and are announced with `changed` like any other
write.

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
| -32001 | `not_found` | No node has this id. |
| -32002 | `not_a_space` | The path is not a `.knode` package. |
| -32003 | `already_exists` | A space already exists at this path. |
| -32004 | `invalid` | A value breaks a rule of the format, such as an empty title or a bad kind. |
| -32005 | `unsupported` | The space was written by a newer version of Knode, or its files do not agree. |
| -32006 | `storage` | The files of the space could not be read or written. |
| -32010 | `unauthenticated` | The connection has not authenticated, or its key is unknown or was revoked. |
| -32011 | `forbidden` | An agent wrote where it may only read, or called a method for people only. |

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
