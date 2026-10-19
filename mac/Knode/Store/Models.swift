import Foundation

/// A Knode space: one `.knode` package on disk.
public struct Space: Identifiable, Hashable, Sendable {
    public typealias ID = String

    /// The space id from `manifest.json`.
    public var id: ID
    /// The space name from `manifest.json`.
    public var name: String
    /// Where the `.knode` package lives, if it lives on disk.
    public var url: URL?

    public init(id: ID, name: String, url: URL? = nil) {
        self.id = id
        self.name = name
        self.url = url
    }
}

/// A node: one note in a space. Its text is Markdown (decision 0006).
public struct Node: Identifiable, Hashable, Sendable {
    public typealias ID = String

    /// Time-ordered id (ULID).
    public var id: ID
    public var title: String
    /// CommonMark with GitHub Flavored Markdown and `[[links]]`.
    public var body: String
    public var createdAt: Date
    public var updatedAt: Date

    public init(id: ID, title: String, body: String, createdAt: Date, updatedAt: Date) {
        self.id = id
        self.title = title
        self.body = body
        self.createdAt = createdAt
        self.updatedAt = updatedAt
    }

    public var summary: NodeSummary {
        NodeSummary(id: id, title: title, excerpt: NodeSummary.excerpt(of: body), updatedAt: updatedAt)
    }
}

/// What a list or a search result shows about a node, without its whole body.
public struct NodeSummary: Identifiable, Hashable, Sendable {
    public var id: Node.ID
    public var title: String
    public var excerpt: String
    public var updatedAt: Date

    public init(id: Node.ID, title: String, excerpt: String, updatedAt: Date) {
        self.id = id
        self.title = title
        self.excerpt = excerpt
        self.updatedAt = updatedAt
    }

    /// The first line of plain text in a Markdown body, without heading or list markers.
    static func excerpt(of body: String, limit: Int = 140) -> String {
        for raw in body.split(separator: "\n", omittingEmptySubsequences: true) {
            var line = raw.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("```") { continue }
            while let first = line.first, "#>-*".contains(first) {
                line.removeFirst()
            }
            line = line.trimmingCharacters(in: .whitespaces)
            if !line.isEmpty { return String(line.prefix(limit)) }
        }
        return ""
    }
}

/// Errors any `SpaceStore` can report.
public enum SpaceStoreError: Error, Equatable, LocalizedError {
    case spaceNotFound(Space.ID)
    case nodeNotFound(Node.ID)
    case notASpace(URL)
    case alreadyExists(URL)
    /// The space was written by a newer version of Knode, or its files do not agree.
    case unsupported(String)
    /// A value broke a rule of the format, such as an empty title.
    case invalid(String)
    /// A file or the database could not be read or written.
    case storage(String)

    public var errorDescription: String? {
        switch self {
        case .spaceNotFound: "This space is not open."
        case .nodeNotFound: "This note no longer exists."
        case .notASpace(let url): "\(url.lastPathComponent) is not a Knode space."
        case .alreadyExists(let url): "\(url.lastPathComponent) already exists."
        case .unsupported(let message): "This space can't be opened: \(message)."
        case .invalid(let message): message.prefix(1).uppercased() + message.dropFirst() + "."
        case .storage(let message): "The space could not be read or saved: \(message)."
        }
    }
}
