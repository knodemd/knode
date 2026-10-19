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

    public init(service: ServiceSpaceStore = ServiceSpaceStore(), core: CoreSpaceStore = CoreSpaceStore()) {
        self.service = service
        self.core = core
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
