import Foundation

extension ServiceSpaceStore: DeletedNotesStore {
    public func deletedNotes(in space: Space.ID) async throws -> [DeletedNote] {
        let path = try path(space)
        return try await call("markdown.deleted.list", ["space": .string(path)], as: [WireDeletedNote].self)
            .map { DeletedNote(id: $0.id, title: $0.title, path: $0.path) }
    }

    public func keepDeletedNote(_ id: Node.ID, in space: Space.ID) async throws {
        let path = try path(space)
        _ = try await call("markdown.deleted.keep", ["space": .string(path), "id": .string(id)], as: Bool.self, node: id)
    }

    public func deleteNode(_ id: Node.ID, in space: Space.ID) async throws {
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "id": .string(id), "by": author]
        _ = try await call("node.delete", params, as: WireNothing?.self, node: id)
    }
}

struct WireDeletedNote: Decodable, Sendable {
    var id: String
    var title: String
    var path: String
}
