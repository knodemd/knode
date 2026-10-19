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

    /// True while the app runs on `PreviewSpaceStore` instead of the Knode core.
    let isPreview: Bool

    var selectedSpace: Space? { spaces.first { $0.id == selectedSpaceID } }

    var selectedNodeID: Node.ID? {
        get { editor?.nodeID }
        set { Task { await openNode(newValue) } }
    }

    var searchText = "" {
        didSet { if searchText != oldValue { scheduleSearch() } }
    }

    @ObservationIgnored private let store: any SpaceStore
    @ObservationIgnored private let searchDelay: Duration
    @ObservationIgnored private let saveDelay: Duration
    @ObservationIgnored private var pendingSearch: Task<Void, Never>?

    init(
        store: any SpaceStore,
        isPreview: Bool = false,
        searchDelay: Duration = .milliseconds(200),
        saveDelay: Duration = .milliseconds(600)
    ) {
        self.store = store
        self.isPreview = isPreview
        self.searchDelay = searchDelay
        self.saveDelay = saveDelay
    }

    // MARK: Spaces

    func openSpace(at url: URL) async {
        await show { try await self.store.openSpace(at: url) }
    }

    func createSpace(at url: URL) async {
        let name = url.deletingPathExtension().lastPathComponent
        await show { try await self.store.createSpace(at: url, name: name) }
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

    private func show(_ open: @escaping () async throws -> Space) async {
        do {
            let space = try await open()
            if !spaces.contains(where: { $0.id == space.id }) { spaces.append(space) }
            await selectSpace(space.id)
        } catch {
            errorMessage = error.localizedDescription
        }
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
            let result = query.isEmpty
                ? try await store.listNodes(in: space)
                : try await store.search(query, in: space)
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
            let node = try await store.node(id, in: space)
            guard space == selectedSpaceID else { return }
            let editor = NoteEditorModel(node: node, spaceID: space, store: store, saveDelay: saveDelay)
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
            let node = try await store.createNode(in: space, title: "Untitled", body: "")
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
            let all = try await store.listNodes(in: space)
            if let match = all.first(where: { $0.title.caseInsensitiveCompare(title) == .orderedSame }) {
                await openNode(match.id)
            }
        } catch {
            errorMessage = error.localizedDescription
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
