# Knode for Mac

Knode for Mac, in SwiftUI: open a space, browse its notes newest first, read and edit a note in
Markdown with a live preview, and search.

The app talks to a space through the `SpaceStore` protocol (`Knode/Store/SpaceStore.swift`).
`CoreSpaceStore` implements it with the Rust core, so the app reads and writes real `.knode`
packages; every change is recorded as made by the macOS user. `PreviewSpaceStore` keeps example
notes in memory, for SwiftUI previews, tests and the example space.

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
script fails if a source file of `knode-ffi`, `knode-format` or `knode-md` is missing from that
list. Cargo builds into `build/cargo`, apart from `core/target`. The script also runs on its own:

```sh
scripts/build-xcframework.sh
```

## Layout

| Path | What it holds |
|---|---|
| `Knode/Store` | `SpaceStore`, the models, `CoreSpaceStore`, `PreviewSpaceStore` and the recent spaces |
| `Knode/Models` | `AppModel` (spaces, list, search) and `NoteEditorModel` (one open note) |
| `Knode/Markdown` | The block parser behind the preview and `[[links]]` |
| `Knode/Views` | The window, the note editor and the open and save panels |
| `KnodeTests` | Tests for both stores, the view models and the Markdown preview |
| `scripts` | The build of the core and of the document icon |
| `project.yml` | The [XcodeGen](https://github.com/yonaskolb/XcodeGen) source of `Knode.xcodeproj` |

`Knode.xcodeproj` is generated: after adding or moving files, run `xcodegen` here and commit both.

## Brand

Colours, Knode Block and Knode Line come from the `KnodeBrand` Swift package in the brand kit; the
app icon and the `.knode` document icon come from `brand/07 App Icon`. The app declares the
`md.knode.space` type for `.knode` packages.
