import Foundation
import KnodeCore

// Photos with the core in the app (core/crates/knode-ffi), the same rules as `photos.*`.
extension CoreSpaceStore: PhotoStore {
    func createPhoto(_ photo: NewPhotoDraft, in space: Space.ID) async throws -> StoredPhoto {
        let core = try self.core(space)
        let author = author
        let new = KnodeCore.NewPhoto(
            title: photo.title, path: photo.url.standardizedFileURL.path(percentEncoded: false),
            project: photo.project, metadata: KnodeCore.PhotoMetadata(photo.metadata)
        )
        return StoredPhoto(try await run { try core.createPhoto(photo: new, by: author) })
    }

    func pendingPhotos(in space: Space.ID) async throws -> [StoredPhoto] {
        let core = try self.core(space)
        return try await run { try core.pendingPhotos() }.map(StoredPhoto.init)
    }

    func failedPhotos(in space: Space.ID) async throws -> [StoredPhoto] {
        let core = try self.core(space)
        return try await run { try core.failedPhotos() }.map(StoredPhoto.init)
    }

    func photo(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        let core = try self.core(space)
        return StoredPhoto(try await run { try core.photo(id: id) })
    }

    func setPhotoReading(_ reading: PhotoReadingDraft, of id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        let core = try self.core(space)
        let value = KnodeCore.PhotoReading(
            engine: PhotoEngine.name, text: reading.text, labels: reading.labels,
            description: reading.description, metadata: KnodeCore.PhotoMetadata(reading.metadata)
        )
        let by = Self.photoProvenance
        return StoredPhoto(try await run { try core.setPhotoReading(id: id, reading: value, by: by) })
    }

    func failPhoto(_ id: Node.ID, error: String, in space: Space.ID) async throws -> StoredPhoto {
        let core = try self.core(space)
        let by = Self.photoProvenance
        return StoredPhoto(try await run { try core.failPhoto(id: id, error: error, by: by) })
    }

    func retryPhoto(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        let core = try self.core(space)
        let author = author
        return StoredPhoto(try await run { try core.retryPhoto(id: id, by: author) })
    }

    static var photoProvenance: Provenance {
        Provenance(kind: .system, actor: PhotoEngine.actor, session: nil, source: PhotoEngine.source)
    }
}

extension KnodeCore.PhotoMetadata {
    init(_ m: PhotoMetadataDraft) {
        self.init(
            takenAt: m.takenAt.map(RecordingWire.milliseconds), latitude: m.latitude, longitude: m.longitude,
            place: m.place, camera: m.camera, width: m.width.map { UInt32(clamping: $0) }, height: m.height.map { UInt32(clamping: $0) }
        )
    }
}

extension StoredPhoto {
    init(_ photo: KnodeCore.Photo) {
        let reading: Reading = switch photo.reading {
        case .pending: .pending
        case .done: .done
        case .failed: .failed
        }
        let m = photo.metadata
        self.init(
            id: photo.id, title: photo.title, file: photo.file,
            metadata: PhotoMetadataDraft(
                takenAt: m.takenAt.map(Date.init(unixMilliseconds:)), latitude: m.latitude, longitude: m.longitude,
                place: m.place, camera: m.camera, width: m.width.map(Int.init), height: m.height.map(Int.init)
            ),
            reading: reading, error: photo.error, project: photo.project
        )
    }
}
