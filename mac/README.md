# Knode for Mac

Knode for Mac, in SwiftUI: open a space, browse its notes newest first, read and edit a note in
Markdown with a live preview, and search.

The app talks to a space through the `SpaceStore` protocol (`Knode/Store/SpaceStore.swift`). The
Rust core will implement it through UniFFI; until then the app runs on `PreviewSpaceStore`, which
keeps example notes in memory and does not read or write `.knode` packages yet. The app says so in
its sidebar.

## Requirements

- macOS 26 or later
- Xcode 26 or later
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

To open the example space at launch, pass `-KnodeOpenExample YES` as a launch argument.

## Layout

| Path | What it holds |
|---|---|
| `Knode/Store` | `SpaceStore`, the models and `PreviewSpaceStore` |
| `Knode/Models` | `AppModel` (spaces, list, search) and `NoteEditorModel` (one open note) |
| `Knode/Markdown` | The block parser behind the preview and `[[links]]` |
| `Knode/Views` | The window, the note editor and the open and save panels |
| `KnodeTests` | Tests for the preview store, the view models and the Markdown preview |
| `project.yml` | The [XcodeGen](https://github.com/yonaskolb/XcodeGen) source of `Knode.xcodeproj` |

`Knode.xcodeproj` is generated: after adding or moving files, run `xcodegen` here and commit both.

## Brand

Colours, Knode Block and Knode Line come from the `KnodeBrand` Swift package in the brand kit; the
app icon and the `.knode` document icon come from `brand/07 App Icon`. The app declares the
`md.knode.space` type for `.knode` packages.
