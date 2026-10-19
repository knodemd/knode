import Foundation
import Observation

/// The state of the main window: the open spaces, the node list, the search and the open note.
@MainActor
@Observable
final class AppModel {
    private(set) var spaces: [Space] = []
    private(set) var selectedSpaceID: Space.ID?
    /// The nodes shown in the list: every node newest first, or the search results.
    private(set) var nodes: [NodeSummary] = []
    private(set) var editor: NoteEditorModel?
    var errorMessage: String?

    /// The spaces opened most recently, newest first.
    private(set) var recentSpaceURLs: [URL] = []

    var selectedSpace: Space? { spaces.first { $0.id == selectedSpaceID } }

    /// True when the selected space is the example space, kept in memory.
    var isShowingExample: Bool {
        selectedSpaceID.map(exampleSpaceIDs.contains) ?? false
    }

    var selectedNodeID: Node.ID? {
        get { editor?.nodeID }
        set { Task { await openNode(newValue) } }
    }

    var searchText = "" {
        didSet { if searchText != oldValue { scheduleSearch() } }
    }

    @ObservationIgnored private let store: any SpaceStore
    @ObservationIgnored private let exampleStore: any SpaceStore
    @ObservationIgnored private let recentSpaces: RecentSpaces?
    @ObservationIgnored private var exampleSpaceIDs: Set<Space.ID> = []
    @ObservationIgnored private let searchDelay: Duration
    @ObservationIgnored private let saveDelay: Duration
    @ObservationIgnored private var pendingSearch: Task<Void, Never>?
    @ObservationIgnored private var pendingReload: Task<Void, Never>?
    @ObservationIgnored private var changesTask: Task<Void, Never>?

    /// - Parameters:
    ///   - store: where spaces are read and written: the Knode core in the app.
    ///   - exampleStore: where the example space lives, in memory.
    ///   - recentSpaces: where the recent spaces are remembered; `nil` remembers nothing.
    init(
        store: any SpaceStore,
        exampleStore: any SpaceStore = PreviewSpaceStore(),
        recentSpaces: RecentSpaces? = nil,
        searchDelay: Duration = .milliseconds(200),
        saveDelay: Duration = .milliseconds(600)
    ) {
        self.store = store
        self.exampleStore = exampleStore
        self.recentSpaces = recentSpaces
        self.searchDelay = searchDelay
        self.saveDelay = saveDelay
        recentSpaceURLs = recentSpaces?.urls ?? []
        let changes = store.changes()
        changesTask = Task { [weak self] in
            for await change in changes {
                await self?.apply(change)
            }
        }
    }

    // MARK: Spaces

    func openSpace(at url: URL) async {
        await show(remembering: url) { try await self.store.openSpace(at: url) }
    }

    func createSpace(at url: URL) async {
        let name = url.deletingPathExtension().lastPathComponent
        await show(remembering: url) { try await self.store.createSpace(at: url, name: name) }
    }

    /// Opens the example space: example notes kept in memory, never written to disk.
    func openExampleSpace() async {
        await show {
            let space = try await self.exampleStore.openSpace(at: SpacePanels.exampleURL)
            self.exampleSpaceIDs.insert(space.id)
            return space
        }
    }

    func clearRecentSpaces() {
        recentSpaces?.clear()
        recentSpaceURLs = []
    }

    func selectSpace(_ id: Space.ID?) async {
        guard id != selectedSpaceID else { return }
        await editor?.save()
        selectedSpaceID = id
        editor = nil
        searchText = ""
        pendingSearch?.cancel()
        await reload()
    }

    private func show(remembering url: URL? = nil, _ open: @escaping () async throws -> Space) async {
        do {
            let space = try await open()
            if !spaces.contains(where: { $0.id == space.id }) { spaces.append(space) }
            if let recentSpaces, let url = space.url ?? url {
                recentSpaces.add(url)
                recentSpaceURLs = recentSpaces.urls
            }
            await selectSpace(space.id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// The store that holds a space: the example store for the example space.
    private func store(for space: Space.ID) -> any SpaceStore {
        exampleSpaceIDs.contains(space) ? exampleStore : store
    }

    // MARK: Nodes

    /// Reloads the list: every node, or the results for the current search.
    func reload() async {
        guard let space = selectedSpaceID else {
            nodes = []
            return
        }
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        do {
            let spaceStore = store(for: space)
            let result = query.isEmpty
                ? try await spaceStore.listNodes(in: space)
                : try await spaceStore.search(query, in: space)
            // A newer search or another space may have started meanwhile.
            guard space == selectedSpaceID, query == searchText.trimmingCharacters(in: .whitespacesAndNewlines) else { return }
            nodes = result
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func scheduleSearch() {
        pendingSearch?.cancel()
        let delay = searchDelay
        pendingSearch = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled else { return }
            await self?.reload()
        }
    }

    func openNode(_ id: Node.ID?) async {
        guard id != editor?.nodeID else { return }
        await editor?.save()
        guard let id, let space = selectedSpaceID else {
            editor = nil
            return
        }
        do {
            let spaceStore = store(for: space)
            let node = try await spaceStore.node(id, in: space)
            guard space == selectedSpaceID else { return }
            let editor = NoteEditorModel(node: node, spaceID: space, store: spaceStore, saveDelay: saveDelay)
            editor.onSave = { [weak self] node in self?.didSave(node) }
            self.editor = editor
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Creates an empty note at the top of the list and opens it.
    func createNode() async {
        guard let space = selectedSpaceID else { return }
        do {
            let node = try await store(for: space).createNode(in: space, title: "Untitled", body: "")
            searchText = ""
            pendingSearch?.cancel()
            await reload()
            await openNode(node.id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Opens the note a `[[Title]]` link points to, if there is one.
    func openLinkedNode(titled title: String) async {
        guard let space = selectedSpaceID else { return }
        do {
            let all = try await store(for: space).listNodes(in: space)
            if let match = all.first(where: { $0.title.caseInsensitiveCompare(title) == .orderedSame }) {
                await openNode(match.id)
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: Changes made by others

    /// Shows a change someone else made, such as an agent writing through Knode Service: the list
    /// reloads, and the open note too when it changed and has no unsaved edits.
    func apply(_ change: SpaceChange) async {
        guard change.space == selectedSpaceID else { return }
        scheduleReload()
        guard let editor, change.nodeID == editor.nodeID || change.kind == .reload else { return }
        do {
            let node = try await store(for: change.space).node(editor.nodeID, in: change.space)
            // The app's own saves come back as changes too: those are already shown.
            guard self.editor === editor, !editor.hasUnsavedChanges, node.updatedAt > editor.updatedAt else { return }
            let reloaded = NoteEditorModel(node: node, spaceID: change.space, store: store(for: change.space), saveDelay: saveDelay)
            reloaded.onSave = { [weak self] node in self?.didSave(node) }
            self.editor = reloaded
        } catch SpaceStoreError.nodeNotFound {
            if self.editor === editor { self.editor = nil }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Reloads the list once for a burst of changes.
    private func scheduleReload() {
        guard pendingReload == nil else { return }
        pendingReload = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(100))
            self?.pendingReload = nil
            await self?.reload()
        }
    }

    private func didSave(_ node: Node) {
        // Keep the list in step without a reload: move the note to the top, as it is now the newest.
        guard searchText.isEmpty else {
            if let index = nodes.firstIndex(where: { $0.id == node.id }) { nodes[index] = node.summary }
            return
        }
        nodes.removeAll { $0.id == node.id }
        nodes.insert(node.summary, at: 0)
    }
}
