import AppKit
import KnodeBrand
import SwiftUI

/// Which sidebar shows: every space, or the notes of the open space.
enum SidebarLevel: Hashable {
    case spaces, space
}

/// The main window: one sidebar at a time on the left, our top bar, and the open note, the graph
/// or a canvas.
struct ContentView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @AppStorage("sidebarCollapsed") private var collapsed = false
    @State private var level: SidebarLevel = .spaces

    var body: some View {
        @Bindable var model = model
        HStack(spacing: 0) {
            if !collapsed {
                Sidebar(level: $level, collapse: toggleSidebar)
                    .frame(width: 260)
                    .transition(reduceMotion ? .opacity : .move(edge: .leading))
                KDivider(.vertical)
            }
            VStack(spacing: 0) {
                TopBar(collapsed: collapsed, expand: toggleSidebar)
                KDivider()
                MainArea()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(theme.bg)
        .background(WindowChrome())
        .ignoresSafeArea()
        .onChange(of: model.selectedSpaceID, initial: true) { _, id in
            withAnimation(KMotion.animation(reduceMotion: reduceMotion)) { level = id == nil ? .spaces : .space }
        }
        .kSheet(item: $model.ownSheet) { sheet in
            switch sheet {
            case .newSpace: NewSpaceSheet()
            case .rename(let url): RenameSpaceSheet(url: url)
            case .icon(let url): SpaceIconSheet(url: url)
            default: EmptyView()
            }
        }
        .sheet(item: $model.panelSheet) { sheet in
            switch sheet {
            case .access(let url): SpaceAccessSheet(url: url)
            case .folders(let url): SpaceFoldersSheet(url: url)
            default: EmptyView()
            }
        }
        .kSheet(item: $model.spaceToRemove, width: 460) { item in
            RemoveSpaceCard(item: item)
        }
        .kSheet(isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } }), width: 400) {
            VStack(alignment: .leading, spacing: KSpace.x5) {
                KSheetHeader(title: "Something went wrong", message: model.errorMessage)
                HStack {
                    Spacer()
                    KButton("OK", kind: .primary) { model.errorMessage = nil }
                        .keyboardShortcut(.defaultAction)
                }
            }
        }
    }

    private func toggleSidebar() {
        withAnimation(KMotion.animation(reduceMotion: reduceMotion)) { collapsed.toggle() }
    }
}

extension AppModel {
    /// The sheets drawn with our own sheet: new space, rename and icon.
    var ownSheet: SpaceSheet? {
        get { sheet.flatMap { $0.isPanel ? nil : $0 } }
        set { if newValue != nil || !(sheet?.isPanel ?? false) { sheet = newValue } }
    }

    /// The sheets that still use the system sheet: agent access and folders.
    var panelSheet: SpaceSheet? {
        get { sheet.flatMap { $0.isPanel ? $0 : nil } }
        set { if newValue != nil || (sheet?.isPanel ?? false) { sheet = newValue } }
    }
}

extension SpaceSheet {
    fileprivate var isPanel: Bool {
        switch self {
        case .access, .folders: true
        case .newSpace, .rename, .icon: false
        }
    }
}

/// Asks before taking a space off the sidebar.
private struct RemoveSpaceCard: View {
    @Environment(AppModel.self) private var model
    let item: SpaceItem

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x5) {
            KSheetHeader(
                kicker: "Remove space",
                title: "Remove \(item.name) from the list?",
                message: "Its files stay where they are. Unless you stop capturing into it, Knode still knows the space and captures the conversations of its folders into it."
            )
            HStack(spacing: KSpace.x2) {
                KButton("Cancel") { model.spaceToRemove = nil }
                Spacer()
                KButton("Remove and stop capturing", kind: .destructive) {
                    model.spaceToRemove = nil
                    Task { await model.removeFromList(item.url, forget: true) }
                }
                Button {
                    model.spaceToRemove = nil
                    Task { await model.removeFromList(item.url) }
                } label: {
                    Text("Remove from list")
                }
                .buttonStyle(KButtonStyle(kind: .primary))
                .keyboardShortcut(.defaultAction)
            }
        }
    }
}

// MARK: - Top bar

/// Our title bar: the traffic lights, where the window is, and the panels of the open note.
private struct TopBar: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    let collapsed: Bool
    let expand: () -> Void

    var body: some View {
        @Bindable var model = model
        HStack(spacing: KSpace.x2) {
            if collapsed {
                Color.clear.frame(width: WindowChrome.trafficLightsWidth - KSpace.x3)
                KIconButton(.sidebar, title: "Show sidebar", action: expand)
            }
            breadcrumb
            Spacer(minLength: KSpace.x4)
            if model.editor != nil, model.canvas == nil, model.mode == .notes {
                KIconButton(.link, title: "Links (Shift-Command-L)", isActive: model.isShowingLinks) { model.toggleLinks() }
                    .disabled(model.links == nil)
                KIconButton(.attach, title: "Attachments (Shift-Command-A)", isActive: model.isShowingAttachments) {
                    model.isShowingAttachments.toggle()
                }
                .disabled(!model.canAttach)
                KIconButton(.history, title: "History (Command-Y)", isActive: model.isShowingHistory) { model.toggleHistory() }
            }
        }
        .padding(.horizontal, KSpace.x3)
        .frame(height: WindowChrome.barHeight)
        .background {
            theme.bg
                .gesture(WindowDragGesture())
                .onTapGesture(count: 2) { NSApp.keyWindow?.zoom(nil) }
        }
    }

    @ViewBuilder private var breadcrumb: some View {
        if let space = model.selectedSpace {
            HStack(spacing: KSpace.x2) {
                Text(space.name).kLineStyle(11).foregroundStyle(theme.muted).lineLimit(1)
                if let current {
                    KIcon(.forward, size: .xs, color: theme.line)
                    Text(current).font(KFont.ui(13, .medium)).foregroundStyle(theme.fg).lineLimit(1)
                }
            }
            .padding(.leading, collapsed ? KSpace.x1 : KSpace.x3)
        }
    }

    private var current: String? {
        if model.mode == .graph { return "Graph" }
        guard let editor = model.editor else { return nil }
        return editor.title.isEmpty ? "Untitled" : editor.title
    }
}

// MARK: - Main area

private struct MainArea: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if model.mode == .graph, let graph = model.graph {
            GraphScreen(graph: graph)
                .id(graph.spaceID)
        } else if let editor = model.editor, let canvas = model.canvas {
            CanvasScreen(canvas: canvas, editor: editor)
                .id(canvas.nodeID)
        } else if let editor = model.editor {
            NoteView(editor: editor)
                .id(editor.nodeID)
        } else if model.selectedSpace == nil {
            WelcomeView()
        } else {
            KEmptyState(icon: .note, title: "No note open", message: "Choose a note in the sidebar, or start a new one.") {
                KButton("New note", icon: .add, kind: .primary) { Task { await model.createNode() } }
            }
        }
    }
}

// MARK: - Sidebar

/// The one sidebar: the spaces, or the notes of the open space. Opening a space slides its notes
/// in; back returns to the spaces.
private struct Sidebar: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Binding var level: SidebarLevel
    let collapse: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Spacer()
                KIconButton(.sidebar, title: "Hide sidebar", action: collapse)
            }
            .padding(.horizontal, KSpace.x3)
            .frame(height: WindowChrome.barHeight)
            .background {
                theme.panel
                    .gesture(WindowDragGesture())
                    .onTapGesture(count: 2) { NSApp.keyWindow?.zoom(nil) }
            }
            ZStack {
                if level == .space, model.selectedSpace != nil {
                    SpaceSidebar(back: { go(.spaces) })
                        .transition(reduceMotion ? .opacity : .move(edge: .trailing))
                } else {
                    SpacesSidebar(open: { go(.space) })
                        .transition(reduceMotion ? .opacity : .move(edge: .leading))
                }
            }
            .frame(maxHeight: .infinity)
            .clipped()
        }
        .background(theme.panel)
    }

    private func go(_ level: SidebarLevel) {
        withAnimation(KMotion.animation(reduceMotion: reduceMotion)) { self.level = level }
    }
}

/// Every space: the Knode logo, the list, and new or open at the bottom.
private struct SpacesSidebar: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    let open: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            KnodeLogo(markColor: theme.isDark ? theme.indigo : theme.fg, height: 15)
                .foregroundStyle(theme.fg)
                .padding(.horizontal, KSpace.x5)
                .padding(.top, KSpace.x1)
                .padding(.bottom, KSpace.x6)
            KSectionLabel("Spaces")
                .padding(.horizontal, KSpace.x5)
                .padding(.bottom, KSpace.x2)
            KScrollView {
                LazyVStack(spacing: 2) {
                    ForEach(Array(model.sidebarSpaces.enumerated()), id: \.element.id) { index, item in
                        SpaceRow(item: item, number: index + 1, isSelected: item.url == model.selectedSpaceURL) {
                            if item.url != model.selectedSpaceURL {
                                Task { await model.selectSpace(at: item.url) }
                            }
                            open()
                        }
                        .kContextMenu { spaceMenuEntries(item, model: model) }
                    }
                }
                .padding(.horizontal, KSpace.x2)
                .padding(.bottom, KSpace.x2)
            }
            VStack(alignment: .leading, spacing: KSpace.x1) {
                if model.isShowingExample {
                    ExampleNotice().padding(.bottom, KSpace.x2)
                }
                KButton("New space", icon: .add, kind: .ghost, size: .small) { model.sheet = .newSpace }
                KButton("Open space", icon: .folder, kind: .ghost, size: .small) {
                    if let url = SpacePanels.chooseSpace() { Task { await model.openSpace(at: url) } }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(KSpace.x3)
            .overlay(alignment: .top) { KDivider() }
        }
        .onAppear { model.reloadDefaultSpace() }
    }
}

/// One space in the sidebar: its icon, its name, and a mark on the default space.
struct SpaceRow: View {
    let item: SpaceItem
    let number: Int
    var isSelected = false
    var action: () -> Void = {}
    @Environment(\.kTheme) private var theme
    @State private var hovering = false

    var body: some View {
        KRow(isSelected: isSelected, action: action) {
            SpaceIconView(icon: item.icon)
                .frame(width: 18)
            Text(item.name)
                .font(KFont.ui(13, isSelected ? .semibold : .medium))
                .foregroundStyle(item.isMissing ? theme.muted : theme.fg)
                .lineLimit(1)
            Spacer(minLength: KSpace.x1)
            if item.isDefault {
                KIcon(.pin, size: .xs, color: theme.isDark ? theme.indigo : theme.fg)
                    .kTooltip("Default space: agents and capture write here")
                    .accessibilityLabel("Default space for agents")
            }
            if item.isMissing {
                KIcon(.warning, size: .xs, color: theme.muted)
                    .kTooltip("Not found where it was")
            }
            if hovering, number <= 9 {
                KKeyHint([.command, .char(String(number))])
            }
        }
        .onHover { hovering = $0 }
        .kTooltip(item.isExample ? "Example space, kept in memory" : item.url.path(percentEncoded: false))
    }
}

/// The emoji of a space, or the Knode icon when it has none.
struct SpaceIconView: View {
    let icon: String?
    @Environment(\.kTheme) private var theme

    var body: some View {
        if let icon {
            Text(icon).font(KFont.ui(14))
        } else {
            KIcon(.knode, size: .sm, color: theme.isDark ? theme.indigo : theme.fg)
        }
    }
}

/// What can be done to a space, in its context menu in the sidebar.
@MainActor
func spaceMenuEntries(_ item: SpaceItem, model: AppModel) -> [KMenuEntry] {
    var entries: [KMenuEntry] = [
        .action("Rename", icon: .edit, isEnabled: !item.isMissing) { model.sheet = .rename(item.url) },
        .action("Choose icon", icon: .image, isEnabled: !item.isMissing) { model.sheet = .icon(item.url) },
    ]
    if !item.isExample {
        entries.append(.divider)
        entries.append(.action("Agent access", icon: .`private`, isEnabled: !item.isMissing) { model.sheet = .access(item.url) })
        entries.append(.action("Folders and capture", icon: .folder, isEnabled: !item.isMissing) { model.sheet = .folders(item.url) })
        if item.isDefault {
            entries.append(.action("Stop using for agents and capture", icon: .pin) { model.setDefaultSpace(nil) })
        } else {
            entries.append(.action("Use for agents and capture", icon: .pin, isEnabled: !item.isMissing) { model.setDefaultSpace(item.url) })
        }
        entries.append(.action("Show in Finder", icon: .show, isEnabled: !item.isMissing) {
            NSWorkspace.shared.activateFileViewerSelecting([item.url])
        })
    }
    entries.append(.divider)
    entries.append(.action("Remove from list", icon: .close, isDestructive: true) {
        if item.isExample {
            Task { await model.removeFromList(item.url) }
        } else {
            model.spaceToRemove = item
        }
    })
    return entries
}

/// What can be done to a space, in the Space menu of the menu bar.
struct SpaceMenuItems: View {
    @Environment(AppModel.self) private var model
    let item: SpaceItem

    var body: some View {
        Button("Rename…") { model.sheet = .rename(item.url) }
            .disabled(item.isMissing)
        Button("Choose Icon…") { model.sheet = .icon(item.url) }
            .disabled(item.isMissing)
        if !item.isExample {
            Divider()
            Button("Agent Access…") { model.sheet = .access(item.url) }
                .disabled(item.isMissing)
            Button("Folders and Capture…") { model.sheet = .folders(item.url) }
                .disabled(item.isMissing)
            if item.isDefault {
                Button("Stop Using for Agents and Capture") { model.setDefaultSpace(nil) }
            } else {
                Button("Use for Agents and Capture") { model.setDefaultSpace(item.url) }
                    .disabled(item.isMissing)
            }
            Button("Show in Finder") {
                NSWorkspace.shared.activateFileViewerSelecting([item.url])
            }
            .disabled(item.isMissing)
        }
        Divider()
        Button("Remove from List…") {
            if item.isExample {
                Task { await model.removeFromList(item.url) }
            } else {
                model.spaceToRemove = item
            }
        }
    }
}

/// Says plainly that the example space is not saved.
struct ExampleNotice: View {
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(alignment: .top, spacing: KSpace.x2) {
            KIcon(.info, size: .xs, color: theme.muted).padding(.top, 2)
            Text("Example space: notes kept in memory, nothing is saved.")
                .font(KFont.ui(12))
                .foregroundStyle(theme.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, KSpace.x2)
    }
}

/// The notes of the open space: back to the spaces, its name, search, the list, and new note,
/// graph and canvas at the bottom.
private struct SpaceSidebar: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    let back: () -> Void
    @FocusState private var searchFocused: Bool

    var body: some View {
        @Bindable var model = model
        let item = model.sidebarSpaces.first { $0.url == model.selectedSpaceURL }
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: KSpace.x2) {
                KIconButton(.back, title: "All spaces", action: back)
                SpaceIconView(icon: item?.icon)
                Text(model.selectedSpace?.name ?? "")
                    .kBlockStyle(17)
                    .foregroundStyle(theme.fg)
                    .lineLimit(1)
                Spacer(minLength: 0)
                if let item {
                    KMenuButton(entries: { spaceMenuEntries(item, model: model) }) {
                        KIcon(.more, size: .sm)
                    }
                    .accessibilityLabel("Space actions")
                }
            }
            .padding(.leading, KSpace.x2)
            .padding(.trailing, KSpace.x2)
            .padding(.bottom, KSpace.x3)

            HStack(spacing: KSpace.x1) {
                KSearchField(placeholder: "Search", text: $model.searchText, hint: [.command, .char("F")], focus: $searchFocused)
                SearchModeButton()
            }
            .padding(.horizontal, KSpace.x3)
            .background {
                Button("Search") { searchFocused = true }
                    .keyboardShortcut("f")
                    .opacity(0)
                    .accessibilityHidden(true)
            }

            VStack(spacing: KSpace.x2) {
                if let deleted = model.deletedNotes, !deleted.waiting.isEmpty {
                    DeletedNotesBanner(deleted: deleted) { model.isShowingDeletedNotes = true }
                        .clipShape(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous))
                }
                if !model.searchText.isEmpty, let note = model.searchResults?.note {
                    SearchNoteBanner(note: note)
                        .clipShape(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous))
                }
            }
            .padding(.horizontal, KSpace.x3)
            .padding(.top, KSpace.x2)

            KSectionLabel(model.searchResults == nil ? "Notes" : "Results") {
                Text("\(model.searchResults?.hits.count ?? model.nodes.count)")
                    .font(KFont.mono(11))
                    .foregroundStyle(theme.muted)
            }
            .padding(.horizontal, KSpace.x5)
            .padding(.top, KSpace.x3)
            .padding(.bottom, KSpace.x1)

            NoteList()
                .frame(maxHeight: .infinity)

            HStack(spacing: KSpace.x1) {
                KButton("New note", icon: .add, kind: .primary, size: .small) {
                    Task { await model.createNode() }
                }
                Spacer()
                KIconButton(.map, title: "Graph (Shift-Command-M)", isActive: model.mode == .graph) { model.toggleGraph() }
                    .disabled(!model.hasMap)
                KIconButton(.canvas, title: "New canvas (Option-Command-N)") { Task { await model.createCanvas() } }
                    .disabled(!model.hasMap)
            }
            .padding(KSpace.x3)
            .overlay(alignment: .top) { KDivider() }
        }
        .sheet(isPresented: $model.isShowingDeletedNotes) {
            if let deleted = model.deletedNotes {
                DeletedNotesSheet(deleted: deleted)
            }
        }
    }
}

/// How search finds notes: by words and meaning, by words, or by meaning.
private struct SearchModeButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        KMenuButton(entries: {
            [.section("Search by")] + SearchMode.allCases.map { mode in
                .action(mode.title, isChecked: model.searchMode == mode) { model.searchMode = mode }
            }
        }) {
            KIcon(.filter, size: .sm)
        }
        .kTooltip("Search by \(model.searchMode.title.lowercased()): \(model.searchMode.explanation)")
        .accessibilityLabel("Search by \(model.searchMode.title)")
    }
}

/// The notes of the space newest first, or the search results.
private struct NoteList: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme

    var body: some View {
        if model.nodes.isEmpty {
            VStack(spacing: KSpace.x2) {
                KIcon(model.searchText.isEmpty ? .note : .search, size: .lg, color: theme.muted)
                Text(model.searchText.isEmpty ? "No notes yet" : "Nothing found").font(KFont.ui(13, .semibold))
                Text(model.searchText.isEmpty ? "Notes you and your agents write appear here." : "No note matches “\(model.searchText)”.")
                    .font(KFont.ui(12))
                    .foregroundStyle(theme.muted)
                    .multilineTextAlignment(.center)
            }
            .padding(KSpace.x6)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            KScrollView {
                LazyVStack(spacing: 2) {
                    if let results = model.searchResults {
                        ForEach(results.hits) { hit in
                            KRow(isSelected: model.selectedNodeID == hit.id, action: { model.selectedNodeID = hit.id }) {
                                SearchResultRow(hit: hit)
                            }
                            .draggable(hit.id)
                        }
                    } else {
                        ForEach(model.nodes) { node in
                            KRow(isSelected: model.selectedNodeID == node.id, action: { model.selectedNodeID = node.id }) {
                                NodeRow(node: node)
                            }
                            .draggable(node.id)
                        }
                    }
                }
                .padding(.horizontal, KSpace.x2)
                .padding(.bottom, KSpace.x2)
            }
        }
    }
}

/// A note in the list: its title, when it changed, and how it starts.
struct NodeRow: View {
    let node: NodeSummary
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                if node.kind == NodeKind.canvas {
                    KIcon(.canvas, size: .xs, color: theme.isDark ? theme.indigo : theme.fg)
                        .accessibilityLabel("Canvas")
                }
                Text(node.title.isEmpty ? "Untitled" : node.title)
                    .font(KFont.ui(13, .semibold))
                    .foregroundStyle(theme.fg)
                    .lineLimit(1)
                Spacer(minLength: KSpace.x1)
                Text(node.updatedAt, format: .relative(presentation: .numeric, unitsStyle: .narrow))
                    .kLineStyle(9.5)
                    .foregroundStyle(theme.muted)
                    .lineLimit(1)
                    .fixedSize()
            }
            if !node.excerpt.isEmpty {
                Text(node.excerpt)
                    .font(KFont.ui(12))
                    .foregroundStyle(theme.muted)
                    .lineLimit(2)
            }
        }
    }
}

// MARK: - Welcome

struct WelcomeView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(spacing: KSpace.x6) {
            KnodeMark()
                .foregroundStyle(theme.isDark ? theme.indigo : theme.fg)
                .frame(height: 48)
            VStack(spacing: KSpace.x2) {
                Text("Connect your thoughts.").kBlockStyle(30).foregroundStyle(theme.fg)
                Text("Naturally.").kBlockStyle(30)
                    .foregroundStyle(theme.isDark ? theme.indigo : theme.fg)
                    .background(alignment: .bottom) {
                        if !theme.isDark { theme.indigo.frame(height: 12).offset(y: -2) }
                    }
            }
            .multilineTextAlignment(.center)
            Text("Open a space to see what you and your agents remember.")
                .font(KFont.ui(15))
                .foregroundStyle(theme.muted)
            HStack(spacing: KSpace.x2) {
                KButton("Open space", icon: .folder, kind: .primary, size: .large) {
                    if let url = SpacePanels.chooseSpace() { Task { await model.openSpace(at: url) } }
                }
                KButton("New space", icon: .add, size: .large) { model.sheet = .newSpace }
                KButton("Try the example space", icon: .knode, kind: .ghost, size: .large) {
                    Task { await model.openExampleSpace() }
                }
            }
            if !model.recentSpaceURLs.isEmpty {
                RecentSpacesList().padding(.top, KSpace.x4)
            }
        }
        .padding(KSpace.x10)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.bg)
    }
}

/// The spaces opened most recently, one click to open again.
struct RecentSpacesList: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSectionLabel("Recent")
            ForEach(model.recentSpaceURLs.prefix(5), id: \.self) { url in
                Button {
                    Task { await model.openSpace(at: url) }
                } label: {
                    HStack(spacing: KSpace.x2) {
                        KIcon(.knode, size: .xs)
                        Text(url.deletingPathExtension().lastPathComponent).font(KFont.ui(14, .medium))
                    }
                }
                .buttonStyle(KLinkButtonStyle(muted: true))
                .kTooltip(url.path(percentEncoded: false))
            }
        }
        .frame(width: 300, alignment: .leading)
    }
}

/// Paper in light mode, ink in dark mode.
struct PaperBackground: View {
    @Environment(\.kTheme) private var theme

    var body: some View {
        theme.bg.ignoresSafeArea()
    }
}
