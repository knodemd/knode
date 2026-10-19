# Changelog

Changes people notice in Knode: the app, the `knode` command, the MCP server and the `.knode`
format. Every version follows [semantic versioning](https://semver.org), and the format of this file
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- `knode service run`, `start`, `stop`, `status`, `install` and `uninstall`: run Knode Service,
  see what it is doing, and start it at login with a launch agent on macOS or a systemd user unit
  on Linux.
- `knode mcp`, `knode search`, `knode import` and `knode init` go through Knode Service, starting
  it in the background when it is not running, so agents, the CLI and the app write through one
  writer and see each other's changes at once. If the service cannot run, they open the space
  directly and say so.
- Knode Service captures every Claude Code and Codex conversation into the default space and keeps
  it current while the agent writes. `capture = false` in `config.toml` turns it off and
  `capture_exclude = ["/path/to/project"]` leaves projects out.
