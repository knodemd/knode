import Foundation
import Observation

/// A sheet over the main window.
enum SpaceSheet: Identifiable, Hashable {
    case newSpace
    case rename(URL)
    case icon(URL)

    var id: Self { self }
}

/// A space in the sidebar.
struct SpaceItem: Identifiable, Hashable {
    var url: URL
    var name: String
    var icon: String?
    /// The default space for agents and capture (`default_space` in `config.toml`).
    var isDefault: Bool
    /// The package is not where it was.
    var isMissing: Bool
    /// The example space, kept in memory.
    var isExample: Bool
    var id: URL { url }
}

/// The state of the main window: the spaces, the node list, the search and the open note.
@MainActor
@Observable
final class AppModel {
    /// The spaces open now.
    private(set) var spaces: [Space] = []
    /// The spaces in the sidebar, open or not.
    private(set) var library: [SpaceLibrary.Entry] = []
    /// The default space for agents and capture.
    private(set) var defaultSpaceURL: URL?
    /// The sheet shown over the window, if any.
    var sheet: SpaceSheet?
    private(set) var selectedSpaceID: Space.ID?
    /// The nodes shown in the list: every node newest first, or the search results.
    private(set) var nodes: [NodeSummary] = []
    private(set) var editor: NoteEditorModel? {
        didSet { if editor !== oldValue { makeHistory() } }
    }
    /// The history of the open note.
    private(set) var history: NoteHistoryModel?
    /// Whether the history of the open note is shown.
    var isShowingHistory = false {
        didSet { if isShowingHistory, !oldValue { Task { await history?.load() } } }
    }
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
    @ObservationIgnored private let spaceLibrary: SpaceLibrary?
    @ObservationIgnored let settings: any KnodeSettings
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
    ///   - library: where the spaces of the sidebar are kept; `nil` keeps them in memory.
    ///   - settings: the default space and the spaces folder, shared with the `knode` command.
    init(
        store: any SpaceStore,
        exampleStore: any SpaceStore = PreviewSpaceStore(),
        recentSpaces: RecentSpaces? = nil,
        library: SpaceLibrary? = nil,
        settings: any KnodeSettings = MemoryKnodeSettings(),
        searchDelay: Duration = .milliseconds(200),
        saveDelay: Duration = .milliseconds(600)
    ) {
        self.store = store
        self.exampleStore = exampleStore
        self.recentSpaces = recentSpaces
        self.spaceLibrary = library
        self.settings = settings
        self.searchDelay = searchDelay
        self.saveDelay = saveDelay
        recentSpaceURLs = recentSpaces?.urls ?? []
        defaultSpaceURL = try? settings.defaultSpace()
        if let library {
            if !library.hasBeenSaved {
                // The first time, the sidebar lists the spaces Knode already knows.
                let known = [defaultSpaceURL].compactMap { $0 } + recentSpaceURLs + settings.spacesInFolder()
                for url in known where FileManager.default.fileExists(atPath: url.path(percentEncoded: false)) {
                    library.add(url)
                }
                library.markSaved()
            }
            self.library = library.entries
        }
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

    /// Creates `<folder>/<name>.knode` and opens it; with `makeDefault`, agents and capture write
    /// to it from now on.
    func createSpace(named name: String, in folder: URL, makeDefault: Bool = false) async {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else {
            errorMessage = SpaceStoreError.invalid("a space needs a name").localizedDescription
            return
        }
        // A file name cannot hold `/` or `:`; the space keeps the name as typed.
        let fileName = name.replacing("/", with: "-").replacing(":", with: "-")
        let url = folder.appending(path: "\(fileName).knode", directoryHint: .isDirectory)
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        } catch {
            errorMessage = error.localizedDescription
            return
        }
        await show(remembering: url) { try await self.store.createSpace(at: url, name: name) }
        if makeDefault, let space = selectedSpace, space.url?.standardizedFileURL == url.standardizedFileURL {
            setDefaultSpace(url)
        }
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

    /// The spaces in the sidebar: the library, then the example space while it is open.
    var sidebarSpaces: [SpaceItem] {
        var items = library.map { entry in
            let open = spaces.first { $0.url?.standardizedFileURL == entry.url }
            return SpaceItem(
                url: entry.url,
                name: open?.name ?? entry.name,
                icon: open.map(\.icon) ?? entry.icon,
                isDefault: entry.url == defaultSpaceURL?.standardizedFileURL,
                isMissing: entry.isMissing,
                isExample: false
            )
        }
        for space in spaces where exampleSpaceIDs.contains(space.id) {
            items.append(SpaceItem(url: SpacePanels.exampleURL, name: space.name, icon: space.icon, isDefault: false, isMissing: false, isExample: true))
        }
        return items
    }

    /// The sidebar item of the selected space.
    var selectedSpaceURL: URL? {
        guard let space = selectedSpace else { return nil }
        return exampleSpaceIDs.contains(space.id) ? SpacePanels.exampleURL : space.url?.standardizedFileURL
    }

    /// Switches to a space of the sidebar, opening it if needed.
    func selectSpace(at url: URL?) async {
        guard let url else { return }
        if let open = space(at: url) {
            await selectSpace(open.id)
        } else if url == SpacePanels.exampleURL {
            await openExampleSpace()
        } else {
            await openSpace(at: url)
        }
    }

    /// Switches to the space at this place in the sidebar, counting from 1 (Command-1 to 9).
    func selectSpace(number: Int) async {
        let items = sidebarSpaces
        guard items.indices.contains(number - 1) else { return }
        await selectSpace(at: items[number - 1].url)
    }

    /// The open space at a sidebar URL.
    func space(at url: URL) -> Space? {
        if url == SpacePanels.exampleURL { return spaces.first { exampleSpaceIDs.contains($0.id) } }
        return spaces.first { $0.url?.standardizedFileURL == url.standardizedFileURL && !exampleSpaceIDs.contains($0.id) }
    }

    /// Renames a space: its name in `manifest.json`. The package keeps its file name.
    func renameSpace(at url: URL, to name: String) async {
        do {
            let space = try await openedSpace(at: url)
            let renamed = try await store(for: space.id).renameSpace(space.id, to: name)
            update(space.id) { $0.name = renamed.name }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Sets the icon of a space, one emoji, or removes it with `nil`.
    func setIcon(_ icon: String?, forSpaceAt url: URL) async {
        do {
            let icon = try SpaceIcon.normalized(icon)
            let space = try await openedSpace(at: url)
            try await store(for: space.id).setIcon(icon, of: space.id)
            update(space.id) { $0.icon = icon }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Takes a space off the sidebar and closes it. Its files stay where they are.
    func removeFromList(_ url: URL) async {
        if let space = space(at: url) {
            if space.id == selectedSpaceID {
                await editor?.save()
                await selectSpace(nil)
            }
            spaces.removeAll { $0.id == space.id }
            if exampleSpaceIDs.remove(space.id) != nil {
                await exampleStore.closeSpace(space.id)
            } else {
                await store.closeSpace(space.id)
            }
        }
        spaceLibrary?.remove(url)
        library.removeAll { $0.url == url.standardizedFileURL }
    }

    /// Makes a space the default for agents and capture (`default_space` in `config.toml`), or
    /// leaves none with `nil`.
    func setDefaultSpace(_ url: URL?) {
        do {
            try settings.setDefaultSpace(url)
            defaultSpaceURL = try settings.defaultSpace()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Reads the default space again, as the `knode` command may have changed it.
    func reloadDefaultSpace() {
        defaultSpaceURL = try? settings.defaultSpace()
    }

    private func openedSpace(at url: URL) async throws -> Space {
        if let space = space(at: url) { return space }
        await openSpace(at: url)
        guard let space = space(at: url) else { throw SpaceStoreError.notASpace(url) }
        return space
    }

    /// Changes an open space here and in the sidebar.
    private func update(_ id: Space.ID, _ change: (inout Space) -> Void) {
        guard let index = spaces.firstIndex(where: { $0.id == id }) else { return }
        change(&spaces[index])
        let space = spaces[index]
        guard !exampleSpaceIDs.contains(id), let url = space.url else { return }
        remember(space, at: url)
    }

    /// Puts a space on the sidebar list, with its name and icon as they are now.
    private func remember(_ space: Space, at url: URL) {
        let url = url.standardizedFileURL
        spaceLibrary?.add(url, name: space.name, icon: .some(space.icon))
        if let index = library.firstIndex(where: { $0.url == url }) {
            library[index].name = space.name
            library[index].icon = space.icon
            library[index].isMissing = false
        } else {
            library.append(SpaceLibrary.Entry(url: url, name: space.name, icon: space.icon))
        }
    }

    private func show(remembering url: URL? = nil, _ open: @escaping () async throws -> Space) async {
        do {
            let space = try await open()
            if let index = spaces.firstIndex(where: { $0.id == space.id }) {
                spaces[index] = space
            } else {
                spaces.append(space)
            }
            if !exampleSpaceIDs.contains(space.id), let url = space.url ?? url {
                remember(space, at: url)
                if let recentSpaces {
                    recentSpaces.add(url)
                    recentSpaceURLs = recentSpaces.urls
                }
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
            editor = makeEditor(node, in: space)
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

    private func makeEditor(_ node: Node, in space: Space.ID) -> NoteEditorModel {
        let editor = NoteEditorModel(node: node, spaceID: space, store: store(for: space), saveDelay: saveDelay)
        editor.onSave = { [weak self] node in
            self?.didSave(node)
            Task { await self?.history?.load() }
        }
        return editor
    }

    // MARK: History

    func toggleHistory() {
        isShowingHistory.toggle()
    }

    /// Undoes the last change this person made to the open note, through its history: Edit >
    /// Undo Last Change. Returns false when there was nothing of theirs to undo.
    @discardableResult
    func undoLastChange() async -> Bool {
        await history?.undoLastChange() ?? false
    }

    /// Redoes the change this person undid last in the open note: Edit > Redo Last Change.
    /// Returns false when there was nothing to redo.
    @discardableResult
    func redoLastChange() async -> Bool {
        await history?.redoLastChange() ?? false
    }

    private func makeHistory() {
        guard let editor else {
            history = nil
            return
        }
        let history = NoteHistoryModel(nodeID: editor.nodeID, spaceID: editor.spaceID, store: store(for: editor.spaceID))
        history.currentTitle = { [weak editor] in editor?.title ?? "" }
        history.currentBody = { [weak editor] in editor?.body ?? "" }
        // Unsaved edits are saved first, so the history has them and an undo sees them.
        history.willChange = { [weak editor] in await editor?.save() }
        history.didChange = { [weak self] in await self?.reloadEditor() }
        self.history = history
        if isShowingHistory { Task { await history.load() } }
    }

    /// Reads the open note again after its history changed it, and the list with it.
    private func reloadEditor() async {
        guard let editor else { return }
        do {
            let node = try await store(for: editor.spaceID).node(editor.nodeID, in: editor.spaceID)
            guard self.editor === editor else { return }
            // A new editor comes with a new history, loaded when the panel shows.
            self.editor = makeEditor(node, in: editor.spaceID)
            await reload()
        } catch SpaceStoreError.nodeNotFound {
            if self.editor === editor { self.editor = nil }
            await reload()
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
            self.editor = makeEditor(node, in: change.space)
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
