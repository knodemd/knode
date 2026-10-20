# Knode for Mac

Knode for Mac, in SwiftUI: keep several spaces and switch between them, browse a space's notes
newest first, read and edit a note in Markdown with a live preview, search, and see the history of a
note: who changed it, when, and what. A menu bar item shows Knode Service, and the app carries the
`knode` command inside.

## Spaces

The sidebar lists every space you created or opened, like libraries in Apple Music, until you
remove it from the list (which never touches its files). The list is kept in the app's settings;
the first time, it holds the default space, the recent spaces and the spaces in `~/Knode`.

- **New Space…** (Shift-Command-N) asks for a name and a folder, `~/Knode` by default, and can make
  the new space the default one.
- Command-1 to Command-9 switch to the first nine spaces; clicking a space opens it.
- The **Space** menu and the context menu of a space rename it (its name in `manifest.json`; the
  package keeps its file name), choose its icon (one emoji, the `space.icon` setting of the space),
  make it the default space for agents and capture, show it in the Finder, or remove it from the
  list.
- The default space is `default_space` in `$KNODE_HOME/config.toml`, the one the `knode` command,
  the MCP server and capture use. The sidebar marks it with a sparkle, and **Space for Agents** in
  the menu bar menu switches it without restarting Knode Service.

## History

**Show History** (Command-Y) opens the history of the open note beside it: every version, newest
first, with the person or agent who made it (and the agent's session), when, and how many lines it
added and removed. Choosing a version compares it with the note as it is now, line by line or side
by side, and offers:

- **Restore This Version**: the whole note goes back to that version, written as a new version, so
  nothing is lost.
- **Undo This Change**: only what that change did is undone. When later changes touched the same
  note the core refuses, and the app says so and asks before undoing it anyway.

Unsaved edits are saved before a restore or an undo.

**Undo Last Change to Note** (Option-Command-Z, in the Edit menu) undoes the newest edit or restore
of the open note made by the person using the app, through the history, even one made before the
note was opened. **Redo Last Change to Note** (Shift-Option-Command-Z) undoes that undo, until the
person edits the note again. Changes made by agents are never picked; they are undone from the
history panel. When someone else changed the note after the picked change, the app asks first; the
person's own undone edits and undos in between do not count. Command-Z and Shift-Command-Z stay
the text's own undo and redo while typing.

## Agents and access

**Settings > Agents** lists the agents of this machine: their kind, when they were added and
whether their key is active (by fingerprint; keys stay in the Keychain). For each space it sets
**No Access**, **Read** or **Read and Write**, and **Revoke** stops every key of an agent and takes
away its access everywhere. **Agent Access** in the Space menu, and in a space's context menu,
does the same for one space. **Connect an Agent** runs `knode connect <agent>`, or first only shows
what it would do, and shows what the command printed.

All of it goes through Knode Service, as the person (`agents.list`, `grants.set`,
`agents.revoke`); when the service is not running, the screen offers to start it. Grants are not a
sandbox, and the screen says so.

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

History, the changes of a note, restore and undo go through the service when it runs, and through
the core when it does not. Knode Service has no methods yet to rename a space or change its
settings. For those, `RoutingSpaceStore` opens the space with the core in the app for the call, even
while the service holds it (SQLite allows both), and the app reloads what it shows.

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
| `Knode/Store` | `SpaceStore`, the models, `CoreSpaceStore`, `RoutingSpaceStore`, `PreviewSpaceStore`, the list of spaces (`SpaceLibrary`), the recent spaces and the default space (`KnodeSettings`) |
| `Knode/Service` | The socket client, `ServiceSpaceStore`, the agents and grants (`AgentDirectory`), the menu bar status and the `knode` runner |
| `Knode/CommandLineTool` | Where the `knode` command is, and installing and uninstalling its link |
| `Knode/Models` | `AppModel` (spaces, list, search), `NoteEditorModel` (one open note) `NoteHistoryModel` (its history, and undoing the last change) and `AgentsModel` (agents and their access) |
| `Knode/Markdown` | The block parser behind the preview and `[[links]]` |
| `Knode/Views` | The window, the note editor, the history, the agents settings, the space sheets, the menu bar menu and the open panels |
| `KnodeTests` | Tests for the stores (against a fake Knode Service), the view models, the command installer and the Markdown preview |
| `scripts` | The build of the core, of the `knode` command and of the document icon |
| `project.yml` | The [XcodeGen](https://github.com/yonaskolb/XcodeGen) source of `Knode.xcodeproj` |

`Knode.xcodeproj` is generated: after adding or moving files, run `xcodegen` here and commit both.

## Brand

Colours, Knode Block and Knode Line come from the `KnodeBrand` Swift package in the brand kit; the
app icon and the `.knode` document icon come from `brand/07 App Icon`, and the menu bar icon from
`KnodeMenuBarIcon`. The app declares the
`md.knode.space` type for `.knode` packages.
