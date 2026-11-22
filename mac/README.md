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

## Map: graph, canvases and links

**Show Graph** (Shift-Command-M, or the graph button in the toolbar) shows the space as a graph:
every note and the links between them, laid out by force. Kinds of note are told apart by glyph in
the brand inks; `mentions` links are faint and typed links dashed. Drag the background or scroll to
pan, pinch or scroll with Command to zoom, drag a note to move it and click one to open it. Pointing
at a note or a link shows how each link reads, such as *part of*. The bar above the graph filters by
kind, link kind, tag, person or agent and date, finds a note by title and centres it, and saves what
is shown as a **named graph view**: its filter, its viewport and where every note is, so it opens
the same way next time, on any machine.

**New Canvas** (Option-Command-N) makes a canvas: a node of kind `canvas` that opens as a board.
Drag notes from the list onto it or use **Add Notes**, then move, resize, collapse, group, link and
remove them, and zoom and pan around. A group moves the notes in it; a note dropped inside a group
joins it. Positions, sizes, groups and the viewport are saved at once and add no change to the
history; Edit > Undo undoes and redoes moves, adds and removals made on the board in this session.

**Show Links** (Shift-Command-L) shows the links of the open note beside it: links going out and
backlinks, each with the label it reads with from this note (*has part*, *mentioned in*). **Add
Link** links the note to another with a kind, or defines a new kind first: its label, the inverse
label read from the other note, whether it reads the same both ways, and what it means.

Knode Service has no methods yet for link kinds, labelled links, views, positions or canvases. The
app reads and writes them through the core in the app even while the service holds the space, as it
does for renaming; making and removing a link goes through the service when it runs, so its other
clients see it.

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

## Meetings

**Record Meeting…** in the menu bar menu records the sound of the Mac (everyone on the call, through
a Core Audio process tap) and the microphone (you), as two AAC tracks; **Record In Person…** records
only the microphone, for people in a room. The first time, the app explains that some places
require every participant's consent; after that, each recording starts only by hand, for that
meeting, after choosing its space and project, with a notice to copy into the meeting chat. The
same sheet has **Write minutes**, the space setting `recordings.summaries` (on unless it says
`off`), written at once; the transcript is always kept. While it records, the menu bar shows a red
dot and the time.

Both tracks are resampled to 48 kHz mono before they are encoded (AAC, a constant 64 kbps: about
29 MB an hour), so a Bluetooth headset at 8 or 16 kHz or an interface at 96 or 192 kHz records like
the built-in microphone. When the microphone changes mid-meeting (AirPods connect, a USB microphone
is unplugged), the engine starts again with the new input and the same `Me` track goes on; the
window and the menu say so briefly. The system audio comes from a process tap read through a private
aggregate device that holds the tap alone, so a headset's microphone never ends up on the `Others`
track; only when such a device cannot be made is the default output added as its clock, and then
only the tap's own buffers (the last ones of the list) are read.

When it stops, the files are closed and moved, with a `manifest.json` (space, project, mode, start,
duration, tracks and the key that keeps a second save from storing it twice), to
`~/Library/Application Support/Knode/Unsaved Recordings/<id>` (under `$KNODE_HOME` when it is set).
Then they are stored in the chosen space with `recordings.create`, as a node of kind `meeting`
titled like `Meeting, 22 November 2026 10:30` (or `In-person meeting, …`), linked to its project,
and removed only once the stored recording lists every track. Saving cannot be cancelled. A
recording that could not be saved (a file larger than the 100 MB a space keeps in one attachment,
a space that cannot be written, a track the space did not keep) stays there: the menu bar lists it
as **Unsaved recording**, with **Save Again**, **Show in Finder** and **Delete** (which asks first,
in the row), and the app tries to save it again at launch and whenever Knode Service starts.

A track grows by 8 kB a second, so a recording warns from three hours on and stops by itself at
95 % of what a space keeps in one file (about 3 h 27 min); it is saved, and **Record Again** goes
on with the meeting, to the same space and project, while the first part is transcribed. Quitting
the app while a meeting records stops it, closes its files and waits up to 20 seconds for it to be
saved; what is not saved by then is saved at the next launch.

Transcribing and summarising are one pipeline, `RecordingsProcessor`, for every recording a space
says is still to do (`recordings.pending`): the meetings recorded here and recordings the app did
not make, such as voice memos a connector brings in with one `mixed` track. It looks for work at
launch, when Knode Service starts again, when the open spaces change and when a space announces a
change to a node, in the open spaces and the default space, one recording at a time. Each audio
file is read from the space (`attachment.path`) and transcribed on the Mac with `SpeechAnalyzer`
(sentences with their times; `Me` and `Others` from the two tracks of an online meeting, `Speaker`
for one microphone), then stored with `recordings.transcript`. The minutes are written by Apple's
on-device model (Foundation Models): an overview, the key points, every decision, action item and
open question, by map-reduce over the transcript so a long meeting fits the model, and stored with
`recordings.summary`. A transcription error marks the transcript `failed`; minutes the Mac cannot
write mark the summary `unavailable`, with why. The transcript is always kept. Nothing leaves the
Mac. Audio the speech analyzer cannot open as it is, such as a QuickTime `.qta` voice memo, is
decoded through `AVAssetReader` into a temporary file first. The minutes cut the transcript to
what the model counts as fitting (from macOS 26.4; before, at one character a token for Chinese,
Japanese or Korean and 2.5 for plain ASCII), and a chunk or a set of key points that still does
not fit, or whose answer is cut off, is halved and tried again. Because what is left to do is kept in the space, work interrupted by quitting the app goes on
at the next launch.

The recording is the person's; transcripts, minutes and failures are written by Knode itself:
`{ "kind": "system", "actor": "recordings", "source": "apple-speech" }` (or
`apple-foundation-models`). The menu bar shows the recording being worked on and, for a stage
that failed, **Retry** (`recordings.retry`). The failures come from the spaces
(`recordings.failed`), read with the pending ones, so they can be retried after a restart too.

The pieces are in `Knode/Recordings`, behind protocols (`MeetingRecording`, `Transcriber`,
`Summarizer`, `RecordingSink`), and the store calls in `RecordingStore`: through Knode Service when
it runs, through the core in the app when it does not.

To check transcription against the real model, run the manual test with a Spanish file:

```sh
say -v Mónica -o /tmp/x.aiff "Hemos decidido lanzar la versión el viernes catorce."
TEST_RUNNER_KNODE_MANUAL_AUDIO=/tmp/x.aiff xcodebuild -scheme Knode -destination 'platform=macOS' \
  test -only-testing:KnodeTests/RecordingsManualTests
```

## Photos, recordings and videos

**File > Add Photos…** and **Add Recording or Video…**, or dropping files on the note list, add
photos (HEIC, JPEG, PNG), audio files and videos to the open space, in its selected project. Each
file becomes one node with the original attached unchanged, and the same file added again (by its
SHA-256, `photo.key` or `recording.key`) is found, not copied twice. `MediaImporter` does it:

- A photo is stored with `photos.create`, with the date, place (coordinates), camera and size its
  file says, read with ImageIO. `PhotosProcessor` then reads every photo a space says is still to
  read (`photos.pending`), on this Mac, with Vision: the text in it (`VNRecognizeTextRequest`,
  accurate, the language found by itself) and up to eight labels above 30% confidence
  (`VNClassifyImageRequest`), stored with `photos.reading` between `<!-- knode:photo-reading -->`
  markers, so search finds "the whiteboard photo". The place is never named: that needs the
  network, so photos keep only coordinates. The space setting `photos.location` (`off`, with
  `knode spaces photo-location <space> off`) keeps coordinates out. Readings are written as
  `{ "kind": "system", "actor": "photos", "source": "apple-vision" }`; a photo that cannot be
  read is `failed`, with **Retry** in the menu bar.
- An audio file or a video is a recording (`recordings.create`, kind `recording`, source `file`),
  titled after the file, from its creation date and for its length, with the file as its `mixed`
  track, so `RecordingsProcessor` transcribes it and writes minutes like a meeting. A video is
  always decoded through `AVAssetReader`, which reads its audio track.
- A file larger than the 100 MB a space keeps in one attachment is never changed. For audio or
  video, its audio is extracted on this Mac as mono AAC at 64 kbit/s (about 30 MB an hour) and
  attached instead, and the recording says the original stays where it was (`recording.original`);
  audio still over 100 MB, or a photo over 100 MB, is refused with why.
- Files that would take the space past its size limit (`storage.check`) are added only after
  **Add Anyway**.

`knode import <file>` (or `--from media <folder>`) adds the same nodes without the app; the app
reads and transcribes them once it opens the space.

### Photos library

The **Photos library** source (Settings > Sources, `photos-library`) brings in the photos and
videos of the albums the person picks, and only those: none until they pick one. PhotoKit is
reached only by the app, so the connector is app-driven (`PROTOCOL.md`, "Photos library"):
`PhotosLibrarySyncer` reads it and Knode Service writes.

- Connecting it asks for Photos access in the Connect sheet (`PhotosLibraryAccessSignIn`), never
  before; the albums step then lists the albums of the Mac to switch on (`picks`: switching one
  on is `sources.include`, off is `sources.exclude`). `NSPhotoLibraryUsageDescription` and the
  `com.apple.security.personal-information.photos-library` entitlement allow it.
- Albums are picked per Mac: Knode Service gives each Knode home a library id and names the
  albums `<library id>/<album id>`, so another Mac's picks are not this Mac's, and the source's
  options say "No albums picked on this Mac" when only other Macs picked some. Every node the
  library adds keeps that id (`photos_library.library`), and only this library's nodes are marked.
- A run, for each space: `photos_library.plan` (this Mac's picks), the assets of the albums picked,
  which the space has or does not want (`photos_library.known`: held, deleted by the person, or
  failed for good), then each new one exported with `PHAssetResourceManager`, written and hashed
  (SHA-256) as it comes, and handed to `photos_library.add` with its hash: a photo's original
  HEIC or JPEG unchanged; a video, or for a video over 100 MB only its audio as AAC (the video is
  deleted as soon as its audio is out). The export is deleted after the call. Assets that can
  never be added (audio still over 100 MB, no original) are reported with `photos_library.sync`
  and not exported again. `photos_library.sync` ends the run with every identifier still in the
  albums, so the assets that left them are marked (`photos_library.state`, never in the body),
  never deleted, and the library's change token; when an album picked is not found, nothing is
  marked.
- Nothing is duplicated: each node keeps the asset's `localIdentifier` and its library, a photo is
  found again by its file's hash and a video by its original's, so a rebuilt library adds nothing
  twice. A photo the person added themselves is never taken over. A node the person deletes is
  never brought back. Photos are then read by `PhotosProcessor` and videos transcribed by
  `RecordingsProcessor`, like any other.
- It runs every minute, at once when the library changes (`PHPhotoLibraryChangeObserver`), when
  the albums picked change or when the service asks (Run now). With the same change token as the
  last run and no request, nothing is read. Once a space was read in full since the app started,
  later runs read only what changed since the saved token (`fetchPersistentChanges(since:)`); an
  expired token reads everything again. Listing never reads an asset's resources. The scratch
  folder is emptied when the syncer starts.
- An original kept only in iCloud waits until the Mac has it, unless **Download from iCloud** is
  on for the space (`photos_library.configure`). History From today leaves out what was taken
  before the day it was connected. Once the space is past its size limit nothing new is added;
  the source says how many wait.

### Storage

Settings > Spaces shows **Storage** for the space chosen: its total size; by kind (notes, photos,
videos, audio, other files, and files of deleted nodes that a purge frees); who added how much;
the largest files with **Show** (opens the node) and **Remove** (after a confirmation; the node
keeps its text and the history can undo it); the size past which imports ask first and the
Photos library holds back (10 GB unless changed, or Never). Every size is computed by the core (`storage.summary`, `storage.check`,
`storage.configure`, or `KnodeSpace.storageSummary` and friends when the app holds the space);
`SpaceStorageModel` only shows them.

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

On the first run Get started opens, on **Let your agent do it**, which starts with **Connect your
agents**; if Get started is not open when the command is installed on the first run (or found
already installed), the app shows the step on its own. It is also in **Knode > Connect Your
Agents…**, which is disabled while another sheet is open so it never replaces one. It lists what
`knode connect --list --json` reports: the agents on this Mac and the ones already connected,
Claude Code first, then Codex, then the others. **Connect** runs `knode connect <agent>` (with
`--space` only when the chosen space is not the space for agents) with the login shell's `PATH`
and agent settings variables (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `COPILOT_HOME`, `CLINE_DIR`,
`CLINE_MCP_SETTINGS_PATH`, `XDG_CONFIG_HOME`), read once from an interactive login shell, so the
agent's own command is found and the folder its sessions read is configured. Then it reads the
list again: the core says whether the agent is connected. Once it is, the app says to open a new
session of the agent, which sets the memory up. When the `knode` it would run is the one inside
the app and the app runs from its disk image, a translocated copy, Downloads or a temporary
folder, connecting is refused and the step offers to move Knode to Applications: agents' settings
would point at a copy that goes away. A `knode` too old for `--json` is told to update.

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

### Check release packaging locally

Build a universal Release app with the workspace version (currently `0.0.0` before the first
release), then package it without installing or launching it:

```sh
xcodebuild -project Knode.xcodeproj -scheme Knode -configuration Release \
  -destination 'generic/platform=macOS' -derivedDataPath build/ReleaseData \
  MARKETING_VERSION=0.0.0 ARCHS='arm64 x86_64' ONLY_ACTIVE_ARCH=NO CODE_SIGN_IDENTITY=- build
scripts/package-app.sh build/ReleaseData/Build/Products/Release/Knode.app \
  0.0.0 /tmp/knode-package-check --allow-adhoc
```

The script checks both executable architectures, the app and embedded CLI versions, and the app
signature. It writes `Knode-<version>.dmg` and its checksum, with the app, an Applications link,
LICENSE and NOTICE inside. It refuses to overwrite an existing image. `--allow-adhoc` is for local
checks only: without it, packaging also requires Developer ID signing and a stapled notarisation
ticket. The release workflow signs, notarises and staples the final DMG, then computes its final
checksum. Never distribute the ad-hoc check as a signed release.

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
| `Knode/Store` | `SpaceStore`, `MapStore` (links, views and canvases), the models, `CoreSpaceStore`, `RoutingSpaceStore`, `PreviewSpaceStore`, the list of spaces (`SpaceLibrary`), the recent spaces and the default space (`KnodeSettings`) |
| `Knode/Service` | The socket client, `ServiceSpaceStore`, the agents and grants (`AgentDirectory`), the menu bar status and the `knode` runner |
| `Knode/CommandLineTool` | Where the `knode` command is, and installing and uninstalling its link |
| `Knode/Models` | `AppModel` (spaces, list, search), `NoteEditorModel` (one open note) `NoteHistoryModel` (its history, and undoing the last change), `AgentsModel` (agents and their access), `GraphLayout` (the force-directed layout and its Barnes-Hut quadtree), `GraphModel`, `CanvasModel` and `NoteLinksModel` (the map) |
| `Knode/Markdown` | The block parser behind the preview and `[[links]]` |
| `Knode/Recordings` | Meeting recording (microphone and process tap), on-device transcription and minutes, the recording controller and its menu and window |
| `Knode/Media` | Adding photos, audio and video to a space (`MediaImporter`, audio extraction), reading photos with Vision (`PhotosProcessor`, `VisionPhotoReader`), the `photos.*` store calls (`PhotoStore`) and the menu items and drop target |
| `Knode/PhotosLibrary` | The Photos library source: `PhotoLibrary` over PhotoKit (a fake in tests), `PhotosLibrarySyncer`, its access step and its Knode Service client |
| `Knode/Storage` | The storage of a space: `SpaceStorage` (service, core and routing), `SpaceStorageModel` and its section in Settings > Spaces |
| `Knode/Views` | The window, the note editor, the graph, the canvas, the links panel, the history, the agents settings, the space sheets, the menu bar menu and the open panels |
| `KnodeTests` | Tests for the stores (against a fake Knode Service), the view models, the command installer and the Markdown preview |
| `scripts` | The build of the core, of the `knode` command and of the document icon |
| `project.yml` | The [XcodeGen](https://github.com/yonaskolb/XcodeGen) source of `Knode.xcodeproj` |

`Knode.xcodeproj` is generated: after adding or moving files, run `xcodegen` here and commit both.

## Brand

Colours, Knode Block and Knode Line come from the `KnodeBrand` Swift package in the brand kit; the
app icon and the `.knode` document icon come from `brand/07 App Icon`, and the menu bar icon from
`KnodeMenuBarIcon`. The app declares the
`md.knode.space` type for `.knode` packages.
