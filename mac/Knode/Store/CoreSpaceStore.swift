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
    public let person: String
    private let queue = DispatchQueue(label: "md.knode.core", qos: .userInitiated, attributes: .concurrent)

    /// - Parameter person: who the app writes as, recorded in the provenance of every change.
    ///   The full name of the macOS user by default.
    public init(person: String = CoreSpaceStore.currentUserName) {
        author = Provenance(kind: .person, actor: person, session: nil, source: nil)
        self.person = person
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
        let author = author
        let info = try await run { try core.renameAs(name: name, by: author) }
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
        _ = try await run { try core.changeSetting(key: SpaceIcon.settingKey, value: icon, by: author) }
        spaces.withLock { $0[space]?.space.icon = icon }
    }

    public func spaceDetails(_ space: Space.ID) async throws -> Space {
        let core = try self.core(space)
        let info = try await run { try core.info() }
        let icon = try await icon(of: space)
        return try spaces.withLock { spaces in
            guard var open = spaces[space] else { throw SpaceStoreError.spaceNotFound(space) }
            open.space.name = info.name
            open.space.icon = icon
            spaces[space] = open
            return open.space
        }
    }

    public func captureEnabled(in space: Space.ID) async throws -> Bool {
        let core = try self.core(space)
        return try await run { try core.captureEnabled() }
    }

    public func setCaptureEnabled(_ enabled: Bool, in space: Space.ID) async throws {
        let core = try self.core(space)
        let author = author
        _ = try await run { try core.changeSetting(key: CaptureSetting.key, value: enabled ? "on" : "off", by: author) }
    }

    public func folders(of space: Space.ID) async throws -> [SpaceFolder] {
        let core = try self.core(space)
        return try await run { try core.folders() }.map(SpaceFolder.init)
    }

    public func setFolder(_ folder: FolderDraft, in space: Space.ID) async throws -> SpaceFolder {
        let core = try self.core(space)
        let author = author
        let draft = KnodeCore.FolderDraft(id: folder.id, kind: folder.kind, path: folder.path, capture: folder.capture, rules: folder.rules)
        return SpaceFolder(try await run { try core.setFolder(folder: draft, by: author) })
    }

    public func removeFolder(_ folder: SpaceFolder.ID, from space: Space.ID) async throws {
        let core = try self.core(space)
        let author = author
        _ = try await run { try core.removeFolder(id: folder, by: author) }
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

    public func changes(of node: Node.ID, in space: Space.ID, limit: Int) async throws -> [ChangeRecord] {
        let core = try self.core(space)
        let limit = UInt32(clamping: limit)
        return try await run { try core.changesOf(id: node, limit: limit) }.map(ChangeRecord.init)
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

extension SpaceFolder {
    init(_ folder: KnodeCore.SpaceFolder) {
        self.init(
            id: folder.id,
            kind: folder.kind,
            path: folder.path,
            capture: folder.capture,
            rules: folder.rules,
            localURL: folder.local.map { URL(filePath: $0, directoryHint: .isDirectory) },
            isMissing: folder.missing
        )
    }
}

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
            updatedAt: Date(unixMilliseconds: node.updatedAt),
            kind: node.kind
        )
    }
}

extension NodeSummary {
    init(_ node: KnodeCore.NodeSummary, excerpt: String) {
        self.init(id: node.id, title: node.title, excerpt: excerpt, updatedAt: Date(unixMilliseconds: node.updatedAt), kind: node.kind)
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

extension ChangeRecord {
    init(_ change: KnodeCore.Change) {
        self.init(
            id: change.id,
            changedAt: Date(unixMilliseconds: change.changedAt),
            author: Author(change.changedBy),
            action: change.action,
            subject: change.subject,
            undoes: change.undoes
        )
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

// MARK: - Map

extension CoreSpaceStore: MapStore {
    public func linkKinds(in space: Space.ID) async throws -> [LinkKindInfo] {
        let core = try self.core(space)
        return try await run { try core.linkKinds() }.map(LinkKindInfo.init)
    }

    public func setLinkKind(_ kind: LinkKindInfo, in space: Space.ID) async throws -> LinkKindInfo {
        let core = try self.core(space)
        let author = author
        let spec = LinkKindSpec(kind: kind.kind, label: kind.label, inverseLabel: kind.inverseLabel, direction: kind.direction.core, description: kind.description)
        return LinkKindInfo(try await run { try core.setLinkKind(spec: spec, by: author) })
    }

    public func links(of node: Node.ID, in space: Space.ID) async throws -> [NodeLinkInfo] {
        let core = try self.core(space)
        return try await run { try core.nodeLinks(id: node) }.map(NodeLinkInfo.init)
    }

    public func link(from: Node.ID, to: Node.ID, kind: String, in space: Space.ID) async throws {
        let core = try self.core(space)
        let author = author
        _ = try await run { try core.link(from: from, to: to, kind: kind, by: author) }
    }

    public func unlink(from: Node.ID, to: Node.ID, kind: String, in space: Space.ID) async throws {
        let core = try self.core(space)
        let author = author
        _ = try await run { try core.unlink(from: from, to: to, kind: kind, by: author) }
    }

    public func mapViews(in space: Space.ID) async throws -> [MapView] {
        let core = try self.core(space)
        return try await run { try core.views() }.compactMap(MapView.init)
    }

    public func saveGraphView(id: MapView.ID?, name: String, filter: GraphFilter, in space: Space.ID) async throws -> MapView {
        let core = try self.core(space)
        let author = author
        let filter = filter.isEmpty ? nil : filter.core
        let view = try await run { try core.setGraphView(id: id, name: name, filter: filter, by: author) }
        guard let view = MapView(view) else { throw SpaceStoreError.storage("the view could not be read back") }
        return view
    }

    public func removeGraphView(_ id: MapView.ID, in space: Space.ID) async throws {
        let core = try self.core(space)
        let author = author
        _ = try await run { try core.removeView(id: id, by: author) }
    }

    public func setViewport(_ viewport: Viewport, of view: MapView.ID, in space: Space.ID) async throws {
        let core = try self.core(space)
        let author = author
        let value = KnodeCore.Viewport(x: viewport.x, y: viewport.y, zoom: viewport.zoom)
        try await run { try core.setViewport(id: view, viewport: value, by: author) }
    }

    public func graph(in space: Space.ID, view: MapView.ID?, filter: GraphFilter) async throws -> GraphData {
        let core = try self.core(space)
        let filter = filter.core
        let graph = try await run { try core.graph(view: view, filter: filter) }
        return GraphData(
            nodes: graph.nodes.map(GraphNodeInfo.init),
            edges: graph.edges.map(GraphEdgeInfo.init),
            positions: graph.positions.map(ViewPosition.init),
            linkKinds: graph.linkKinds.map(LinkKindInfo.init)
        )
    }

    public func setPositions(_ positions: [ViewPosition], in view: MapView.ID, space: Space.ID) async throws {
        guard !positions.isEmpty else { return }
        let core = try self.core(space)
        let author = author
        let values = positions.map(\.core)
        try await run { try core.setPositions(view: view, positions: values, by: author) }
    }

    public func removePositions(_ nodes: [Node.ID], from view: MapView.ID, space: Space.ID) async throws {
        let core = try self.core(space)
        _ = try await run { try core.removePositions(view: view, nodes: nodes) }
    }

    public func createCanvas(title: String, in space: Space.ID) async throws -> MapView {
        let core = try self.core(space)
        let author = author
        let view = try await run { try core.createCanvas(title: title, body: "", by: author) }
        guard let view = MapView(view) else { throw SpaceStoreError.storage("the canvas could not be read back") }
        return view
    }

    public func canvasBoard(of node: Node.ID, in space: Space.ID) async throws -> CanvasBoard? {
        let core = try self.core(space)
        guard let board = try await run({ try core.canvasBoard(node: node) }), let view = MapView(board.view) else { return nil }
        return CanvasBoard(
            view: view,
            items: board.items.map { CanvasItem(position: ViewPosition($0.position), kind: $0.kind, title: $0.title, excerpt: $0.excerpt) },
            groups: board.groups.map(ViewGroupInfo.init),
            edges: board.edges.map(GraphEdgeInfo.init),
            linkKinds: board.linkKinds.map(LinkKindInfo.init)
        )
    }

    public func setGroup(_ group: GroupDraft, in view: MapView.ID, space: Space.ID) async throws -> ViewGroupInfo {
        let core = try self.core(space)
        let author = author
        let spec = GroupSpec(id: group.id, label: group.label, x: group.x, y: group.y, width: group.width, height: group.height, collapsed: group.collapsed)
        return ViewGroupInfo(try await run { try core.setGroup(view: view, spec: spec, by: author) })
    }

    public func removeGroup(_ id: ViewGroupInfo.ID, in view: MapView.ID, space: Space.ID) async throws {
        let core = try self.core(space)
        _ = try await run { try core.removeGroup(id: id) }
    }
}

extension LinkDirection {
    init(_ direction: KnodeCore.LinkDirection) {
        self = direction == .symmetric ? .symmetric : .directed
    }

    var core: KnodeCore.LinkDirection {
        self == .symmetric ? .symmetric : .directed
    }
}

extension LinkKindInfo {
    init(_ kind: KnodeCore.LinkKind) {
        self.init(kind: kind.kind, label: kind.label, inverseLabel: kind.inverseLabel, direction: LinkDirection(kind.direction), description: kind.description, builtIn: kind.builtIn)
    }
}

extension NodeLinkInfo {
    init(_ link: KnodeCore.NodeLink) {
        self.init(
            from: link.from, to: link.to, kind: link.kind, outgoing: link.outgoing, nodeID: link.node,
            title: link.title, label: link.label, direction: LinkDirection(link.direction),
            createdAt: Date(unixMilliseconds: link.createdAt), author: Author(link.createdBy)
        )
    }
}

extension GraphFilter {
    init(_ filter: KnodeCore.GraphFilter) {
        self.init(
            nodeKinds: filter.nodeKinds, tags: filter.tags, actors: filter.actors,
            since: filter.since.map(Date.init(unixMilliseconds:)), until: filter.until.map(Date.init(unixMilliseconds:)),
            linkKinds: filter.linkKinds
        )
    }

    var core: KnodeCore.GraphFilter {
        KnodeCore.GraphFilter(
            nodeKinds: nodeKinds, tags: tags, actors: actors,
            since: since.map { Int64(($0.timeIntervalSince1970 * 1000).rounded()) },
            until: until.map { Int64(($0.timeIntervalSince1970 * 1000).rounded()) },
            linkKinds: linkKinds
        )
    }
}

extension MapView {
    /// A view of a kind this version knows, or nil.
    init?(_ view: KnodeCore.View) {
        guard let kind = Kind(rawValue: view.kind) else { return nil }
        self.init(
            id: view.id, kind: kind, name: view.name, nodeID: view.nodeId,
            viewport: view.viewport.map { Viewport(x: $0.x, y: $0.y, zoom: $0.zoom) },
            filter: view.filter.map(GraphFilter.init) ?? GraphFilter()
        )
    }
}

extension ViewPosition {
    init(_ position: KnodeCore.Position) {
        let size: CGSize? = if let width = position.width, let height = position.height { CGSize(width: width, height: height) } else { nil }
        self.init(nodeID: position.nodeId, x: position.x, y: position.y, size: size, z: position.z, collapsed: position.collapsed, groupID: position.groupId)
    }

    var core: KnodeCore.Position {
        KnodeCore.Position(nodeId: nodeID, x: x, y: y, width: size.map { Double($0.width) }, height: size.map { Double($0.height) }, z: z, collapsed: collapsed, groupId: groupID)
    }
}

extension ViewGroupInfo {
    init(_ group: KnodeCore.ViewGroup) {
        self.init(id: group.id, label: group.label, x: group.x, y: group.y, width: group.width, height: group.height, collapsed: group.collapsed)
    }
}

extension GraphNodeInfo {
    init(_ node: KnodeCore.GraphNode) {
        self.init(
            id: node.id, kind: node.kind, title: node.title, tags: node.tags,
            createdAt: Date(unixMilliseconds: node.createdAt), updatedAt: Date(unixMilliseconds: node.updatedAt),
            createdBy: node.createdBy, updatedBy: node.updatedBy
        )
    }
}

extension GraphEdgeInfo {
    init(_ edge: KnodeCore.GraphEdge) {
        self.init(from: edge.from, to: edge.to, kind: edge.kind)
    }
}

// MARK: - Attachments

extension CoreSpaceStore: AttachmentStore {
    public func attachments(of node: Node.ID, in space: Space.ID) async throws -> [AttachmentInfo] {
        let core = try self.core(space)
        return try await run { try core.attachments(node: node) }.map(AttachmentInfo.init)
    }

    public func attachFile(at url: URL, to node: Node.ID, in space: Space.ID) async throws -> AttachmentInfo {
        let core = try self.core(space)
        let author = author
        let path = url.standardizedFileURL.path(percentEncoded: false)
        return AttachmentInfo(try await run { try core.attachFile(node: node, path: path, by: author) })
    }

    public func attach(_ data: Data, named name: String, to node: Node.ID, in space: Space.ID) async throws -> AttachmentInfo {
        let core = try self.core(space)
        let author = author
        return AttachmentInfo(try await run { try core.attach(node: node, name: name, content: data, by: author) })
    }

    public func removeAttachment(named name: String, from node: Node.ID, in space: Space.ID) async throws {
        let core = try self.core(space)
        let author = author
        _ = try await run { try core.detach(node: node, name: name, by: author) }
    }

    public func attachmentURL(named name: String, of node: Node.ID, in space: Space.ID) async throws -> URL? {
        let core = try self.core(space)
        return try await run { try core.attachmentPath(node: node, name: name) }.map { URL(filePath: $0) }
    }
}

extension AttachmentInfo {
    init(_ attachment: KnodeCore.Attachment) {
        self.init(
            nodeID: attachment.nodeId, name: attachment.name, hash: attachment.hash, size: Int64(clamping: attachment.size),
            mediaType: attachment.mediaType, createdAt: Date(unixMilliseconds: attachment.createdAt), author: Author(attachment.createdBy)
        )
    }
}
