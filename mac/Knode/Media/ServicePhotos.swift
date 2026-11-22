import Foundation

// Photos through Knode Service (`PROTOCOL.md`, "Photos"). Adding and retrying are the person's;
// readings and failures are written as Knode itself, with the on-device engine as `source`.
extension ServiceSpaceStore: PhotoStore {
    func createPhoto(_ photo: NewPhotoDraft, in space: Space.ID) async throws -> StoredPhoto {
        let path = try path(space)
        var params: [String: JSONValue] = [
            "space": .string(path), "title": .string(photo.title),
            "path": .string(photo.url.standardizedFileURL.path(percentEncoded: false)),
            "metadata": PhotoWire.metadata(photo.metadata), "by": author,
        ]
        if let project = photo.project { params["project"] = .string(project) }
        return try await call("photos.create", params, as: WirePhoto.self).photo()
    }

    func pendingPhotos(in space: Space.ID) async throws -> [StoredPhoto] {
        let path = try path(space)
        return try await call("photos.pending", ["space": .string(path)], as: [WirePhoto].self).map { try $0.photo() }
    }

    func failedPhotos(in space: Space.ID) async throws -> [StoredPhoto] {
        let path = try path(space)
        return try await call("photos.failed", ["space": .string(path)], as: [WirePhoto].self).map { try $0.photo() }
    }

    func photo(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        let path = try path(space)
        return try await call("photos.get", ["space": .string(path), "id": .string(id)], as: WirePhoto.self, node: id).photo()
    }

    func setPhotoReading(_ reading: PhotoReadingDraft, of id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        let path = try path(space)
        let params: [String: JSONValue] = [
            "space": .string(path), "id": .string(id), "reading": PhotoWire.reading(reading), "by": PhotoWire.provenance,
        ]
        return try await call("photos.reading", params, as: WirePhoto.self, node: id).photo()
    }

    func failPhoto(_ id: Node.ID, error: String, in space: Space.ID) async throws -> StoredPhoto {
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "id": .string(id), "error": .string(error), "by": PhotoWire.provenance]
        return try await call("photos.fail", params, as: WirePhoto.self, node: id).photo()
    }

    func retryPhoto(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        let path = try path(space)
        return try await call("photos.retry", ["space": .string(path), "id": .string(id), "by": author], as: WirePhoto.self, node: id).photo()
    }
}
