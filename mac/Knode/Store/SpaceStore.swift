import Foundation

/// Everything the app needs from a space.
///
/// `CoreSpaceStore` implements it with the Rust core (`core/crates/knode-ffi`), and
/// `PreviewSpaceStore` with example notes in memory. Every method is async so a store never blocks
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
}
