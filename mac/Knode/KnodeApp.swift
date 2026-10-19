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

    init() {
        KnodeFonts.register()
        let store = RoutingSpaceStore()
        let service = ServiceMonitor(command: { KnodeCommand(executable: CommandLineTool.forThisApp.executable()) })
        service.onRunningChange = { running in
            if running { Task { await store.serviceDidStart() } }
        }
        service.start()
        _model = State(initialValue: AppModel(store: store, recentSpaces: RecentSpaces()))
        _service = State(initialValue: service)
    }

    var body: some Scene {
        Window("Knode", id: "main") {
            ContentView()
                .environment(model)

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

        MenuBarExtra {
            ServiceMenu(monitor: service)
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
                if let url = SpacePanels.chooseNewSpace() { Task { await model.createSpace(at: url) } }
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
    }
}
