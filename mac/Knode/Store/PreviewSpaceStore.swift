import Foundation

/// An in-memory `SpaceStore` with example notes, for SwiftUI previews, tests and the example space.
///
/// It never reads or writes the `.knode` package: opening one shows example nodes, and every change
/// stays in memory until the app quits.
public actor PreviewSpaceStore: SpaceStore {
    private struct Entry {
        var space: Space
        var nodes: [Node.ID: Node]
        /// Every version of every node, oldest first.
        var versions: [NoteVersion] = []
        /// Every change, oldest first: one per version, and the undos.
        var changes: [ChangeRecord] = []
        /// Links made by hand; `mentions` links are read from the bodies.
        var links: [GraphEdgeInfo] = []
        /// The kinds of link the space defined.
        var linkKinds: [LinkKindInfo] = []
        var views: [MapView] = []
        var positions: [MapView.ID: [Node.ID: ViewPosition]] = [:]
        var groups: [MapView.ID: [ViewGroupInfo]] = [:]
    }

    private var spaces: [Space.ID: Entry] = [:]
    private let now: @Sendable () -> Date
    private let seedExamples: Bool
    private let author: Author
    public nonisolated let person: String
    private var sequence: Int64 = 0

    /// - Parameters:
    ///   - seedExamples: fill every opened space with the example nodes.
    ///   - person: who changes are made by.
    ///   - now: the clock, replaceable in tests.
    public init(seedExamples: Bool = true, person: String = "You", now: @escaping @Sendable () -> Date = { Date() }) {
        self.seedExamples = seedExamples
        self.author = Author(kind: .person, name: person)
        self.person = person
        self.now = now
    }

    public func createSpace(at url: URL, name: String) async throws -> Space {
        guard url.pathExtension == "knode" else { throw SpaceStoreError.notASpace(url) }
        let space = Space(id: ULID.make(at: now()), name: name, url: url)
        spaces[space.id] = Entry(space: space, nodes: [:])
        return space
    }

    public func openSpace(at url: URL) async throws -> Space {
        guard url.pathExtension == "knode" else { throw SpaceStoreError.notASpace(url) }
        if let open = spaces.values.first(where: { $0.space.url == url }) { return open.space }
        let space = Space(id: ULID.make(at: now()), name: url.deletingPathExtension().lastPathComponent, url: url)
        let nodes = seedExamples ? Self.examples(endingAt: now()) : []
        var entry = Entry(space: space, nodes: Dictionary(uniqueKeysWithValues: nodes.map { ($0.id, $0) }))
        for (index, node) in nodes.sorted(by: { $0.createdAt < $1.createdAt }).enumerated() {
            // The examples are written by an agent, as captured notes would be.
            let by = index.isMultiple(of: 2) ? Author(kind: .agent, name: "claude-code", session: "example") : author
            let created = version(of: node, .create, by: by)
            entry.versions.append(created)
            entry.changes.append(record(created, action: ChangeRecord.Action.create))
        }
        spaces[space.id] = entry
        return space
    }

    public func listNodes(in space: Space.ID) async throws -> [NodeSummary] {
        try entry(space).nodes.values
            .sorted { ($0.updatedAt, $0.id) > ($1.updatedAt, $1.id) }
            .map(\.summary)
    }

    public func node(_ id: Node.ID, in space: Space.ID) async throws -> Node {
        guard let node = try entry(space).nodes[id] else { throw SpaceStoreError.nodeNotFound(id) }
        return node
    }

    public func createNode(in space: Space.ID, title: String, body: String) async throws -> Node {
        _ = try entry(space)
        let date = now()
        let node = Node(id: ULID.make(at: date), title: title, body: body, createdAt: date, updatedAt: date)
        spaces[space]?.nodes[node.id] = node
        append(version(of: node, .create, by: author), action: ChangeRecord.Action.create, in: space)
        return node
    }

    public func updateNode(_ id: Node.ID, in space: Space.ID, title: String?, body: String?) async throws -> Node {
        var node = try await self.node(id, in: space)
        let before = node
        if let title { node.title = title }
        if let body { node.body = body }
        guard node != before else { return node }
        node.updatedAt = now()
        spaces[space]?.nodes[id] = node
        append(version(of: node, .update, by: author), action: ChangeRecord.Action.update, in: space)
        return node
    }

    public func renameSpace(_ space: Space.ID, to name: String) async throws -> Space {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { throw SpaceStoreError.invalid("a space needs a name") }
        _ = try entry(space)
        spaces[space]?.space.name = name
        return try entry(space).space
    }

    public func icon(of space: Space.ID) async throws -> String? {
        try entry(space).space.icon
    }

    public func setIcon(_ icon: String?, of space: Space.ID) async throws {
        let icon = try SpaceIcon.normalized(icon)
        _ = try entry(space)
        spaces[space]?.space.icon = icon
    }

    public func closeSpace(_ space: Space.ID) async {
        spaces[space] = nil
    }

    public func history(of node: Node.ID, in space: Space.ID) async throws -> [NoteVersion] {
        try entry(space).versions.filter { $0.nodeID == node }.reversed()
    }

    public func changes(of node: Node.ID, in space: Space.ID, limit: Int) async throws -> [ChangeRecord] {
        Array(try entry(space).changes.filter { $0.subject == node }.reversed().prefix(limit))
    }

    public func restore(_ node: Node.ID, to version: Int64, in space: Space.ID) async throws -> Node {
        let entry = try entry(space)
        guard let old = entry.versions.first(where: { $0.nodeID == node && $0.seq == version }) else {
            throw SpaceStoreError.invalid("node \(node) has no version \(version)")
        }
        return try put(old, in: space, action: ChangeRecord.Action.restore, undoes: nil)
    }

    /// Undoes a change by putting back the version before it. Undoing an undo redoes the change it
    /// undid. Later changes to the note, other than undos of this change, are a conflict.
    public func undo(change: Int64, in space: Space.ID, force: Bool) async throws {
        let entry = try entry(space)
        guard let record = entry.changes.first(where: { $0.id == change }),
              let undone = entry.versions.first(where: { $0.changeID == change })
        else {
            throw SpaceStoreError.invalid("change \(change) not found")
        }
        if let undoneBy = activeUndo(of: change, in: entry.changes) {
            throw SpaceStoreError.cannotUndo("change \(change) was already undone by change \(undoneBy)")
        }
        let chain = undoChain(of: change, in: entry.changes)
        let mine = entry.versions.filter { $0.nodeID == undone.nodeID }
        let later = mine.filter { $0.seq > undone.seq }.compactMap(\.changeID).filter { !chain.contains($0) }
        if !later.isEmpty, !force {
            throw SpaceStoreError.undoConflict(change: change, later: later, message: "undoing change \(change) would overwrite later changes")
        }
        guard let before = mine.last(where: { $0.seq < undone.seq }) else {
            throw SpaceStoreError.cannotUndo("undoing the creation of a note deletes it")
        }
        _ = try put(before, in: space, action: ChangeRecord.Action.undo, undoes: record.id)
    }

    /// The undo of a change that is in effect: one that was not undone itself.
    private func activeUndo(of change: Int64, in changes: [ChangeRecord]) -> Int64? {
        changes.first { $0.undoes == change && activeUndo(of: $0.id, in: changes) == nil }?.id
    }

    /// A change with its undos, the undos of those, and so on.
    private func undoChain(of change: Int64, in changes: [ChangeRecord]) -> Set<Int64> {
        var chain: Set<Int64> = [change]
        var open = [change]
        while let next = open.popLast() {
            for undo in changes where undo.undoes == next && chain.insert(undo.id).inserted {
                open.append(undo.id)
            }
        }
        return chain
    }

    /// Writes an old version back as a new one, as a change of its own.
    private func put(_ old: NoteVersion, in space: Space.ID, action: String, undoes: Int64?) throws -> Node {
        guard var node = try entry(space).nodes[old.nodeID] else { throw SpaceStoreError.nodeNotFound(old.nodeID) }
        // Restoring the version the note already has writes nothing.
        if action == ChangeRecord.Action.restore, node.title == old.title, node.body == old.body { return node }
        node.title = old.title
        node.body = old.body
        node.updatedAt = now()
        spaces[space]?.nodes[node.id] = node
        var restored = version(of: node, .update, by: author)
        restored.restoredFrom = old.seq
        append(restored, action: action, undoes: undoes, in: space)
        return node
    }

    private func append(_ version: NoteVersion, action: String, undoes: Int64? = nil, in space: Space.ID) {
        spaces[space]?.versions.append(version)
        spaces[space]?.changes.append(record(version, action: action, undoes: undoes))
    }

    private func record(_ version: NoteVersion, action: String, undoes: Int64? = nil) -> ChangeRecord {
        ChangeRecord(id: version.changeID ?? version.seq, changedAt: version.changedAt, author: version.author, action: action, subject: version.nodeID, undoes: undoes)
    }

    private func version(of node: Node, _ operation: NoteVersion.Operation, by author: Author) -> NoteVersion {
        sequence += 1
        return NoteVersion(seq: sequence, nodeID: node.id, operation: operation, changeID: sequence, changedAt: node.updatedAt, author: author, title: node.title, body: node.body)
    }

    public func search(_ query: String, in space: Space.ID) async throws -> [NodeSummary] {
        let terms = query.lowercased().split(whereSeparator: \.isWhitespace).map(String.init)
        let nodes = try entry(space).nodes.values
        guard !terms.isEmpty else { return try await listNodes(in: space) }

        func score(_ node: Node) -> Int? {
            let title = node.title.lowercased()
            let body = node.body.lowercased()
            var score = 0
            for term in terms {
                if title.contains(term) { score += 2 } else if body.contains(term) { score += 1 } else { return nil }
            }
            return score
        }

        return nodes
            .compactMap { node in score(node).map { (node, $0) } }
            .sorted { ($0.1, $0.0.updatedAt) > ($1.1, $1.0.updatedAt) }
            .map(\.0.summary)
    }

    private func entry(_ id: Space.ID) throws -> Entry {
        guard let entry = spaces[id] else { throw SpaceStoreError.spaceNotFound(id) }
        return entry
    }
}

// MARK: - Map

extension PreviewSpaceStore: MapStore {
    public func linkKinds(in space: Space.ID) async throws -> [LinkKindInfo] {
        LinkKindInfo.builtIns + (try entry(space).linkKinds.sorted { $0.kind < $1.kind })
    }

    public func setLinkKind(_ kind: LinkKindInfo, in space: Space.ID) async throws -> LinkKindInfo {
        let entry = try entry(space)
        guard LinkKindInfo.isValidName(kind.kind) else {
            throw SpaceStoreError.invalid("a link kind's name is lower-case letters, digits and -, starting with a letter")
        }
        guard !LinkKindInfo.builtIns.contains(where: { $0.kind == kind.kind }) else {
            throw SpaceStoreError.invalid("\(kind.kind) is a built-in kind")
        }
        let label = kind.label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !label.isEmpty else { throw SpaceStoreError.invalid("a link kind needs a label") }
        var stored = kind
        stored.label = label
        stored.inverseLabel = kind.inverseLabel.trimmingCharacters(in: .whitespacesAndNewlines)
        stored.builtIn = false
        if let old = entry.linkKinds.first(where: { $0.kind == kind.kind }), old.direction != kind.direction,
           entry.links.contains(where: { $0.kind == kind.kind }) {
            throw SpaceStoreError.invalid("the direction of a kind cannot change while links have it")
        }
        spaces[space]?.linkKinds.removeAll { $0.kind == kind.kind }
        spaces[space]?.linkKinds.append(stored)
        return stored
    }

    public func links(of node: Node.ID, in space: Space.ID) async throws -> [NodeLinkInfo] {
        let entry = try entry(space)
        let kinds = try await linkKinds(in: space)
        let edges = allEdges(entry)
        func kind(_ name: String) -> LinkKindInfo { kinds.first { $0.kind == name } ?? LinkKindInfo(kind: name, label: name) }
        func info(_ edge: GraphEdgeInfo, outgoing: Bool) -> NodeLinkInfo? {
            let other = outgoing ? edge.to : edge.from
            guard let title = entry.nodes[other]?.title else { return nil }
            let kind = kind(edge.kind)
            return NodeLinkInfo(from: edge.from, to: edge.to, kind: edge.kind, outgoing: outgoing, nodeID: other, title: title, label: kind.label(outgoing: outgoing), direction: kind.direction, createdAt: now(), author: author)
        }
        let out = edges.filter { $0.from == node }.compactMap { info($0, outgoing: true) }
        let back = edges.filter { $0.to == node && $0.from != node }.filter { edge in
            !(kind(edge.kind).direction == .symmetric && out.contains { $0.kind == edge.kind && $0.nodeID == edge.from })
        }.compactMap { info($0, outgoing: false) }
        let order: (NodeLinkInfo, NodeLinkInfo) -> Bool = { ($0.kind, $0.title.lowercased()) < ($1.kind, $1.title.lowercased()) }
        return out.sorted(by: order) + back.sorted(by: order)
    }

    public func link(from: Node.ID, to: Node.ID, kind: String, in space: Space.ID) async throws {
        let entry = try entry(space)
        let kind = kind.isEmpty ? LinkKindInfo.related : kind
        guard entry.nodes[from] != nil else { throw SpaceStoreError.nodeNotFound(from) }
        guard entry.nodes[to] != nil else { throw SpaceStoreError.nodeNotFound(to) }
        guard kind != LinkKindInfo.mentions else { throw SpaceStoreError.invalid("mentions links come only from [[links]] in a body") }
        guard let known = try await linkKinds(in: space).first(where: { $0.kind == kind }) else {
            throw SpaceStoreError.invalid("unknown link kind \"\(kind)\": define it first with a label")
        }
        let edge = GraphEdgeInfo(from: from, to: to, kind: kind)
        let reverse = GraphEdgeInfo(from: to, to: from, kind: kind)
        if entry.links.contains(edge) || (known.direction == .symmetric && entry.links.contains(reverse)) { return }
        spaces[space]?.links.append(edge)
    }

    public func unlink(from: Node.ID, to: Node.ID, kind: String, in space: Space.ID) async throws {
        _ = try entry(space)
        let symmetric = try await linkKinds(in: space).first { $0.kind == kind }?.direction == .symmetric
        spaces[space]?.links.removeAll { edge in
            edge.kind == kind && ((edge.from == from && edge.to == to) || (symmetric && edge.from == to && edge.to == from))
        }
    }

    public func mapViews(in space: Space.ID) async throws -> [MapView] {
        let entry = try entry(space)
        return entry.views.compactMap { view in
            guard view.kind == .canvas else { return view }
            guard let node = view.nodeID.flatMap({ entry.nodes[$0] }) else { return nil }
            var view = view
            view.name = node.title
            return view
        }
    }

    public func saveGraphView(id: MapView.ID?, name: String, filter: GraphFilter, in space: Space.ID) async throws -> MapView {
        let entry = try entry(space)
        let name = name.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        guard (1...128).contains(name.count) else { throw SpaceStoreError.invalid("the name of a graph view is 1 to 128 characters") }
        if let id {
            guard let index = entry.views.firstIndex(where: { $0.id == id && $0.kind == .graph }) else {
                throw SpaceStoreError.invalid("no graph view has the id \(id)")
            }
            spaces[space]?.views[index].name = name
            spaces[space]?.views[index].filter = filter
            return spaces[space]!.views[index]
        }
        let view = MapView(id: ULID.make(at: now()), kind: .graph, name: name, filter: filter)
        spaces[space]?.views.append(view)
        return view
    }

    public func removeGraphView(_ id: MapView.ID, in space: Space.ID) async throws {
        _ = try entry(space)
        spaces[space]?.views.removeAll { $0.id == id && $0.kind == .graph }
        spaces[space]?.positions[id] = nil
    }

    public func setViewport(_ viewport: Viewport, of view: MapView.ID, in space: Space.ID) async throws {
        let entry = try entry(space)
        guard viewport.zoom > 0, let index = entry.views.firstIndex(where: { $0.id == view }) else {
            throw SpaceStoreError.invalid("no view has the id \(view)")
        }
        spaces[space]?.views[index].viewport = viewport
    }

    public func graph(in space: Space.ID, view: MapView.ID?, filter: GraphFilter) async throws -> GraphData {
        let entry = try entry(space)
        let nodes = entry.nodes.values.sorted { ($0.updatedAt, $0.id) > ($1.updatedAt, $1.id) }.map { node in
            let versions = entry.versions.filter { $0.nodeID == node.id }
            return GraphNodeInfo(
                id: node.id, kind: node.kind, title: node.title, tags: [],
                createdAt: node.createdAt, updatedAt: node.updatedAt,
                createdBy: versions.first?.author.name ?? person, updatedBy: versions.last?.author.name ?? person
            )
        }.filter { Self.keeps(filter, $0) }
        let shown = Set(nodes.map(\.id))
        let edges = allEdges(entry).filter { shown.contains($0.from) && shown.contains($0.to) && (filter.linkKinds?.contains($0.kind) ?? true) }
        let positions = view.flatMap { entry.positions[$0] }.map { Array($0.values).filter { shown.contains($0.nodeID) } } ?? []
        return GraphData(nodes: nodes, edges: edges, positions: positions, linkKinds: try await linkKinds(in: space))
    }

    /// Whether a node passes a filter, as the core decides.
    static func keeps(_ filter: GraphFilter, _ node: GraphNodeInfo) -> Bool {
        (filter.nodeKinds?.contains(node.kind) ?? true)
            && (filter.tags.map { !Set($0).isDisjoint(with: node.tags) } ?? true)
            && (filter.actors.map { $0.contains(node.createdBy) || $0.contains(node.updatedBy) } ?? true)
            && (filter.since.map { node.updatedAt >= $0 } ?? true)
            && (filter.until.map { node.updatedAt < $0 } ?? true)
    }

    public func setPositions(_ positions: [ViewPosition], in view: MapView.ID, space: Space.ID) async throws {
        let entry = try entry(space)
        guard let found = entry.views.first(where: { $0.id == view }) else { throw SpaceStoreError.invalid("no view has the id \(view)") }
        for position in positions {
            guard entry.nodes[position.nodeID] != nil else { throw SpaceStoreError.nodeNotFound(position.nodeID) }
            if let group = position.groupID, found.kind != .canvas || !(entry.groups[view] ?? []).contains(where: { $0.id == group }) {
                throw SpaceStoreError.invalid("no group \(group) on this canvas")
            }
        }
        for position in positions {
            spaces[space]?.positions[view, default: [:]][position.nodeID] = position
        }
    }

    public func removePositions(_ nodes: [Node.ID], from view: MapView.ID, space: Space.ID) async throws {
        _ = try entry(space)
        for node in nodes { spaces[space]?.positions[view]?[node] = nil }
    }

    public func createCanvas(title: String, in space: Space.ID) async throws -> MapView {
        _ = try entry(space)
        let date = now()
        let node = Node(id: ULID.make(at: date), title: title, body: "", createdAt: date, updatedAt: date, kind: NodeKind.canvas)
        spaces[space]?.nodes[node.id] = node
        append(version(of: node, .create, by: author), action: ChangeRecord.Action.create, in: space)
        let view = MapView(id: ULID.make(at: date), kind: .canvas, name: title, nodeID: node.id)
        spaces[space]?.views.append(view)
        return view
    }

    public func canvasBoard(of node: Node.ID, in space: Space.ID) async throws -> CanvasBoard? {
        let entry = try entry(space)
        guard var view = entry.views.first(where: { $0.kind == .canvas && $0.nodeID == node }) else { return nil }
        view.name = entry.nodes[node]?.title ?? view.name
        let items = (entry.positions[view.id] ?? [:]).values
            .compactMap { position -> CanvasItem? in
                guard let node = entry.nodes[position.nodeID] else { return nil }
                return CanvasItem(position: position, kind: node.kind, title: node.title, excerpt: NodeSummary.excerpt(of: node.body))
            }
            .sorted { ($0.position.z, $0.id) < ($1.position.z, $1.id) }
        let on = Set(items.map(\.id))
        let edges = allEdges(entry).filter { on.contains($0.from) && on.contains($0.to) }
        return CanvasBoard(view: view, items: items, groups: entry.groups[view.id] ?? [], edges: edges, linkKinds: try await linkKinds(in: space))
    }

    public func setGroup(_ group: GroupDraft, in view: MapView.ID, space: Space.ID) async throws -> ViewGroupInfo {
        let entry = try entry(space)
        guard entry.views.contains(where: { $0.id == view && $0.kind == .canvas }) else { throw SpaceStoreError.invalid("groups are only on a canvas") }
        guard group.width > 0, group.height > 0 else { throw SpaceStoreError.invalid("the size of a group must be greater than 0") }
        let id = group.id ?? ULID.make(at: now())
        let stored = ViewGroupInfo(id: id, label: group.label.trimmingCharacters(in: .whitespaces), x: group.x, y: group.y, width: group.width, height: group.height, collapsed: group.collapsed)
        var groups = entry.groups[view] ?? []
        if let index = groups.firstIndex(where: { $0.id == id }) { groups[index] = stored } else { groups.append(stored) }
        spaces[space]?.groups[view] = groups
        return stored
    }

    public func removeGroup(_ id: ViewGroupInfo.ID, in space: Space.ID) async throws {
        let entry = try entry(space)
        for (view, groups) in entry.groups where groups.contains(where: { $0.id == id }) {
            spaces[space]?.groups[view]?.removeAll { $0.id == id }
            for (node, position) in entry.positions[view] ?? [:] where position.groupID == id {
                spaces[space]?.positions[view]?[node]?.groupID = nil
            }
        }
    }

    /// The links made by hand and the `mentions` links read from `[[links]]` in the bodies.
    private func allEdges(_ entry: Entry) -> [GraphEdgeInfo] {
        var byTitle: [String: Node.ID] = [:]
        for node in entry.nodes.values.sorted(by: { $0.createdAt < $1.createdAt }) where byTitle[node.title.lowercased()] == nil {
            byTitle[node.title.lowercased()] = node.id
        }
        var edges: [GraphEdgeInfo] = []
        for node in entry.nodes.values {
            var seen = Set<Node.ID>()
            for title in Self.wikiLinks(in: node.body) {
                if let target = byTitle[title.lowercased()], seen.insert(target).inserted {
                    edges.append(GraphEdgeInfo(from: node.id, to: target, kind: LinkKindInfo.mentions))
                }
            }
        }
        return edges.sorted { ($0.from, $0.to) < ($1.from, $1.to) } + entry.links
    }

    /// The titles of the `[[links]]` in a Markdown body.
    static func wikiLinks(in body: String) -> [String] {
        var titles: [String] = []
        var rest = body[...]
        while let open = rest.range(of: "[["), let close = rest[open.upperBound...].range(of: "]]") {
            let inner = rest[open.upperBound..<close.lowerBound]
            let title = inner.split(separator: "|", maxSplits: 1).first.map(String.init) ?? ""
            if !title.isEmpty, !title.contains("\n") { titles.append(title.trimmingCharacters(in: .whitespaces)) }
            rest = rest[close.upperBound...]
        }
        return titles
    }
}

// MARK: - Example nodes

extension PreviewSpaceStore {
    /// The example nodes, clearly marked as examples, the newest dated `end`.
    static func examples(endingAt end: Date) -> [Node] {
        let samples: [(String, String)] = [
            (
                "Example: Welcome to Knode",
                """
                # Welcome to Knode

                > This is an **example note**. Nothing in the example space is saved: open or create a space to keep your notes.

                Knode is one memory that every AI agent shares through MCP, and this app is where you see it and organise it.

                ## Try it

                - Edit this note on the left and watch the preview on the right.
                - Search from the toolbar.
                - Link notes with `[[double brackets]]`, like [[Example: How links work]].
                """
            ),
            (
                "Example: How links work",
                """
                # How links work

                > Example note.

                You write `[[Title]]`. Knode stores the id of the note it points to, so renaming a note never breaks a link.

                See [[Example: Welcome to Knode]].
                """
            ),
            (
                "Example: Claude Code session, knode repo",
                """
                # Claude Code session

                > Example of a captured conversation.

                Decided to keep metadata in the database, never in frontmatter.

                1. Markdown is read when a node is written.
                2. Search never parses text.

                ```sh
                knode search "frontmatter"
                ```
                """
            ),
            (
                "Example: Reading list",
                """
                # Reading list

                > Example note.

                - [x] CommonMark spec
                - [ ] SQLite FTS5
                - [ ] Loro, a CRDT for text
                """
            ),
        ]
        return samples.enumerated().map { index, sample in
            let date = end.addingTimeInterval(-Double(index) * 3600 * 7)
            return Node(id: ULID.make(at: date, seed: UInt64(index)), title: sample.0, body: sample.1, createdAt: date, updatedAt: date)
        }
    }
}

// MARK: - ULID

/// Time-ordered ids, like the ones the core gives every node.
enum ULID {
    private static let alphabet = Array("0123456789ABCDEFGHJKMNPQRSTVWXYZ")

    static func make(at date: Date, seed: UInt64? = nil) -> String {
        var millis = UInt64(max(0, date.timeIntervalSince1970 * 1000))
        var time = [Character](repeating: "0", count: 10)
        for index in stride(from: 9, through: 0, by: -1) {
            time[index] = alphabet[Int(millis & 31)]
            millis >>= 5
        }
        var random = seed ?? UInt64.random(in: .min ... .max)
        var tail = [Character](repeating: "0", count: 16)
        for index in stride(from: 15, through: 0, by: -1) {
            tail[index] = alphabet[Int(random & 31)]
            random >>= 5
        }
        return String(time + tail)
    }
}
