import KnodeBrand
import SwiftUI

@main
struct KnodeApp: App {
    // The Rust core reaches Swift through UniFFI later (knodemd/knode#11); until then the app runs
    // on example data kept in memory.
    @State private var model = AppModel(store: PreviewSpaceStore(), isPreview: true)

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
                    // `-KnodeOpenExample YES` opens the example space and its newest note at launch.
                    if UserDefaults.standard.bool(forKey: "KnodeOpenExample"), model.spaces.isEmpty {
                        await model.openSpace(at: SpacePanels.exampleURL)
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
        }
    }
}
