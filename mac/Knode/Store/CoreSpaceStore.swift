import Foundation
import KnodeCore
import Synchronization

/// The `SpaceStore` of the app: reads and writes real `.knode` packages through the Rust core
/// (`core/crates/knode-ffi`, bridged with UniFFI).
///
/// The core is synchronous, so every call runs on a queue of its own, never on the main thread.
/// Each open space is one `KnodeSpace`, which takes its calls in turn.
public final class CoreSpaceStore: SpaceStore {
    /// The most nodes a list shows.
    static let listLimit: UInt32 = 10_000
    /// The most results a search shows.
    static let searchLimit: UInt32 = 100

    private let spaces = Mutex<[Space.ID: (space: Space, core: KnodeSpace)]>([:])
    private let author: Provenance
    private let queue = DispatchQueue(label: "md.knode.core", qos: .userInitiated, attributes: .concurrent)

    /// - Parameter person: who the app writes as, recorded in the provenance of every change.
    ///   The full name of the macOS user by default.
    public init(person: String = CoreSpaceStore.currentUserName) {
        author = Provenance(kind: .person, actor: person, session: nil, source: nil)
    }

    /// The full name of the macOS user, or the account name if it has none.
    public static var currentUserName: String {
        let name = NSFullUserName().trimmingCharacters(in: .whitespaces)
        return name.isEmpty ? NSUserName() : name
    }

    public func createSpace(at url: URL, name: String) async throws -> Space {
        let path = url.standardizedFileURL.path(percentEncoded: false)
        let core = try await run { try KnodeSpace.create(path: path, name: name) }
        return try await register(core)
    }

    public func openSpace(at url: URL) async throws -> Space {
        let url = url.standardizedFileURL
        if let open = spaces.withLock({ $0.values.first { $0.space.url == url } }) { return open.space }
        let path = url.path(percentEncoded: false)
        let core = try await run { try KnodeSpace.open(path: path) }
        return try await register(core)
    }

    public func listNodes(in space: Space.ID) async throws -> [NodeSummary] {
        let core = try self.core(space)
        let recent = try await run { try core.recent(limit: Self.listLimit) }
        return recent.map { NodeSummary($0.node, excerpt: $0.excerpt) }
    }

    public func node(_ id: Node.ID, in space: Space.ID) async throws -> Node {
        let core = try self.core(space)
        guard let node = try await run({ try core.getNode(id: id) }) else { throw SpaceStoreError.nodeNotFound(id) }
        return Node(node)
    }

    public func createNode(in space: Space.ID, title: String, body: String) async throws -> Node {
        let core = try self.core(space)
        let author = author
        return Node(try await run { try core.createNode(title: title, body: body, by: author) })
    }

    /// A blank title leaves the title as it is: every node has one.
    public func updateNode(_ id: Node.ID, in space: Space.ID, title: String?, body: String?) async throws -> Node {
        let core = try self.core(space)
        let author = author
        let title = title.flatMap { $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : $0 }
        return Node(try await run { try core.updateNode(id: id, title: title, body: body, by: author) })
    }

    public func search(_ query: String, in space: Space.ID) async throws -> [NodeSummary] {
        let core = try self.core(space)
        let hits = try await run { try core.search(query: query, limit: Self.searchLimit) }
        // The snippet marks each match in **bold**; the list shows plain text.
        return hits.map { NodeSummary($0.node, excerpt: $0.snippet.replacing("**", with: "")) }
    }

    public func renameSpace(_ space: Space.ID, to name: String) async throws -> Space {
        let core = try self.core(space)
        let info = try await run { try core.rename(name: name) }
        return spaces.withLock { spaces in
            spaces[space]?.space.name = info.name
            return spaces[space]?.space ?? Space(id: info.id, name: info.name)
        }
    }

    public func icon(of space: Space.ID) async throws -> String? {
        let core = try self.core(space)
        let value = try await run { try core.setting(key: SpaceIcon.settingKey) }
        // A value that is not one emoji is kept as it is, and shown as no icon.
        return value.flatMap { SpaceIcon.isEmoji($0) ? $0 : nil }
    }

    public func setIcon(_ icon: String?, of space: Space.ID) async throws {
        let icon = try SpaceIcon.normalized(icon)
        let core = try self.core(space)
        let author = author
        _ = try await run { try core.setSetting(key: SpaceIcon.settingKey, value: icon, by: author) }
        spaces.withLock { $0[space]?.space.icon = icon }
    }

    public func closeSpace(_ space: Space.ID) async {
        close(space)
    }

    public func history(of node: Node.ID, in space: Space.ID) async throws -> [NoteVersion] {
        let core = try self.core(space)
        let history = try await run { try core.history(id: node) }
        return history.reversed().map(NoteVersion.init)
    }

    public func restore(_ node: Node.ID, to version: Int64, in space: Space.ID) async throws -> Node {
        let core = try self.core(space)
        let author = author
        return Node(try await run { try core.restore(id: node, seq: version, by: author) })
    }

    public func undo(change: Int64, in space: Space.ID, force: Bool) async throws {
        let core = try self.core(space)
        let author = author
        _ = try await run { try core.undo(change: change, by: author, force: force) }
    }

    /// Whether the core holds the space now.
    func isOpen(_ space: Space.ID) -> Bool {
        spaces.withLock { $0[space] != nil }
    }

    /// Closes the space: the core lets go of its files.
    public func close(_ space: Space.ID) {
        _ = spaces.withLock { $0.removeValue(forKey: space) }
    }

    // MARK: Helpers

    private func register(_ core: KnodeSpace) async throws -> Space {
        let info = try await run { try core.info() }
        let icon = try await run { try core.setting(key: SpaceIcon.settingKey) }.flatMap { SpaceIcon.isEmoji($0) ? $0 : nil }
        let space = Space(id: info.id, name: info.name, url: URL(filePath: info.path, directoryHint: .isDirectory).standardizedFileURL, icon: icon)
        return spaces.withLock { spaces in
            // The same space opened twice, through another path: keep the first.
            if let open = spaces[space.id] { return open.space }
            spaces[space.id] = (space, core)
            return space
        }
    }

    private func core(_ id: Space.ID) throws -> KnodeSpace {
        guard let core = spaces.withLock({ $0[id]?.core }) else { throw SpaceStoreError.spaceNotFound(id) }
        return core
    }

    /// Runs a call into the core off the main thread, with its errors as `SpaceStoreError`.
    private func run<T: Sendable>(_ work: @escaping @Sendable () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                do {
                    continuation.resume(returning: try work())
                } catch {
                    continuation.resume(throwing: Self.storeError(error))
                }
            }
        }
    }

    static func storeError(_ error: any Error) -> any Error {
        guard let error = error as? KnodeError else { return error }
        return switch error {
        case .NotASpace(let path): SpaceStoreError.notASpace(URL(filePath: path))
        case .AlreadyExists(let path): SpaceStoreError.alreadyExists(URL(filePath: path))
        case .NotFound(let id): SpaceStoreError.nodeNotFound(id)
        case .Unsupported(let message): SpaceStoreError.unsupported(message)
        case .Invalid(let message): SpaceStoreError.invalid(message)
        case .Storage(let message): SpaceStoreError.storage(message)
        case .ChangeNotFound(let id): SpaceStoreError.invalid("change \(id) not found")
        case .CannotUndo(let message): SpaceStoreError.cannotUndo(message)
        case .UndoConflict(let change, let later, let message): SpaceStoreError.undoConflict(change: change, later: later, message: message)
        case .PersonOnly(let message): SpaceStoreError.personOnly(message)
        case .Config(let message): SpaceStoreError.config(message)
        }
    }
}

// MARK: - From the core

extension Date {
    /// A date from Unix milliseconds, as the core stores them.
    init(unixMilliseconds: Int64) {
        self.init(timeIntervalSince1970: Double(unixMilliseconds) / 1000)
    }
}

extension Node {
    init(_ node: KnodeCore.Node) {
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
    init(_ node: KnodeCore.NodeSummary, excerpt: String) {
        self.init(id: node.id, title: node.title, excerpt: excerpt, updatedAt: Date(unixMilliseconds: node.updatedAt))
    }
}

extension Author {
    init(_ provenance: Provenance) {
        let kind: Kind = switch provenance.kind {
        case .person: .person
        case .agent: .agent
        case .system: .system
        }
        self.init(kind: kind, name: provenance.actor, session: provenance.session, source: provenance.source)
    }
}

extension NoteVersion {
    init(_ entry: HistoryEntry) {
        let operation: Operation = switch entry.op {
        case .create: .create
        case .update: .update
        case .delete: .delete
        }
        self.init(
            seq: entry.seq,
            nodeID: entry.nodeId,
            operation: operation,
            changeID: entry.changeId,
            restoredFrom: entry.restoredFrom,
            changedAt: Date(unixMilliseconds: entry.changedAt),
            author: Author(entry.changedBy),
            title: entry.title,
            body: entry.body
        )
    }
}
