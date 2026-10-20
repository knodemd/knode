import Foundation

/// Everything the app needs from a space.
///
/// `ServiceSpaceStore` implements it through Knode Service, `CoreSpaceStore` with the Rust core
/// (`core/crates/knode-ffi`) in the app, `RoutingSpaceStore` with whichever of the two is available,
/// and `PreviewSpaceStore` with example notes in memory. Every method is async so a store never blocks
/// the main thread.
public protocol SpaceStore: Sendable {
    /// Who the store writes as: the name of the person in the provenance of every change.
    var person: String { get }

    /// Creates a new, empty `.knode` package at `url` and opens it.
    func createSpace(at url: URL, name: String) async throws -> Space

    /// Opens the `.knode` package at `url`.
    func openSpace(at url: URL) async throws -> Space

    /// Every node in the space, newest first (by last update).
    func listNodes(in space: Space.ID) async throws -> [NodeSummary]

    /// One node with its whole Markdown body.
    func node(_ id: Node.ID, in space: Space.ID) async throws -> Node

    /// Creates a node and returns it.
    func createNode(in space: Space.ID, title: String, body: String) async throws -> Node

    /// Changes the title, the body or both; `nil` leaves a field as it is.
    func updateNode(_ id: Node.ID, in space: Space.ID, title: String?, body: String?) async throws -> Node

    /// Full-text search over titles and bodies, best match first.
    func search(_ query: String, in space: Space.ID) async throws -> [NodeSummary]

    /// Renames the space: its name in `manifest.json`. The package keeps its file name.
    func renameSpace(_ space: Space.ID, to name: String) async throws -> Space

    /// The icon of the space (`space.icon`), or `nil` when it has none.
    func icon(of space: Space.ID) async throws -> String?

    /// Sets the icon of the space, one emoji, or removes it with `nil`.
    func setIcon(_ icon: String?, of space: Space.ID) async throws

    /// Stops using the space. Nothing on disk changes.
    func closeSpace(_ space: Space.ID) async

    /// Every version of a node, newest first, with who changed it and when.
    func history(of node: Node.ID, in space: Space.ID) async throws -> [NoteVersion]

    /// Puts a node back to one of its versions (`NoteVersion.seq`) and returns it.
    func restore(_ node: Node.ID, to version: Int64, in space: Space.ID) async throws -> Node

    /// The changes made to a node, newest first, at most `limit`: its edits, restores and the undos
    /// of them, with who made each.
    func changes(of node: Node.ID, in space: Space.ID, limit: Int) async throws -> [ChangeRecord]

    /// Undoes a change (`NoteVersion.changeID`). Fails with `SpaceStoreError.undoConflict` when
    /// later changes touched the same notes, unless `force` is set: ask the person first.
    func undo(change: Int64, in space: Space.ID, force: Bool) async throws

    /// Changes made to open spaces by someone else, such as an agent writing through Knode
    /// Service, as they happen. A store that sees no one else's changes returns a stream that ends
    /// at once.
    func changes() -> AsyncStream<SpaceChange>
}

extension SpaceStore {
    public func changes() -> AsyncStream<SpaceChange> {
        AsyncStream { $0.finish() }
    }
}

/// A change to a space, made by the app or by anyone else.
public struct SpaceChange: Sendable, Hashable {
    public enum Kind: String, Sendable {
        case create, update, delete, link, unlink
        /// Changes may have been missed: reload everything shown from the space.
        case reload
    }

    public var space: Space.ID
    /// The node that changed; `nil` for `reload`.
    public var nodeID: Node.ID?
    public var kind: Kind
    /// For a body written through its document by Knode Service: the version of the document
    /// now. An open editor that has another version catches up.
    public var version: String?
    /// Who made the change, when the store knows.
    public var author: Author?

    public init(space: Space.ID, nodeID: Node.ID?, kind: Kind, version: String? = nil, author: Author? = nil) {
        self.space = space
        self.nodeID = nodeID
        self.kind = kind
        self.version = version
        self.author = author
    }
}
