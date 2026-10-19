# Knode for Mac

Knode for Mac, in SwiftUI: open a space, browse its notes newest first, read and edit a note in
Markdown with a live preview, and search. A menu bar item shows Knode Service, and the app carries
the `knode` command inside.

The app talks to a space through the `SpaceStore` protocol (`Knode/Store/SpaceStore.swift`):

- `ServiceSpaceStore` talks to Knode Service over its Unix socket
  ([protocol](../core/crates/knode-service/PROTOCOL.md)), so the app and the agents write to the
  same space and the app shows their changes as they happen.
- `CoreSpaceStore` opens the space with the Rust core inside the app, for when the service is not
  running.
- `RoutingSpaceStore`, the one the app uses, picks between the two: the service when it runs, the
  core when it does not, and it moves open spaces between them when the service starts or stops.
- `PreviewSpaceStore` keeps example notes in memory, for SwiftUI previews, tests and the example
  space.

Every change the app makes is recorded as made by the macOS user.

## Menu bar and the knode command

The menu bar item shows whether Knode Service is running, the spaces it has open and how many
clients are connected, kept up to date over the socket. Its menu opens the app, starts or stops the
service (`knode service start` and `knode service stop`) and turns on running it at login
(`knode service install` and `knode service uninstall`). The icon is dimmed while the service is
stopped. The app keeps working without the service.

The socket is `$KNODE_HOME/service.sock`; the Knode home is `KNODE_HOME` when it is set, else
`~/Library/Application Support/Knode`.

The `knode` command is at `Knode.app/Contents/Helpers/knode`. On the first run, and from
**Knode > Install Command Line Tool…**, the app offers to link it as `/usr/local/bin/knode` (or
`/opt/homebrew/bin/knode` when only that folder is writable, else with an administrator's
password). When a `knode` is already installed, for example with Homebrew, the app uses that one
and says so. **Uninstall Command Line Tool…** removes only the link the app made.

## Requirements

- macOS 26 or later
- Xcode 26 or later
- Rust, from [rustup.rs](https://rustup.rs). The build adds the `aarch64-apple-darwin` and
  `x86_64-apple-darwin` targets with `rustup` if they are missing.
- The brand kit, a git submodule of this repository:

  ```sh
  git submodule update --init
  ```

## Build and run

From this folder:

```sh
xcodebuild -scheme Knode -destination 'platform=macOS' build test
```

or open `Knode.xcodeproj` in Xcode and run the `Knode` scheme. Local builds are signed ad hoc, so no
Apple developer team is needed.

To open a space at launch, pass `-KnodeOpenSpace <path to the .knode package>` as a launch argument;
`-KnodeOpenExample YES` opens the example space instead.

## How the core is built

The `KnodeCore` target runs `scripts/build-xcframework.sh` before it compiles. The script:

1. builds `knode-ffi` (`core/crates/knode-ffi`) as a static library in release mode for Apple
   silicon and Intel, and joins the two with `lipo`;
2. generates the Swift bindings with UniFFI, using the `uniffi-bindgen` binary of `knode-ffi`, so
   the generator always matches the library;
3. packages `build/KnodeCore/KnodeCore.xcframework` (the library and the `KnodeCoreFFI` C module)
   and `build/KnodeCore/KnodeCore.swift`, which the `KnodeCore` target compiles.

`build/` is ignored: the framework and the bindings are made by the build and never committed.
Xcode reruns the script only when a file in `scripts/knode-core-inputs.xcfilelist` changes; the
script fails if a source file of `knode-ffi`, `knode-config`, `knode-format` or `knode-md` is missing from that
list. Cargo builds into `build/cargo`, apart from `core/target`. The script also runs on its own:

```sh
scripts/build-xcframework.sh
```

## How the knode command is built

The `KnodeCLI` target runs `scripts/build-knode-cli.sh`, which builds `knode-cli`
(`core/crates/knode-cli`) in release mode for Apple silicon and Intel and joins the two with
`lipo` into `build/KnodeCLI/knode`. The `Knode` target copies it to `Contents/Helpers` and signs it
with the identity of the app and the hardened runtime (`scripts/embed-knode-cli.sh`): ad hoc for
local builds. Like the core, it is
rebuilt only when a file in `scripts/knode-cli-inputs.xcfilelist` changes, and the script fails if a
source file of `knode-cli` or of a crate it uses is missing from that list.

## Layout

| Path | What it holds |
|---|---|
| `Knode/Store` | `SpaceStore`, the models, `CoreSpaceStore`, `RoutingSpaceStore`, `PreviewSpaceStore` and the recent spaces |
| `Knode/Service` | The socket client, `ServiceSpaceStore`, the menu bar status and the `knode` runner |
| `Knode/CommandLineTool` | Where the `knode` command is, and installing and uninstalling its link |
| `Knode/Models` | `AppModel` (spaces, list, search) and `NoteEditorModel` (one open note) |
| `Knode/Markdown` | The block parser behind the preview and `[[links]]` |
| `Knode/Views` | The window, the note editor, the menu bar menu and the open and save panels |
| `KnodeTests` | Tests for the stores (against a fake Knode Service), the view models, the command installer and the Markdown preview |
| `scripts` | The build of the core, of the `knode` command and of the document icon |
| `project.yml` | The [XcodeGen](https://github.com/yonaskolb/XcodeGen) source of `Knode.xcodeproj` |

`Knode.xcodeproj` is generated: after adding or moving files, run `xcodegen` here and commit both.

## Brand

Colours, Knode Block and Knode Line come from the `KnodeBrand` Swift package in the brand kit; the
app icon and the `.knode` document icon come from `brand/07 App Icon`, and the menu bar icon from
`KnodeMenuBarIcon`. The app declares the
`md.knode.space` type for `.knode` packages.
