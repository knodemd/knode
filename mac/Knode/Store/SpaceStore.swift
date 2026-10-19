import Foundation

/// Everything the app needs from a space.
///
/// `ServiceSpaceStore` implements it through Knode Service, `CoreSpaceStore` with the Rust core
/// (`core/crates/knode-ffi`) in the app, `RoutingSpaceStore` with whichever of the two is available,
/// and `PreviewSpaceStore` with example notes in memory. Every method is async so a store never blocks
/// the main thread.
public protocol SpaceStore: Sendable {
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

    public init(space: Space.ID, nodeID: Node.ID?, kind: Kind) {
        self.space = space
        self.nodeID = nodeID
        self.kind = kind
    }
}
