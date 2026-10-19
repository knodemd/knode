# Knode - architecture

What someone needs before their first change. The full reasoning is in
[decision 0005](https://github.com/knodemd/handbook/blob/main/decisions/0005-architecture.md).

## The shape

One Rust core (`core/`) owns the data, the files, the search, the capture and the sync. One native app
per platform (`mac/`, `ios/`, `web/`, later `android/` and `windows/`) owns the screens and calls the
core through generated bindings. Nothing platform specific in the core; nothing product deciding in
an app.

## The core

| Crate | What it does |
|---|---|
| `knode-format` | Reads and writes `.knode` spaces: nodes, links, provenance, history, search |
| `knode-md` | Converts a space to Markdown and back |
| `knode-ingest` | Captures agent conversations; imports notes and chat exports |
| `knode-service` | The one process that opens spaces, for the app and the agents |
| `knode-mcp` | The MCP server agents use |
| `knode-sync` | Sync between devices with Loro updates |
| `knode-ffi` | Bindings for Swift, Kotlin and the web, generated with UniFFI |
| `knode-cli` | The `knode` command |

## The .knode file

A package the Finder shows as one file: `manifest.json`, `knode.db` (SQLite in WAL mode with full-text
and vector search), `blobs/` for attachments and `sync/` for Loro updates. The specification lives in
[knodemd/format](https://github.com/knodemd/format).

## Data and privacy

No telemetry. See [PRIVACY.md](PRIVACY.md).
