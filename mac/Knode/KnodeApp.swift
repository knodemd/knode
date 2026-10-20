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

    init() {
        KnodeFonts.register()
        let store = RoutingSpaceStore()
        let command: @MainActor () -> KnodeCommand = { KnodeCommand(executable: CommandLineTool.forThisApp.executable()) }
        let service = ServiceMonitor(command: command)
        service.onRunningChange = { running in
            if running { Task { await store.serviceDidStart() } }
        }
        service.start()
        // While tests run in the app, the list of spaces stays in memory.
        let testing = ProcessInfo.processInfo.environment.keys.contains { $0.hasPrefix("XCTest") }
        _model = State(initialValue: AppModel(store: store, recentSpaces: RecentSpaces(), library: testing ? nil : SpaceLibrary(), settings: CoreKnodeSettings()))
        _service = State(initialValue: service)
        _agents = State(initialValue: AgentsModel(directory: ServiceAgentDirectory(), command: command, startService: { await service.startService() }))
    }

    var body: some Scene {
        Window("Knode", id: "main") {
            ContentView()
                .environment(model)
                .environment(agents)

                .onOpenURL { url in
                    Task { await model.openSpace(at: url) }
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
        .defaultSize(width: 1200, height: 760)
        .commands {
            SpaceCommands(model: model)
            CommandGroup(after: .appInfo) {
                Button("Install Command Line Tool…", systemImage: "terminal") { appDelegate.installer.install() }
                Button("Uninstall Command Line Tool…", systemImage: "trash") { appDelegate.installer.uninstall() }
            }
        }

        Settings {
            SettingsView()
                .environment(model)
                .environment(agents)
        }

        MenuBarExtra {
            ServiceMenu(monitor: service, model: model)
        } label: {
            KnodeMenuBarIcon.image(paused: !service.isRunning)
                .accessibilityLabel(service.isRunning ? "Knode Service, running" : "Knode Service, stopped")
        }
    }
}

/// Keeps the app in the menu bar when its window closes, and offers to install the `knode`
/// command on the first run.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let installer = CommandLineToolInstaller()

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Never while tests run in the app: the dialog would wait for a click.
        guard !ProcessInfo.processInfo.environment.keys.contains(where: { $0.hasPrefix("XCTest") }) else { return }
        DispatchQueue.main.async { [installer] in installer.offerOnFirstRun() }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}

struct SpaceCommands: Commands {
    let model: AppModel

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("New Note", systemImage: "square.and.pencil") {
                Task { await model.createNode() }
            }
            .keyboardShortcut("n")
            .disabled(model.selectedSpaceID == nil)

            Button("New Space…", systemImage: "plus.square.on.square") {
                model.sheet = .newSpace
            }
            .keyboardShortcut("n", modifiers: [.command, .shift])

            Button("Open Space…", systemImage: "folder") {
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
                Button("Clear Menu", systemImage: "trash") { model.clearRecentSpaces() }
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
            Button("Undo Last Change to Note", systemImage: "arrow.uturn.backward.circle") {
                Task { if await !model.undoLastChange() { NSSound.beep() } }
            }
            .keyboardShortcut("z", modifiers: [.command, .option])
            .disabled(model.editor == nil)

            Button("Redo Last Change to Note", systemImage: "arrow.uturn.forward.circle") {
                Task { if await !model.redoLastChange() { NSSound.beep() } }
            }
            .keyboardShortcut("z", modifiers: [.command, .option, .shift])
            .disabled(model.editor == nil)
        }

        CommandGroup(after: .sidebar) {
            Button(model.isShowingHistory ? "Hide History" : "Show History", systemImage: "clock.arrow.circlepath") {
                model.toggleHistory()
            }
            .keyboardShortcut("y")
            .disabled(model.editor == nil)
        }
    }
}
