# AGENTS.md

Knode: your second brain for AI. A Rust core with native apps around it.

- `core/` is a Cargo workspace; run `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`
  and `cargo fmt --all --check` from `core/` before every pull request.
- `mac/`, `ios/`, `web/`, `android/`, `windows/` are the native layers. Product logic belongs in the core.
- `brand/` is a git submodule of knodemd/brand: run `git submodule update --init` after cloning, and
  never edit files inside it here.
- Everything in this repository is in English. Conventional Commits, one issue per change, pull
  requests only. See the [handbook](https://github.com/knodemd/handbook).
- A change users see (a command, an MCP tool, the app, the format) comes with a pull request in
  [knodemd/docs](https://github.com/knodemd/docs), linked from this one.
