import Foundation

/// A note's body opened for editing together: its text and the version of its document.
public struct BodyDocument: Hashable, Sendable {
    public var text: String
    /// The version of the body's document, passed back with the next edit.
    public var version: String

    public init(text: String, version: String) {
        self.text = text
        self.version = version
    }
}

/// What an edit of a body did (`doc.edit`).
public struct BodyEditResult: Hashable, Sendable {
    /// The version of the body's document now.
    public var version: String
    /// The version of the text the editor has now: what it had, with its edit, without what
    /// others wrote since. The next edit is based on it.
    public var seen: String
    /// The body now, when others wrote since the edit's base (`version` differs from `seen`).
    public var text: String?

    public init(version: String, seen: String, text: String?) {
        self.version = version
        self.seen = seen
        self.text = text
    }
}

/// A store that can merge edits of a body with what others write at the same time, through
/// Knode Service (section 21 of the .knode specification). The editor sends what the person types
/// as splices based on the version it has, instead of the whole text.
public protocol CoeditingStore: SpaceStore {
    /// Opens a note's body for editing together.
    func openBody(_ node: Node.ID, in space: Space.ID) async throws -> BodyDocument

    /// Applies splices, one after another, to the text at `version`, merged with what others
    /// wrote since. Empty splices only catch up.
    func editBody(_ node: Node.ID, in space: Space.ID, version: String, splices: [TextSplice]) async throws -> BodyEditResult
}

extension SpaceStoreError {
    /// The space is not shared through Knode Service now: bodies are written whole.
    static let notCoediting = SpaceStoreError.unsupported("Editing together needs Knode Service")
}
