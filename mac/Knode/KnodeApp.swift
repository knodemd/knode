import KnodeBrand
import SwiftUI

@main
struct KnodeApp: App {
    // Spaces are read and written by the Rust core (core/crates/knode-ffi); only the example space
    // lives in memory.
    @State private var model = AppModel(store: CoreSpaceStore(), recentSpaces: RecentSpaces())

    init() {
        KnodeFonts.register()
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
        .commands { SpaceCommands(model: model) }
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
