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
