<img src="https://raw.githubusercontent.com/knodemd/brand/main/09%20Banners/README%20Headers/PNG/readme-knode.png" alt="Knode" width="100%">

# Knode

Your second brain for AI: one local memory that is yours, that every AI agent you use can read and
add to, and that you see and organise in a native app. [knode.md](https://knode.md)

Knode is in design and built in public. Nothing to install yet: follow along in the
[roadmap](https://github.com/knodemd/handbook/blob/main/06-roadmap.md).

## What's here

| Path | What |
|---|---|
| `core/` | The Rust core, one crate per piece: format, Markdown, capture, service, MCP, sync, bindings, CLI |
| `mac/` | Knode for Mac, with Knode Service in the menu bar |
| `ios/` | Knode for iPhone and iPad |
| `web/` | The web app, app.knode.md |
| `android/`, `windows/` | After v1.0 |
| `docs/` | [Product](docs/PRD.md), [architecture](docs/ARCHITECTURE.md), [privacy](docs/PRIVACY.md) |
| `brand/` | The Knode brand, as a git submodule |

The `.knode` format is specified in [knodemd/format](https://github.com/knodemd/format).

## Build

```sh
git clone --recurse-submodules https://github.com/knodemd/knode.git
cd knode/core
cargo test
cargo run -p knode-cli
```

Only working on one app? Clone just the core and that app:

```sh
git clone --filter=blob:none --sparse https://github.com/knodemd/knode.git
cd knode && git sparse-checkout set core mac
```

## Contributing

Read the [contributing guide](https://github.com/knodemd/.github/blob/main/CONTRIBUTING.md) and the
[handbook](https://github.com/knodemd/handbook). Every change starts with an issue.

## Licence

Code under [Apache 2.0](LICENSE); keep the [NOTICE](NOTICE). The Knode name, logo, icons and typefaces
follow the [Knode Brand License](BRAND-LICENSE.md).
