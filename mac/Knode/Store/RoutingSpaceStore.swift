import Foundation

/// The `SpaceStore` of the app: through Knode Service when it is running, so the app and the
/// agents share each space and the app sees their changes live; through the core in the app
/// (`CoreSpaceStore`) when it is not, so the app keeps working on its own.
///
/// Each open space remembers which of the two holds it. A space held by the service moves to the
/// core when the service stops, and back when it starts again (`serviceDidStart()`); each move
/// sends a `reload` change.
public actor RoutingSpaceStore: SpaceStore {
    enum Backend: Equatable { case service, core }

    private let service: ServiceSpaceStore
    private let core: CoreSpaceStore
    private var open: [Space.ID: (url: URL, backend: Backend)] = [:]
    private nonisolated let broadcast = Broadcast<SpaceChange>()
    public nonisolated let person: String

    public init(service: ServiceSpaceStore = ServiceSpaceStore(), core: CoreSpaceStore = CoreSpaceStore()) {
        self.service = service
        self.core = core
        person = service.person
        let changes = service.changes()
        let broadcast = broadcast
        Task {
            for await change in changes { broadcast.yield(change) }
        }
    }

    public nonisolated func changes() -> AsyncStream<SpaceChange> {
        broadcast.stream()
    }

    /// Which store holds the space, for tests.
    func backend(of space: Space.ID) -> Backend? {
        open[space]?.backend
    }

    /// Moves every space the core holds to Knode Service, once it runs.
    public func serviceDidStart() async {
        for (id, entry) in open where entry.backend == .core {
            guard let space = try? await service.openSpace(at: entry.url) else { continue }
            core.close(id)
            open[space.id] = (entry.url, .service)
            broadcast.yield(SpaceChange(space: space.id, nodeID: nil, kind: .reload))
        }
    }

    // MARK: SpaceStore

    public func createSpace(at url: URL, name: String) async throws -> Space {
        try await openWith { store in try await store.createSpace(at: url, name: name) }
    }

    public func openSpace(at url: URL) async throws -> Space {
        try await openWith { store in try await store.openSpace(at: url) }
    }

    public func listNodes(in space: Space.ID) async throws -> [NodeSummary] {
        try await route(space) { store in try await store.listNodes(in: space) }
    }

    public func node(_ id: Node.ID, in space: Space.ID) async throws -> Node {
        try await route(space) { store in try await store.node(id, in: space) }
    }

    public func createNode(in space: Space.ID, title: String, body: String) async throws -> Node {
        // Not run again after the service stops mid-call: the note may have been made already.
        try await route(space, retry: false) { store in try await store.createNode(in: space, title: title, body: body) }
    }

    public func updateNode(_ id: Node.ID, in space: Space.ID, title: String?, body: String?) async throws -> Node {
        try await route(space) { store in try await store.updateNode(id, in: space, title: title, body: body) }
    }

    public func search(_ query: String, in space: Space.ID) async throws -> [NodeSummary] {
        try await route(space) { store in try await store.search(query, in: space) }
    }

    public func history(of node: Node.ID, in space: Space.ID) async throws -> [NoteVersion] {
        try await route(space) { store in try await store.history(of: node, in: space) }
    }

    public func changes(of node: Node.ID, in space: Space.ID, limit: Int) async throws -> [ChangeRecord] {
        try await route(space) { store in try await store.changes(of: node, in: space, limit: limit) }
    }

    // Restore and undo are writes: not run again after the service stops mid-call, as the service
    // may have made them already.

    public func restore(_ node: Node.ID, to version: Int64, in space: Space.ID) async throws -> Node {
        try await route(space, retry: false) { store in try await store.restore(node, to: version, in: space) }
    }

    public func undo(change: Int64, in space: Space.ID, force: Bool) async throws {
        try await route(space, retry: false) { store in try await store.undo(change: change, in: space, force: force) }
    }

    // The name, settings and folders of a space go through the service while it holds the space,
    // so its other clients and capture see each change at once. Each call is safe to run again
    // on the core if the service stops meanwhile: it leaves the space the same either way.

    public func renameSpace(_ space: Space.ID, to name: String) async throws -> Space {
        try await route(space) { store in try await store.renameSpace(space, to: name) }
    }

    public func spaceDetails(_ space: Space.ID) async throws -> Space {
        try await route(space) { store in try await store.spaceDetails(space) }
    }

    public func icon(of space: Space.ID) async throws -> String? {
        try await route(space) { store in try await store.icon(of: space) }
    }

    public func setIcon(_ icon: String?, of space: Space.ID) async throws {
        try await route(space) { store in try await store.setIcon(icon, of: space) }
    }

    public func captureEnabled(in space: Space.ID) async throws -> Bool {
        try await route(space) { store in try await store.captureEnabled(in: space) }
    }

    public func setCaptureEnabled(_ enabled: Bool, in space: Space.ID) async throws {
        try await route(space) { store in try await store.setCaptureEnabled(enabled, in: space) }
    }

    public func folders(of space: Space.ID) async throws -> [SpaceFolder] {
        try await route(space) { store in try await store.folders(of: space) }
    }

    public func setFolder(_ folder: FolderDraft, in space: Space.ID) async throws -> SpaceFolder {
        try await route(space) { store in try await store.setFolder(folder, in: space) }
    }

    public func removeFolder(_ folder: SpaceFolder.ID, from space: Space.ID) async throws {
        try await route(space) { store in try await store.removeFolder(folder, from: space) }
    }

    public func closeSpace(_ space: Space.ID) async {
        guard let entry = open.removeValue(forKey: space) else { return }
        if entry.backend == .service {
            await service.close(space)
        } else {
            core.close(space)
        }
    }

    // MARK: Routing

    private func store(_ backend: Backend) -> any SpaceStore {
        backend == .service ? service : core
    }

    /// Opens or creates a space through the service, or through the core when the service is not
    /// running.
    private func openWith(_ open: (any SpaceStore) async throws -> Space) async throws -> Space {
        let space: Space
        let backend: Backend
        do {
            space = try await open(service)
            backend = .service
        } catch let error as ServiceError where error.meansNotRunning {
            space = try await open(core)
            backend = .core
        }
        if let url = space.url { self.open[space.id] = (url, backend) }
        return space
    }

    /// Runs a call with the core in the app, opening the space there for the call when the
    /// service holds it.
    private func direct<T: Sendable>(_ space: Space.ID, _ work: (CoreSpaceStore) async throws -> T) async throws -> T {
        guard let entry = open[space] else { throw SpaceStoreError.spaceNotFound(space) }
        if entry.backend == .core { return try await work(core) }
        let wasOpen = core.isOpen(space)
        _ = try await core.openSpace(at: entry.url)
        defer {
            if !wasOpen, self.open[space]?.backend != .core { core.close(space) }
        }
        return try await work(core)
    }

    /// Runs a call on the store that holds the space. If the service stopped meanwhile, the space
    /// moves to the core and the call runs again there; with `retry` false, a call the service
    /// may have run before it stopped fails instead.
    private func route<T: Sendable>(_ space: Space.ID, retry: Bool = true, _ work: (any SpaceStore) async throws -> T) async throws -> T {
        guard let entry = open[space] else { throw SpaceStoreError.spaceNotFound(space) }
        do {
            return try await work(store(entry.backend))
        } catch let error as ServiceError where error.meansNotRunning && entry.backend == .service {
            _ = try await core.openSpace(at: entry.url)
            await service.close(space)
            open[space] = (entry.url, .core)
            broadcast.yield(SpaceChange(space: space, nodeID: nil, kind: .reload))
            if !retry, error == .disconnected { throw error }
            return try await work(core)
        }
    }
}

extension ServiceError {
    /// True when the service is not running, or stopped while answering.
    var meansNotRunning: Bool {
        switch self {
        case .unavailable, .disconnected: true
        case .remote, .badResponse: false
        }
    }
}

// MARK: - Editing together

// Bodies are edited together only while Knode Service holds the space. Without it the core in the
// app writes them whole, as before, and these calls fail with `SpaceStoreError.notCoediting`.
extension RoutingSpaceStore: CoeditingStore {
    public func openBody(_ node: Node.ID, in space: Space.ID) async throws -> BodyDocument {
        try await route(space) { store in
            guard let service = store as? ServiceSpaceStore else { throw SpaceStoreError.notCoediting }
            return try await service.openBody(node, in: space)
        }
    }

    public func editBody(_ node: Node.ID, in space: Space.ID, version: String, splices: [TextSplice]) async throws -> BodyEditResult {
        // Not run again after the service stops mid-call: the edit may have been written.
        try await route(space, retry: false) { store in
            guard let service = store as? ServiceSpaceStore else { throw SpaceStoreError.notCoediting }
            return try await service.editBody(node, in: space, version: version, splices: splices)
        }
    }
}

// MARK: - Map

// Knode Service has no methods yet for link kinds, labelled links, views, positions or canvases:
// these go straight to the space through the core in the app, which SQLite allows next to the
// service. Making and removing a link goes through the service while it holds the space, so its
// other clients see the change.
extension RoutingSpaceStore: MapStore {
    public func linkKinds(in space: Space.ID) async throws -> [LinkKindInfo] {
        try await direct(space) { core in try await core.linkKinds(in: space) }
    }

    public func setLinkKind(_ kind: LinkKindInfo, in space: Space.ID) async throws -> LinkKindInfo {
        try await direct(space) { core in try await core.setLinkKind(kind, in: space) }
    }

    public func links(of node: Node.ID, in space: Space.ID) async throws -> [NodeLinkInfo] {
        try await direct(space) { core in try await core.links(of: node, in: space) }
    }

    public func link(from: Node.ID, to: Node.ID, kind: String, in space: Space.ID) async throws {
        if open[space]?.backend == .service {
            try await route(space, retry: false) { store in
                if let service = store as? ServiceSpaceStore {
                    try await service.link(from: from, to: to, kind: kind, in: space)
                } else if let core = store as? CoreSpaceStore {
                    try await core.link(from: from, to: to, kind: kind, in: space)
                }
            }
        } else {
            try await direct(space) { core in try await core.link(from: from, to: to, kind: kind, in: space) }
        }
    }

    public func unlink(from: Node.ID, to: Node.ID, kind: String, in space: Space.ID) async throws {
        if open[space]?.backend == .service {
            try await route(space, retry: false) { store in
                if let service = store as? ServiceSpaceStore {
                    try await service.unlink(from: from, to: to, kind: kind, in: space)
                } else if let core = store as? CoreSpaceStore {
                    try await core.unlink(from: from, to: to, kind: kind, in: space)
                }
            }
        } else {
            try await direct(space) { core in try await core.unlink(from: from, to: to, kind: kind, in: space) }
        }
    }

    public func mapViews(in space: Space.ID) async throws -> [MapView] {
        try await direct(space) { core in try await core.mapViews(in: space) }
    }

    public func saveGraphView(id: MapView.ID?, name: String, filter: GraphFilter, in space: Space.ID) async throws -> MapView {
        try await direct(space) { core in try await core.saveGraphView(id: id, name: name, filter: filter, in: space) }
    }

    public func removeGraphView(_ id: MapView.ID, in space: Space.ID) async throws {
        try await direct(space) { core in try await core.removeGraphView(id, in: space) }
    }

    public func setViewport(_ viewport: Viewport, of view: MapView.ID, in space: Space.ID) async throws {
        try await direct(space) { core in try await core.setViewport(viewport, of: view, in: space) }
    }

    public func graph(in space: Space.ID, view: MapView.ID?, filter: GraphFilter) async throws -> GraphData {
        try await direct(space) { core in try await core.graph(in: space, view: view, filter: filter) }
    }

    public func setPositions(_ positions: [ViewPosition], in view: MapView.ID, space: Space.ID) async throws {
        try await direct(space) { core in try await core.setPositions(positions, in: view, space: space) }
    }

    public func removePositions(_ nodes: [Node.ID], from view: MapView.ID, space: Space.ID) async throws {
        try await direct(space) { core in try await core.removePositions(nodes, from: view, space: space) }
    }

    public func createCanvas(title: String, in space: Space.ID) async throws -> MapView {
        try await direct(space) { core in try await core.createCanvas(title: title, in: space) }
    }

    public func canvasBoard(of node: Node.ID, in space: Space.ID) async throws -> CanvasBoard? {
        try await direct(space) { core in try await core.canvasBoard(of: node, in: space) }
    }

    public func setGroup(_ group: GroupDraft, in view: MapView.ID, space: Space.ID) async throws -> ViewGroupInfo {
        try await direct(space) { core in try await core.setGroup(group, in: view, space: space) }
    }

    public func removeGroup(_ id: ViewGroupInfo.ID, in space: Space.ID) async throws {
        try await direct(space) { core in try await core.removeGroup(id, in: space) }
    }
}
