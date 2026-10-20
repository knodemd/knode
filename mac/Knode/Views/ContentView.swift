import AppKit
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
        .sheet(item: $model.sheet) { sheet in
            switch sheet {
            case .newSpace: NewSpaceSheet()
            case .rename(let url): RenameSpaceSheet(url: url)
            case .icon(let url): SpaceIconSheet(url: url)
            }
        }
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
        let selection = Binding<URL?>(
            get: { model.selectedSpaceURL },
            set: { url in Task { await model.selectSpace(at: url) } }
        )
        List(selection: selection) {
            Section {
                ForEach(Array(model.sidebarSpaces.enumerated()), id: \.element.id) { index, item in
                    SpaceRow(item: item, number: index + 1)
                        .tag(item.url)
                        .contextMenu { SpaceMenuItems(item: item) }
                }
            } header: {
                Text("SPACES").font(.knodeLine(11)).foregroundStyle(.secondary)
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .bottom) {
            VStack(alignment: .leading, spacing: 8) {
                Button("New Space…", systemImage: "plus") {
                    model.sheet = .newSpace
                }
                .buttonStyle(.borderless)
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
        .onAppear { model.reloadDefaultSpace() }
    }
}

/// One space in the sidebar: its icon, its name, and a mark on the default space.
struct SpaceRow: View {
    let item: SpaceItem
    let number: Int

    var body: some View {
        Label {
            HStack(spacing: 6) {
                Text(item.name)
                    .lineLimit(1)
                    .foregroundStyle(item.isMissing ? .secondary : .primary)
                Spacer(minLength: 4)
                if item.isDefault {
                    Image(systemName: "sparkles")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .help("Default space: agents and capture write here")
                        .accessibilityLabel("Default space for agents")
                }
                if item.isMissing {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .help("Not found where it was")
                }
            }
        } icon: {
            SpaceIconView(icon: item.icon)
        }
        .help(help)
    }

    private var help: String {
        var lines = [item.isExample ? "Example space, kept in memory" : item.url.path(percentEncoded: false)]
        if number <= 9 { lines.append("Command-\(number) switches to it") }
        return lines.joined(separator: "\n")
    }
}

/// The emoji of a space, or the Knode icon when it has none.
struct SpaceIconView: View {
    let icon: String?

    var body: some View {
        if let icon {
            Text(icon).font(.system(size: 13))
        } else {
            KnodeIconView(.knode).frame(width: 13, height: 13)
        }
    }
}

/// What can be done to a space, in its context menu and in the Space menu.
struct SpaceMenuItems: View {
    @Environment(AppModel.self) private var model
    let item: SpaceItem

    var body: some View {
        Button("Rename…", systemImage: "pencil") { model.sheet = .rename(item.url) }
            .disabled(item.isMissing)
        Button("Choose Icon…", systemImage: "face.smiling") { model.sheet = .icon(item.url) }
            .disabled(item.isMissing)
        if !item.isExample {
            Divider()
            if item.isDefault {
                Button("Stop Using for Agents and Capture", systemImage: "sparkles") { model.setDefaultSpace(nil) }
            } else {
                Button("Use for Agents and Capture", systemImage: "sparkles") { model.setDefaultSpace(item.url) }
                    .disabled(item.isMissing)
            }
            Button("Show in Finder", systemImage: "folder") {
                NSWorkspace.shared.activateFileViewerSelecting([item.url])
            }
            .disabled(item.isMissing)
        }
        Divider()
        Button("Remove from List", systemImage: "minus.circle") {
            Task { await model.removeFromList(item.url) }
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
                    model.sheet = .newSpace
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
