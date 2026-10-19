import Foundation

/// The `SpaceStore` that talks to Knode Service over its Unix socket, so the app writes to a space
/// together with the agents and sees their changes live.
///
/// It connects on first use and again after the service restarts. While Knode Service is not
/// running every call fails with `ServiceError.unavailable`; `RoutingSpaceStore` then uses the
/// core in the app instead.
public actor ServiceSpaceStore: SpaceStore {
    /// The most nodes a list shows: the most `recent` answers with.
    static let listLimit: Int64 = 1000
    /// The most results a search shows.
    static let searchLimit: Int64 = 100
    /// The most notes whose first line one list loads; the rest show only their title until a
    /// later list.
    static let excerptBatch = 200

    private let socketURL: URL
    private let author: JSONValue
    private var connection: ServiceConnection?
    /// The open spaces: their URL and the path the service knows them by.
    private var spaces: [Space.ID: (space: Space, path: String)] = [:]
    /// The first line of each note listed, while the note does not change.
    private var excerpts: [Node.ID: (updatedAt: Int64, excerpt: String)] = [:]
    private nonisolated let broadcast = Broadcast<SpaceChange>()

    /// - Parameters:
    ///   - socketURL: the socket of Knode Service, `$KNODE_HOME/service.sock` by default.
    ///   - person: who the app writes as, recorded in the provenance of every change.
    public init(socketURL: URL = KnodeHome.current.socketURL, person: String = CoreSpaceStore.currentUserName) {
        self.socketURL = socketURL
        author = ["kind": "person", "actor": .string(person)]
    }

    /// Whether Knode Service answers on the socket now.
    public func isAvailable() async -> Bool {
        (try? await connected()) != nil
    }

    /// Stops using the space: the service keeps it open for others.
    public func close(_ space: Space.ID) {
        spaces[space] = nil
    }

    public nonisolated func changes() -> AsyncStream<SpaceChange> {
        broadcast.stream()
    }

    // MARK: SpaceStore

    public func createSpace(at url: URL, name: String) async throws -> Space {
        let path = Self.path(of: url)
        let info = try await call("space.create", ["space": .string(path), "name": .string(name)], as: ServiceSpaceInfo.self, url: url)
        return register(info)
    }

    public func openSpace(at url: URL) async throws -> Space {
        let url = url.standardizedFileURL
        if let open = spaces.values.first(where: { $0.space.url == url }) { return open.space }
        let info = try await call("space.open", ["space": .string(Self.path(of: url))], as: ServiceSpaceInfo.self, url: url)
        return register(info)
    }

    public func listNodes(in space: Space.ID) async throws -> [NodeSummary] {
        let path = try path(space)
        let recent = try await call("recent", ["space": .string(path), "limit": .int(Self.listLimit)], as: [WireSummary].self)
        var missing = recent.filter { excerpts[$0.id]?.updatedAt != $0.updatedAt }.prefix(Self.excerptBatch)
        while let summary = missing.popFirst() {
            guard let node = try await call("node.get", ["space": .string(path), "id": .string(summary.id)], as: WireNode?.self) else { continue }
            excerpts[node.id] = (node.updatedAt, NodeSummary.excerpt(of: node.body))
        }
        return recent.map { summary in
            let excerpt = excerpts[summary.id].flatMap { $0.updatedAt == summary.updatedAt ? $0.excerpt : nil } ?? ""
            return NodeSummary(summary, excerpt: excerpt)
        }
    }

    public func node(_ id: Node.ID, in space: Space.ID) async throws -> Node {
        let path = try path(space)
        guard let node = try await call("node.get", ["space": .string(path), "id": .string(id)], as: WireNode?.self, node: id) else {
            throw SpaceStoreError.nodeNotFound(id)
        }
        return remember(node)
    }

    public func createNode(in space: Space.ID, title: String, body: String) async throws -> Node {
        let path = try path(space)
        let node = try await call("node.create", ["space": .string(path), "title": .string(title), "body": .string(body), "by": author], as: WireNode.self)
        return remember(node)
    }

    /// A blank title leaves the title as it is: every node has one.
    public func updateNode(_ id: Node.ID, in space: Space.ID, title: String?, body: String?) async throws -> Node {
        let path = try path(space)
        var params: [String: JSONValue] = ["space": .string(path), "id": .string(id), "by": author]
        if let title, !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { params["title"] = .string(title) }
        if let body { params["body"] = .string(body) }
        return remember(try await call("node.update", params, as: WireNode.self, node: id))
    }

    public func search(_ query: String, in space: Space.ID) async throws -> [NodeSummary] {
        let path = try path(space)
        let hits = try await call("search", ["space": .string(path), "query": .string(query), "limit": .int(Self.searchLimit)], as: [WireHit].self)
        // The snippet marks each match in **bold**; the list shows plain text.
        return hits.map { NodeSummary($0.node, excerpt: $0.snippet.replacing("**", with: "")) }
    }

    public func history(of node: Node.ID, in space: Space.ID) async throws -> [NoteVersion] {
        let path = try path(space)
        let history = try await call("history", ["space": .string(path), "id": .string(node)], as: [WireHistoryEntry].self, node: node)
        return history.reversed().map(NoteVersion.init)
    }

    public func closeSpace(_ space: Space.ID) async {
        close(space)
    }

    // Knode Service has no methods yet to rename a space, change its settings, restore a version
    // or undo a change: `RoutingSpaceStore` does those with the core in the app.

    public func renameSpace(_ space: Space.ID, to name: String) async throws -> Space {
        throw Self.notYet
    }

    public func icon(of space: Space.ID) async throws -> String? {
        throw Self.notYet
    }

    public func setIcon(_ icon: String?, of space: Space.ID) async throws {
        throw Self.notYet
    }

    public func restore(_ node: Node.ID, to version: Int64, in space: Space.ID) async throws -> Node {
        throw Self.notYet
    }

    public func undo(change: Int64, in space: Space.ID, force: Bool) async throws {
        throw Self.notYet
    }

    static let notYet = SpaceStoreError.unsupported("Knode Service cannot do this yet")

    // MARK: Connection

    /// The open connection, or a new one subscribed to every change.
    private func connected() async throws -> ServiceConnection {
        if let connection, !connection.isClosed { return connection }
        connection = nil
        let connection = try ServiceConnection.connect(to: socketURL)
        let notifications = connection.notifications
        Task { [weak self] in
            for await notification in notifications {
                await self?.receive(notification)
            }
        }
        // Kept before `subscribe` answers: requests sent meanwhile go after it on the socket.
        self.connection = connection
        do {
            _ = try await connection.call("subscribe", as: Bool.self)
        } catch {
            connection.close()
            if self.connection === connection { self.connection = nil }
            throw error
        }
        // Changes made while there was no connection were missed.
        for id in spaces.keys { broadcast.yield(SpaceChange(space: id, nodeID: nil, kind: .reload)) }
        return connection
    }

    private func receive(_ notification: ServiceConnection.Notification) {
        switch notification.method {
        case "changed":
            guard let change = try? notification.params(as: Changed.self),
                  let space = spaces.first(where: { $0.value.path == change.space })?.key
            else { return }
            if change.op == "delete" { excerpts[change.nodeId] = nil }
            broadcast.yield(SpaceChange(space: space, nodeID: change.nodeId, kind: SpaceChange.Kind(rawValue: change.op) ?? .reload))
        case "lagged":
            for id in spaces.keys { broadcast.yield(SpaceChange(space: id, nodeID: nil, kind: .reload)) }
        default:
            break
        }
    }

    private func call<Result: Decodable & Sendable>(_ method: String, _ params: [String: JSONValue], as type: Result.Type, url: URL? = nil, node: Node.ID? = nil) async throws -> Result {
        do {
            return try await connected().call(method, params, as: type)
        } catch ServiceError.remote(_, let kind, let message) {
            throw Self.storeError(kind: kind, message: message, url: url, node: node)
        }
    }

    static func storeError(kind: String?, message: String, url: URL?, node: Node.ID?) -> any Error {
        switch kind {
        case "not_found": return node.map(SpaceStoreError.nodeNotFound) ?? SpaceStoreError.invalid(message)
        case "not_a_space": return url.map(SpaceStoreError.notASpace) ?? SpaceStoreError.invalid(message)
        case "already_exists": return url.map(SpaceStoreError.alreadyExists) ?? SpaceStoreError.invalid(message)
        case "invalid", "invalid_params": return SpaceStoreError.invalid(message)
        case "unsupported": return SpaceStoreError.unsupported(message)
        case "storage": return SpaceStoreError.storage(message)
        default: return SpaceStoreError.storage(message)
        }
    }

    // MARK: Helpers

    /// The absolute path the service takes, without a trailing slash.
    static func path(of url: URL) -> String {
        var path = url.standardizedFileURL.path(percentEncoded: false)
        while path.count > 1, path.hasSuffix("/") { path.removeLast() }
        return path
    }

    private func register(_ info: ServiceSpaceInfo) -> Space {
        if let open = spaces[info.id] { return open.space }
        let space = Space(id: info.id, name: info.name, url: URL(filePath: info.path, directoryHint: .isDirectory).standardizedFileURL)
        spaces[space.id] = (space, info.path)
        return space
    }

    private func path(_ space: Space.ID) throws -> String {
        guard let path = spaces[space]?.path else { throw SpaceStoreError.spaceNotFound(space) }
        return path
    }

    private func remember(_ node: WireNode) -> Node {
        excerpts[node.id] = (node.updatedAt, NodeSummary.excerpt(of: node.body))
        return Node(node)
    }
}

// MARK: - Wire types

/// What the service sends, as in `PROTOCOL.md`; keys are snake case.
struct ServiceSpaceInfo: Decodable, Sendable {
    var path: String
    var id: String
    var name: String
}

struct WireNode: Decodable, Sendable {
    var id: String
    var title: String
    var body: String
    var createdAt: Int64
    var updatedAt: Int64
}

struct WireSummary: Decodable, Sendable {
    var id: String
    var title: String
    var updatedAt: Int64
}

struct WireHit: Decodable, Sendable {
    var node: WireSummary
    var snippet: String
}

struct WireProvenance: Decodable, Sendable {
    var kind: String
    var actor: String
    var session: String?
    var source: String?
}

struct WireHistoryEntry: Decodable, Sendable {
    var seq: Int64
    var nodeId: String
    var op: String
    var changedAt: Int64
    var changedBy: WireProvenance
    var title: String
    var body: String
    var changeId: Int64?
    var restoredFrom: Int64?
}

struct Changed: Decodable, Sendable {
    var space: String
    var nodeId: String
    var op: String
}

extension Node {
    init(_ node: WireNode) {
        self.init(
            id: node.id,
            title: node.title,
            body: node.body,
            createdAt: Date(unixMilliseconds: node.createdAt),
            updatedAt: Date(unixMilliseconds: node.updatedAt)
        )
    }
}

extension NodeSummary {
    init(_ summary: WireSummary, excerpt: String) {
        self.init(id: summary.id, title: summary.title, excerpt: excerpt, updatedAt: Date(unixMilliseconds: summary.updatedAt))
    }
}

extension NoteVersion {
    init(_ entry: WireHistoryEntry) {
        self.init(
            seq: entry.seq,
            nodeID: entry.nodeId,
            operation: Operation(rawValue: entry.op) ?? .update,
            changeID: entry.changeId,
            restoredFrom: entry.restoredFrom,
            changedAt: Date(unixMilliseconds: entry.changedAt),
            author: Author(
                kind: Author.Kind(rawValue: entry.changedBy.kind) ?? .system,
                name: entry.changedBy.actor,
                session: entry.changedBy.session,
                source: entry.changedBy.source
            ),
            title: entry.title,
            body: entry.body
        )
    }
}
