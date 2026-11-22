import AppKit
import KnodeBrand
import SwiftUI

@main
struct KnodeApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    // Spaces are read and written through Knode Service when it runs, else by the Rust core in the
    // app (core/crates/knode-ffi); only the example space lives in memory.
    @State private var model: AppModel
    @State private var service: ServiceMonitor
    @State private var agents: AgentsModel
    @State private var searchIndex: SearchIndexModel
    @State private var sources: SourcesModel
    @State private var recording: RecordingController
    @State private var recordings: RecordingsProcessor
    @State private var photos: PhotosProcessor
    @State private var media: MediaImporter

    init() {
        KFont.register()
        let store = RoutingSpaceStore()
        let command: @MainActor () -> KnodeCommand = { KnodeCommand(executable: CommandLineTool.forThisApp.executable()) }
        let service = ServiceMonitor(command: command)
        // While tests run in the app, the list of spaces stays in memory.
        let testing = ProcessInfo.processInfo.environment.keys.contains { $0.hasPrefix("XCTest") }
        let model = AppModel(
            store: store,
            recentSpaces: RecentSpaces(),
            library: testing ? nil : SpaceLibrary(),
            settings: CoreKnodeSettings(),
            preferences: testing ? nil : .standard
        )
        _model = State(initialValue: model)
        // The one pipeline that transcribes and summarises recordings on this Mac: the meetings
        // recorded here and those that connectors bring in, such as voice memos.
        let recordings = RecordingsProcessor(
            store: store,
            transcriber: SpeechAnalyzerTranscriber(),
            summarizer: FoundationModelsSummarizer(),
            spaceURLs: { [model] in model.recordingSpaceURLs }
        )
        _recordings = State(initialValue: recordings)
        // Photos are read, and audio and video added by hand transcribed, on this Mac too.
        let photos = PhotosProcessor(store: store, reader: VisionPhotoReader(), spaceURLs: { [model] in model.recordingSpaceURLs })
        _photos = State(initialValue: photos)
        _media = State(initialValue: MediaImporter(store: store) { url, kind in
            if kind == .photo { photos.scan(spaceAt: url) } else { recordings.scan(spaceAt: url) }
        })
        let recording = RecordingController(
            recorder: MeetingRecorder(),
            sink: StoreRecordingSink(store: store),
            processor: recordings,
            consent: DefaultsRecordingConsentStore(),
            unsaved: UnsavedRecordingStore(home: .current)
        )
        _recording = State(initialValue: recording)
        service.onRunningChange = { running in
            if running {
                Task {
                    await store.serviceDidStart()
                    recordings.serviceDidStart()
                    photos.serviceDidStart()
                    if !testing { await recording.saveUnsaved() }
                }
            }
        }
        service.start()
        if !testing {
            recordings.start()
            photos.start()
            // Recordings that could not be saved before, such as when the app quit while saving.
            Task { await recording.saveUnsaved() }
        }
        // Quitting while a meeting records stops it, closes its files and saves it first.
        AppDelegate.beforeTerminating = { [recording] in
            guard recording.shouldDelayTermination else { return false }
            await recording.prepareForTermination(timeout: .seconds(20))
            return true
        }
        _service = State(initialValue: service)
        _agents = State(initialValue: AgentsModel(directory: ServiceAgentDirectory(), command: command, startService: { await service.startService() }))
        _sources = State(initialValue: SourcesModel(
            client: ServiceSourcesClient(),
            openURL: { NSWorkspace.shared.open($0) },
            startService: { await service.startService() }
        ))
        _searchIndex = State(initialValue: SearchIndexModel(service: ServiceSearchIndex(), settings: CoreKnodeSettings(), startService: { await service.startService() }))
    }

    var body: some Scene {
        Window("Knode", id: "main") {
            ContentView()
                .knodeTheme(background: true)
                .environment(model)
                .environment(agents)
                .environment(media)

                .task {
                    // Once the knode command is installed on the first run: Connect your agents,
                    // unless Get started is open, which shows the same step.
                    appDelegate.installer.whenReadyOnFirstRun { [model] in model.showConnectAgents() }
                }
                .onOpenURL { url in
                    Task { await model.openSpace(at: url) }
                }
                // Back from the terminal or an agent: list the spaces made meanwhile.
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
                    model.refreshKnownSpaces()
                }
                .task {
                    guard model.spaces.isEmpty else { return }
                    // `-KnodeOpenSpace <path>` opens a space at launch; `-KnodeOpenExample YES`
                    // opens the example space. Either way the newest note opens too.
                    if let path = UserDefaults.standard.string(forKey: "KnodeOpenSpace") {
                        await model.openSpace(at: URL(filePath: path, directoryHint: .isDirectory))
                        await model.openNode(model.nodes.first?.id)
                    } else if UserDefaults.standard.bool(forKey: "KnodeOpenExample") {
                        await model.openExampleSpace()
                        await model.openNode(model.nodes.first?.id)
                    }
                }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1200, height: 760)
        .commands {
            SpaceCommands(model: model, media: media)
            #if DEBUG
            DesignGalleryCommands()
            #endif
            CommandGroup(after: .appInfo) {
                Button("Install Command Line Tool…") { appDelegate.installer.install() }
                Button("Uninstall Command Line Tool…") { appDelegate.installer.uninstall() }
                Button("Connect Your Agents…") { model.showConnectAgents() }
                    .disabled(!model.canShowConnectAgents)
            }
        }

        Settings {
            SettingsView()
                .environment(model)
                .environment(agents)
                .environment(searchIndex)
                .environment(sources)
                .environment(service)
        }
        .windowResizability(.contentSize)

        Window("Record Meeting", id: RecordingWindow.id) {
            RecordingWindow()
                .knodeTheme(background: true)
                .environment(recording)
                .environment(model)
        }
        .windowResizability(.contentSize)

        #if DEBUG
        Window("Design Gallery", id: DesignGallery.windowID) {
            DesignGallery()
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1040, height: 820)
        #endif

        MenuBarExtra {
            ServiceMenu(monitor: service, model: model, searchIndex: searchIndex, recording: recording, recordings: recordings)
                .environment(photos)
        } label: {
            if recording.isRecording {
                // Always visible while recording: a dot and the time, beside the icon.
                Image(systemName: recording.isPaused ? "pause.circle.fill" : "record.circle.fill")
                    .accessibilityLabel(recording.isPaused ? "Recording paused" : "Recording")
                Text(RecordingText.elapsed(recording.elapsedSeconds))
            } else {
                KnodeMenuBarIcon.image(paused: !service.isRunning)
                    .accessibilityLabel(service.isRunning ? "Knode Service, running" : "Knode Service, stopped")
            }
        }
        .menuBarExtraStyle(.window)
    }
}

/// Keeps the app in the menu bar when its window closes, offers to move it to Applications when it
/// runs from elsewhere, and offers to install the `knode` command or point it at this copy.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let installer = CommandLineToolInstaller()
    let mover = ApplicationMover()

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Never while tests run in the app: the dialog would wait for a click.
        guard !ProcessInfo.processInfo.environment.keys.contains(where: { $0.hasPrefix("XCTest") }) else { return }
        DispatchQueue.main.async { [installer, mover] in
            #if !DEBUG
            // Debug builds run from Xcode's build folder on purpose.
            if mover.offerToMove() { return }
            #endif
            installer.offerOnLaunch()
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    /// Work to finish before the app quits; returns false when there is none. Set by `KnodeApp`.
    static var beforeTerminating: (@MainActor () async -> Bool)?

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let work = Self.beforeTerminating else { return .terminateNow }
        Task { @MainActor in
            _ = await work()
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }
}

struct SpaceCommands: Commands {
    let model: AppModel
    let media: MediaImporter

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("New Note") {
                Task { await model.createNode() }
            }
            .keyboardShortcut("n")
            .disabled(model.selectedSpaceID == nil)

            Button("New Canvas") {
                Task { await model.createCanvas() }
            }
            .keyboardShortcut("n", modifiers: [.command, .option])
            .disabled(!model.hasMap)

            Button("New Space…") {
                model.sheet = .newSpace
            }
            .keyboardShortcut("n", modifiers: [.command, .shift])

            Divider()
            MediaMenuItems(model: model, media: media)
            Divider()

            Button("Open Space…") {
                if let url = SpacePanels.chooseSpace() { Task { await model.openSpace(at: url) } }
            }
            .keyboardShortcut("o")

            Menu("Open Recent") {
                ForEach(model.recentSpaceURLs, id: \.self) { url in
                    Button(url.deletingPathExtension().lastPathComponent) {
                        Task { await model.openSpace(at: url) }
                    }
                }
                Divider()
                Button("Clear Menu") { model.clearRecentSpaces() }
                    .disabled(model.recentSpaceURLs.isEmpty)
            }
        }

        CommandMenu("Space") {
            let items = model.sidebarSpaces
            ForEach(Array(items.prefix(9).enumerated()), id: \.element.id) { index, item in
                Button {
                    Task { await model.selectSpace(number: index + 1) }
                } label: {
                    Text(item.icon.map { "\($0) \(item.name)" } ?? item.name)
                }
                .keyboardShortcut(KeyEquivalent(Character(String(index + 1))), modifiers: .command)
            }
            if !items.isEmpty { Divider() }
            if let selected = items.first(where: { $0.url == model.selectedSpaceURL }) {
                SpaceMenuItems(item: selected)
                    .environment(model)
            } else {
                Text("No Space Selected")
            }
        }

        // Command-Z and Shift-Command-Z stay the text's own undo while typing. These undo and redo
        // whole saved changes of the note, through its history, so they also reach changes made
        // before the note was opened, from this app or the knode command.
        CommandGroup(after: .undoRedo) {
            Button("Undo Last Change to Note") {
                Task { if await !model.undoLastChange() { NSSound.beep() } }
            }
            .keyboardShortcut("z", modifiers: [.command, .option])
            .disabled(model.editor == nil)

            Button("Redo Last Change to Note") {
                Task { if await !model.redoLastChange() { NSSound.beep() } }
            }
            .keyboardShortcut("z", modifiers: [.command, .option, .shift])
            .disabled(model.editor == nil)
        }

        CommandGroup(after: .sidebar) {
            Button(model.mode == .graph ? "Show Notes" : "Show Graph") {
                model.toggleGraph()
            }
            .keyboardShortcut("m", modifiers: [.command, .shift])
            .disabled(!model.hasMap)

            Button(model.isShowingLinks ? "Hide Links" : "Show Links") {
                model.toggleLinks()
            }
            .keyboardShortcut("l", modifiers: [.command, .shift])
            .disabled(model.editor == nil || model.links == nil)

            Button(model.isShowingAttachments ? "Hide Attachments" : "Show Attachments") {
                model.isShowingAttachments.toggle()
            }
            .keyboardShortcut("a", modifiers: [.command, .shift])
            .disabled(model.editor == nil || !model.canAttach)

            Button(model.isShowingHistory ? "Hide History" : "Show History") {
                model.toggleHistory()
            }
            .keyboardShortcut("y")
            .disabled(model.editor == nil)
        }
    }
}
