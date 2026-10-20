import Foundation

/// The `SpaceStore` that talks to Knode Service over its Unix socket, so the app writes to a space
/// together with the agents and sees their changes live.
///
/// It connects on first use and again after the service restarts. While Knode Service is not
/// running every call fails with `ServiceError.unavailable`; `RoutingSpaceStore` then uses the
/// core in the app instead.
public actor ServiceSpaceStore: CoeditingStore {
    /// The most nodes a list shows: the most `recent` answers with.
    static let listLimit: Int64 = 1000
    /// The most results a search shows.
    static let searchLimit: Int64 = 100
    /// The most notes whose first line one list loads; the rest show only their title until a
    /// later list.
    static let excerptBatch = 200

    private let socketURL: URL
    public nonisolated let person: String
    /// Who the app writes as, in the `by` of each write.
    let author: JSONValue
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
        self.person = person
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
        return await withIcon(register(info))
    }

    public func openSpace(at url: URL) async throws -> Space {
        let url = url.standardizedFileURL
        if let open = spaces.values.first(where: { $0.space.url == url }) { return open.space }
        let info = try await call("space.open", ["space": .string(Self.path(of: url))], as: ServiceSpaceInfo.self, url: url)
        return await withIcon(register(info))
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

    public func changes(of node: Node.ID, in space: Space.ID, limit: Int) async throws -> [ChangeRecord] {
        let path = try path(space)
        let limit = Int64(min(max(limit, 1), 1000))
        let changes = try await call("changes", ["space": .string(path), "subject": .string(node), "limit": .int(limit)], as: [WireChange].self, node: node)
        return changes.map(ChangeRecord.init)
    }

    public func restore(_ node: Node.ID, to version: Int64, in space: Space.ID) async throws -> Node {
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "id": .string(node), "version": .int(version), "by": author]
        return remember(try await call("restore", params, as: WireNode.self, node: node))
    }

    public func undo(change: Int64, in space: Space.ID, force: Bool) async throws {
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "change": .int(change), "force": .bool(force), "by": author]
        _ = try await call("undo", params, as: WireChange.self, change: change)
    }

    // MARK: Name, settings and folders

    public func renameSpace(_ space: Space.ID, to name: String) async throws -> Space {
        let path = try path(space)
        let info = try await call("space.rename", ["space": .string(path), "name": .string(name), "by": author], as: ServiceSpaceInfo.self)
        spaces[space]?.space.name = info.name
        return spaces[space]?.space ?? Space(id: info.id, name: info.name)
    }

    public func spaceDetails(_ space: Space.ID) async throws -> Space {
        let path = try path(space)
        // The name the service reads from the manifest, which a rename changes.
        let info = try await call("space.open", ["space": .string(path)], as: ServiceSpaceInfo.self)
        let icon = try await icon(of: space)
        spaces[space]?.space.name = info.name
        spaces[space]?.space.icon = icon
        return try open(space)
    }

    public func icon(of space: Space.ID) async throws -> String? {
        // A value that is not one emoji is kept as it is, and shown as no icon.
        try await settings(of: space)[SpaceIcon.settingKey].flatMap { SpaceIcon.isEmoji($0) ? $0 : nil }
    }

    public func setIcon(_ icon: String?, of space: Space.ID) async throws {
        let icon = try SpaceIcon.normalized(icon)
        try await setSetting(SpaceIcon.settingKey, to: icon, of: space)
        spaces[space]?.space.icon = icon
    }

    public func captureEnabled(in space: Space.ID) async throws -> Bool {
        // Anything but `on` means off: when in doubt, nothing is captured (specification, 15.1).
        try await settings(of: space)[CaptureSetting.key].map { $0 == "on" } ?? true
    }

    public func setCaptureEnabled(_ enabled: Bool, in space: Space.ID) async throws {
        try await setSetting(CaptureSetting.key, to: enabled ? "on" : "off", of: space)
    }

    public func folders(of space: Space.ID) async throws -> [SpaceFolder] {
        let path = try path(space)
        let data = try await callJSON("folders.list", ["space": .string(path)])
        guard let list = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] else {
            throw ServiceError.badResponse("folders.list did not answer with a list")
        }
        return list.compactMap(SpaceFolder.init(service:))
    }

    public func setFolder(_ folder: FolderDraft, in space: Space.ID) async throws -> SpaceFolder {
        let path = try path(space)
        guard let rules = JSONValue(jsonText: folder.rules) else {
            throw SpaceStoreError.invalid("the rules of a folder are a JSON object")
        }
        var params: [String: JSONValue] = [
            "space": .string(path), "kind": .string(folder.kind), "path": .string(folder.path),
            "capture": .bool(folder.capture), "rules": rules, "by": author,
        ]
        if let id = folder.id { params["id"] = .string(id) }
        let data = try await callJSON("folders.set", params)
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any], let set = SpaceFolder(service: object) else {
            throw ServiceError.badResponse("folders.set did not answer with a folder")
        }
        return set
    }

    public func removeFolder(_ folder: SpaceFolder.ID, from space: Space.ID) async throws {
        let path = try path(space)
        _ = try await call("folders.remove", ["space": .string(path), "id": .string(folder), "by": author], as: Bool.self)
    }

    private func settings(of space: Space.ID) async throws -> [String: String] {
        let path = try path(space)
        return try await call("settings.get", ["space": .string(path)], as: [String: String].self)
    }

    private func setSetting(_ key: String, to value: String?, of space: Space.ID) async throws {
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "key": .string(key), "value": value.map(JSONValue.string) ?? .null, "by": author]
        _ = try await call("settings.set", params, as: [String: String].self)
    }

    /// The space with its icon, read from the service; without it when the settings can't be read.
    private func withIcon(_ space: Space) async -> Space {
        guard let icon = try? await icon(of: space.id) else { return space }
        spaces[space.id]?.space.icon = icon
        return spaces[space.id]?.space ?? space
    }

    // MARK: Editing together

    public func openBody(_ node: Node.ID, in space: Space.ID) async throws -> BodyDocument {
        let path = try path(space)
        let open = try await call("doc.open", ["space": .string(path), "id": .string(node)], as: WireDocument.self, node: node)
        return BodyDocument(text: open.text, version: open.version)
    }

    public func editBody(_ node: Node.ID, in space: Space.ID, version: String, splices: [TextSplice]) async throws -> BodyEditResult {
        let path = try path(space)
        let wire: [JSONValue] = splices.map { ["at": .int(Int64($0.at)), "delete": .int(Int64($0.delete)), "insert": .string($0.insert)] }
        let params: [String: JSONValue] = ["space": .string(path), "id": .string(node), "version": .string(version), "splices": .array(wire), "by": author]
        let edit = try await call("doc.edit", params, as: WireDocEdit.self, node: node)
        return BodyEditResult(version: edit.version, seen: edit.seen, text: edit.text)
    }

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
            broadcast.yield(SpaceChange(
                space: space,
                nodeID: change.nodeId,
                kind: SpaceChange.Kind(rawValue: change.op) ?? .reload,
                version: change.version,
                author: change.by.map(Author.init)
            ))
        case "space":
            guard let event = try? notification.params(as: SpaceEvent.self),
                  let space = spaces.first(where: { $0.value.path == event.space })?.key
            else { return }
            let kind: SpaceChange.Kind
            switch event.what {
            case "renamed":
                if let name = event.name { spaces[space]?.space.name = name }
                kind = .renamed
            case "settings": kind = .settings
            case "folders": kind = .folders
            case "link_kinds": kind = .linkKinds
            case "views": kind = .views
            default: kind = .reload
            }
            broadcast.yield(SpaceChange(space: space, nodeID: nil, kind: kind))
        case "lagged":
            for id in spaces.keys { broadcast.yield(SpaceChange(space: id, nodeID: nil, kind: .reload)) }
        default:
            break
        }
    }

    func callJSON(_ method: String, _ params: [String: JSONValue]) async throws -> Data {
        do {
            return try await connected().callJSON(method, params)
        } catch ServiceError.remote(_, let kind, let message, let later) {
            throw Self.storeError(kind: kind, message: message, url: nil, node: nil, later: later)
        }
    }

    func call<Result: Decodable & Sendable>(_ method: String, _ params: [String: JSONValue], as type: Result.Type, url: URL? = nil, node: Node.ID? = nil, change: Int64? = nil) async throws -> Result {
        do {
            return try await connected().call(method, params, as: type)
        } catch ServiceError.remote(_, let kind, let message, let later) {
            throw Self.storeError(kind: kind, message: message, url: url, node: node, change: change, later: later)
        }
    }

    static func storeError(kind: String?, message: String, url: URL?, node: Node.ID?, change: Int64? = nil, later: [Int64] = []) -> any Error {
        switch kind {
        case "undo_conflict": return SpaceStoreError.undoConflict(change: change ?? 0, later: later, message: message)
        case "cannot_undo", "already_undone": return SpaceStoreError.cannotUndo(message)
        case "forbidden", "unauthenticated": return SpaceStoreError.personOnly(message)
        case "not_found": return node.map(SpaceStoreError.nodeNotFound) ?? SpaceStoreError.invalid(message)
        case "not_a_space": return url.map(SpaceStoreError.notASpace) ?? SpaceStoreError.invalid(message)
        case "already_exists": return url.map(SpaceStoreError.alreadyExists) ?? SpaceStoreError.invalid(message)
        case "invalid", "invalid_params", "unknown_version": return SpaceStoreError.invalid(message)
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

    private func open(_ space: Space.ID) throws -> Space {
        guard let open = spaces[space]?.space else { throw SpaceStoreError.spaceNotFound(space) }
        return open
    }

    func path(_ space: Space.ID) throws -> String {
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
    var kind: String?
}

struct WireSummary: Decodable, Sendable {
    var id: String
    var title: String
    var updatedAt: Int64
    var kind: String?
}

/// A link, as `link.create` returns it; the app only needs to know it was made.
struct WireLink: Decodable, Sendable {}

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

struct WireChange: Decodable, Sendable {
    var id: Int64
    var changedAt: Int64
    var changedBy: WireProvenance
    var action: String
    var subject: String
    var undoes: Int64?
}

/// A `space` notification: the space was renamed, or its settings or folders changed.
struct SpaceEvent: Decodable, Sendable {
    var space: String
    var what: String
    var name: String?
}

extension SpaceFolder {
    /// A folder as `folders.list` and `folders.set` answer, read with `JSONSerialization` so its
    /// rules keep their keys as written.
    init?(service object: [String: Any]) {
        guard let id = object["id"] as? String, let kind = object["kind"] as? String, let path = object["path"] as? String else { return nil }
        let rules = (object["rules"]).flatMap { try? JSONSerialization.data(withJSONObject: $0, options: [.sortedKeys, .fragmentsAllowed]) }
        self.init(
            id: id,
            kind: kind,
            path: path,
            capture: object["capture"] as? Bool ?? true,
            rules: rules.map { String(decoding: $0, as: UTF8.self) } ?? "{}",
            localURL: (object["local"] as? String).map { URL(filePath: $0, directoryHint: .isDirectory) },
            isMissing: object["missing"] as? Bool ?? false
        )
    }
}

struct Changed: Decodable, Sendable {
    var space: String
    var nodeId: String
    var op: String
    /// The version of the body's document, for a body written through it.
    var version: String?
    /// Who wrote it, for a body written through its document.
    var by: WireProvenance?
}

struct WireDocument: Decodable, Sendable {
    var text: String
    var version: String
}

struct WireDocEdit: Decodable, Sendable {
    var version: String
    var seen: String
    var text: String?
}

extension Node {
    init(_ node: WireNode) {
        self.init(
            id: node.id,
            title: node.title,
            body: node.body,
            createdAt: Date(unixMilliseconds: node.createdAt),
            updatedAt: Date(unixMilliseconds: node.updatedAt),
            kind: node.kind ?? NodeKind.note
        )
    }
}

extension NodeSummary {
    init(_ summary: WireSummary, excerpt: String) {
        self.init(id: summary.id, title: summary.title, excerpt: excerpt, updatedAt: Date(unixMilliseconds: summary.updatedAt), kind: summary.kind ?? NodeKind.note)
    }
}

extension Author {
    init(_ provenance: WireProvenance) {
        self.init(kind: Kind(rawValue: provenance.kind) ?? .system, name: provenance.actor, session: provenance.session, source: provenance.source)
    }
}

extension ChangeRecord {
    init(_ change: WireChange) {
        self.init(id: change.id, changedAt: Date(unixMilliseconds: change.changedAt), author: Author(change.changedBy), action: change.action, subject: change.subject, undoes: change.undoes)
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
            author: Author(entry.changedBy),
            title: entry.title,
            body: entry.body
        )
    }
}
