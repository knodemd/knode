import Foundation

/// An in-memory `SpaceStore` with example notes, so the app runs before the Rust core reaches Swift.
///
/// It never reads or writes the `.knode` package: opening one shows example nodes, and every change
/// stays in memory until the app quits.
public actor PreviewSpaceStore: SpaceStore {
    private struct Entry {
        var space: Space
        var nodes: [Node.ID: Node]
    }

    private var spaces: [Space.ID: Entry] = [:]
    private let now: @Sendable () -> Date
    private let seedExamples: Bool

    /// - Parameters:
    ///   - seedExamples: fill every opened space with the example nodes.
    ///   - now: the clock, replaceable in tests.
    public init(seedExamples: Bool = true, now: @escaping @Sendable () -> Date = { Date() }) {
        self.seedExamples = seedExamples
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
        spaces[space.id] = Entry(space: space, nodes: Dictionary(uniqueKeysWithValues: nodes.map { ($0.id, $0) }))
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
        return node
    }

    public func updateNode(_ id: Node.ID, in space: Space.ID, title: String?, body: String?) async throws -> Node {
        var node = try await self.node(id, in: space)
        if let title { node.title = title }
        if let body { node.body = body }
        node.updatedAt = now()
        spaces[space]?.nodes[id] = node
        return node
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

                > This is an **example note**. The real notes of your space appear here once the Knode core is connected.

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
