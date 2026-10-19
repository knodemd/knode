import KnodeBrand
import SwiftUI

struct ContentView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        NavigationSplitView {
            SidebarView()
                .navigationSplitViewColumnWidth(min: 180, ideal: 210, max: 280)
        } content: {
            NodeListView()
                .navigationSplitViewColumnWidth(min: 240, ideal: 290, max: 400)
        } detail: {
            if let editor = model.editor {
                NoteView(editor: editor)
                    .id(editor.nodeID)
            } else if model.selectedSpace == nil {
                WelcomeView()
            } else {
                ContentUnavailableView("No Note Selected", systemImage: "doc.text", description: Text("Choose a note from the list, or create one."))
            }
        }
        .searchable(text: $model.searchText, placement: .toolbar, prompt: "Search")
        .tint(KnodeColor.indigo)
        .alert(
            "Something went wrong",
            isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } }),
            actions: { Button("OK") { model.errorMessage = nil } },
            message: { Text(model.errorMessage ?? "") }
        )
    }
}

struct SidebarView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let selection = Binding<Space.ID?>(
            get: { model.selectedSpaceID },
            set: { id in Task { await model.selectSpace(id) } }
        )
        List(selection: selection) {
            Section {
                ForEach(model.spaces) { space in
                    Label {
                        Text(space.name)
                    } icon: {
                        KnodeIconView(.knode).frame(width: 13, height: 13)
                    }
                    .tag(space.id)
                    .help(space.url?.path(percentEncoded: false) ?? space.name)
                }
            } header: {
                Text("SPACES").font(.knodeLine(11)).foregroundStyle(.secondary)
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .bottom) {
            VStack(alignment: .leading, spacing: 8) {
                Button("Open Space…", systemImage: "folder") {
                    if let url = SpacePanels.chooseSpace() { Task { await model.openSpace(at: url) } }
                }
                .buttonStyle(.borderless)
                if model.isShowingExample {
                    ExampleNotice()
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
        }
    }
}

/// Says plainly that the example space is not saved.
struct ExampleNotice: View {
    var body: some View {
        Label {
            Text("Example space: notes kept in memory, nothing is saved.")
        } icon: {
            Image(systemName: "info.circle")
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .fixedSize(horizontal: false, vertical: true)
    }
}

struct NodeListView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        Group {
            if model.selectedSpace == nil {
                ContentUnavailableView("No Space Open", systemImage: "square.stack", description: Text("Open a .knode space to see its notes."))
            } else if model.nodes.isEmpty {
                if model.searchText.isEmpty {
                    ContentUnavailableView("No Notes", systemImage: "doc.text", description: Text("Notes you and your agents write appear here."))
                } else {
                    ContentUnavailableView.search(text: model.searchText)
                }
            } else {
                List(selection: $model.selectedNodeID) {
                    ForEach(model.nodes) { node in
                        NodeRow(node: node).tag(node.id)
                    }
                }
            }
        }
        .navigationTitle(model.selectedSpace?.name ?? "Knode")
        .toolbar {
            ToolbarItem {
                Button("New Note", systemImage: "square.and.pencil") {
                    Task { await model.createNode() }
                }
                .disabled(model.selectedSpace == nil)
            }
        }
    }
}

struct NodeRow: View {
    let node: NodeSummary

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(node.title.isEmpty ? "Untitled" : node.title)
                .font(.headline)
                .lineLimit(1)
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(node.updatedAt, format: .relative(presentation: .named))
                    .foregroundStyle(.secondary)
                Text(node.excerpt)
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
            }
            .font(.subheadline)
        }
        .padding(.vertical, 4)
    }
}

struct WelcomeView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 22) {
            KnodeMark()
                .foregroundStyle(KnodeColor.indigo)
                .frame(height: 44)
            VStack(spacing: 6) {
                Text("CONNECT YOUR THOUGHTS.").font(.knodeBlock(24))
                Text("NATURALLY.").font(.knodeBlock(24)).foregroundStyle(KnodeColor.indigo)
            }
            Text("Open a space to see what you and your agents remember.")
                .foregroundStyle(.secondary)
            HStack(spacing: 10) {
                Button("Open Space…", systemImage: "folder") {
                    if let url = SpacePanels.chooseSpace() { Task { await model.openSpace(at: url) } }
                }
                .buttonStyle(.borderedProminent)
                .foregroundStyle(KnodeColor.ink)
                Button("New Space…", systemImage: "plus.square.on.square") {
                    if let url = SpacePanels.chooseNewSpace() { Task { await model.createSpace(at: url) } }
                }
                Button("Try the Example Space", systemImage: "sparkles") {
                    Task { await model.openExampleSpace() }
                }
            }
            .controlSize(.large)
            if !model.recentSpaceURLs.isEmpty {
                RecentSpacesList()
            }
        }
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(PaperBackground())
    }
}

/// The spaces opened most recently, one click to open again.
struct RecentSpacesList: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("RECENT").font(.knodeLine(11)).foregroundStyle(.secondary)
            ForEach(model.recentSpaceURLs.prefix(5), id: \.self) { url in
                Button {
                    Task { await model.openSpace(at: url) }
                } label: {
                    Label {
                        Text(url.deletingPathExtension().lastPathComponent)
                    } icon: {
                        KnodeIconView(.knode).frame(width: 13, height: 13)
                    }
                }
                .buttonStyle(.link)
                .help(url.path(percentEncoded: false))
            }
        }
        .frame(width: 280, alignment: .leading)
    }
}

/// Paper in light mode, ink in dark mode.
struct PaperBackground: View {
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        (colorScheme == .dark ? KnodeColor.ink : KnodeColor.paper).ignoresSafeArea()
    }
}
