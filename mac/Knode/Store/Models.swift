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
    /// The `space.icon` setting: one emoji, or `nil` for the default icon.
    public var icon: String?

    public init(id: ID, name: String, url: URL? = nil, icon: String? = nil) {
        self.id = id
        self.name = name
        self.url = url
        self.icon = icon
    }
}

/// The icon of a space: one emoji, kept in the `space.icon` setting of the space.
public enum SpaceIcon {
    /// The setting the icon is kept in.
    static let settingKey = "space.icon"

    /// Icons offered when choosing one.
    static let suggestions = ["🌱", "💼", "🏠", "📚", "🧠", "🔬", "🎨", "🎵", "✈️", "🧪", "📝", "⭐️"]

    /// The text as an icon: trimmed, and `nil` when it is empty.
    /// - Throws: `SpaceStoreError.invalid` when it is not exactly one emoji.
    static func normalized(_ text: String?) throws -> String? {
        guard let text = text?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else { return nil }
        guard isEmoji(text) else { throw SpaceStoreError.invalid("an icon is one emoji, such as 🌱") }
        return text
    }

    /// True when the text is exactly one emoji.
    static func isEmoji(_ text: String) -> Bool {
        guard text.count == 1, let character = text.first else { return false }
        let scalars = character.unicodeScalars
        guard let first = scalars.first else { return false }
        // Digits and `#` are emoji only with the emoji presentation selector or a keycap.
        return first.properties.isEmojiPresentation
            || (first.properties.isEmoji && scalars.count > 1)
    }
}

/// Who made a change: a person, an agent or Knode itself.
public struct Author: Hashable, Sendable {
    public enum Kind: String, Sendable {
        case person, agent, system
    }

    public var kind: Kind
    /// The name of the person or the agent, such as `claude-code`.
    public var name: String
    /// The conversation or session the change came from.
    public var session: String?
    /// Where the content came from: a file, a URL, a project folder.
    public var source: String?

    public init(kind: Kind, name: String, session: String? = nil, source: String? = nil) {
        self.kind = kind
        self.name = name
        self.session = session
        self.source = source
    }
}

/// One version of a note: the note right after a change (for a delete, right before it), and who
/// made the change and when.
public struct NoteVersion: Identifiable, Hashable, Sendable {
    public enum Operation: String, Sendable {
        case create, update, delete
    }

    /// The order of the version in the space; it only grows.
    public var seq: Int64
    public var id: Int64 { seq }
    public var nodeID: Node.ID
    public var operation: Operation
    /// The change the version belongs to, which `SpaceStore.undo` takes; `nil` only in spaces
    /// written before change history, which cannot be undone.
    public var changeID: Int64?
    /// When the version puts back an earlier one (an undo or a restore), the `seq` of that one.
    public var restoredFrom: Int64?
    public var changedAt: Date
    public var author: Author
    public var title: String
    public var body: String

    public init(seq: Int64, nodeID: Node.ID, operation: Operation, changeID: Int64?, restoredFrom: Int64? = nil, changedAt: Date, author: Author, title: String, body: String) {
        self.seq = seq
        self.nodeID = nodeID
        self.operation = operation
        self.changeID = changeID
        self.restoredFrom = restoredFrom
        self.changedAt = changedAt
        self.author = author
        self.title = title
        self.body = body
    }
}

/// One change to a space: one write a person or an agent asked for. An undo is a change too, and
/// undoing an undo redoes the change it undid.
public struct ChangeRecord: Identifiable, Hashable, Sendable {
    /// What a change did, as the core names it (`node.update`, `undo`, ...).
    public enum Action {
        public static let create = "node.create"
        public static let update = "node.update"
        public static let delete = "node.delete"
        public static let restore = "node.restore"
        public static let undo = "undo"
    }

    /// The order of the change in the space; it only grows. `SpaceStore.undo` takes it.
    public var id: Int64
    public var changedAt: Date
    public var author: Author
    public var action: String
    /// What it was done to: a node id, a setting key, or empty.
    public var subject: String
    /// For an undo, the change it undoes.
    public var undoes: Int64?

    public init(id: Int64, changedAt: Date, author: Author, action: String, subject: String, undoes: Int64? = nil) {
        self.id = id
        self.changedAt = changedAt
        self.author = author
        self.action = action
        self.subject = subject
        self.undoes = undoes
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
    /// What the node is, such as `note`, `conversation` or `canvas`.
    public var kind: String

    public init(id: ID, title: String, body: String, createdAt: Date, updatedAt: Date, kind: String = NodeKind.note) {
        self.id = id
        self.title = title
        self.body = body
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.kind = kind
    }

    public var summary: NodeSummary {
        NodeSummary(id: id, title: title, excerpt: NodeSummary.excerpt(of: body), updatedAt: updatedAt, kind: kind)
    }
}

/// What a list or a search result shows about a node, without its whole body.
public struct NodeSummary: Identifiable, Hashable, Sendable {
    public var id: Node.ID
    public var title: String
    public var excerpt: String
    public var updatedAt: Date
    /// What the node is, such as `note` or `canvas`.
    public var kind: String

    public init(id: Node.ID, title: String, excerpt: String, updatedAt: Date, kind: String = NodeKind.note) {
        self.id = id
        self.title = title
        self.excerpt = excerpt
        self.updatedAt = updatedAt
        self.kind = kind
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
    /// Undoing the change would overwrite later changes, listed oldest first; it can be forced.
    case undoConflict(change: Int64, later: [Int64], message: String)
    /// The change cannot be undone, or was already undone.
    case cannotUndo(String)
    /// Only a person may do this, or an agent may not do it in this space.
    case personOnly(String)
    /// The Knode settings (`config.toml`) could not be read or written.
    case config(String)

    public var errorDescription: String? {
        switch self {
        case .spaceNotFound: "This space is not open."
        case .nodeNotFound: "This note no longer exists."
        case .notASpace(let url): "\(url.lastPathComponent) is not a Knode space."
        case .alreadyExists(let url): "\(url.lastPathComponent) already exists."
        case .unsupported(let message): "This space can't be opened: \(message)."
        case .invalid(let message): message.prefix(1).uppercased() + message.dropFirst() + "."
        case .storage(let message): "The space could not be read or saved: \(message)."
        case .undoConflict(_, let later, _):
            later.count == 1
                ? "Someone changed this note after that change. Undoing it would also undo their later change."
                : "This note changed \(later.count) times after that change. Undoing it would also undo those later changes."
        case .cannotUndo(let message), .personOnly(let message), .config(let message):
            message.prefix(1).uppercased() + message.dropFirst() + "."
        }
    }
}
