import Foundation

/// The notes whose file was deleted from the live Markdown folder of a space.
///
/// Knode never deletes a note because its file is gone: Knode Service marks it with the property
/// `markdown.deleted`, stops writing it to the folder, and waits for the person to decide.
/// `ServiceSpaceStore` implements it through Knode Service, `CoreSpaceStore` with the core in the
/// app, and `RoutingSpaceStore` with whichever of the two holds the space.
public protocol DeletedNotesStore: Sendable {
    /// The notes waiting for the person, newest first.
    func deletedNotes(in space: Space.ID) async throws -> [DeletedNote]

    /// Keeps a note: its mark is removed, and its file is written to the folder again.
    func keepDeletedNote(_ id: Node.ID, in space: Space.ID) async throws

    /// Deletes a note as the person. Its history keeps it, so the delete can be undone.
    func deleteNode(_ id: Node.ID, in space: Space.ID) async throws
}

/// A note whose file was deleted from the live Markdown folder.
public struct DeletedNote: Identifiable, Hashable, Sendable {
    public var id: Node.ID
    public var title: String
    /// The path the file had in the folder.
    public var path: String

    public init(id: Node.ID, title: String, path: String) {
        self.id = id
        self.title = title
        self.path = path
    }
}
