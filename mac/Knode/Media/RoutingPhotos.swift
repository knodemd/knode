import Foundation

// Through Knode Service while it holds the space, through the core in the app when it does not.
// A create of the same file returns the same photo, and the other writes replace what they wrote,
// so every write is safe to run again.
extension RoutingSpaceStore: PhotosBackend {
    func createPhoto(_ photo: NewPhotoDraft, in space: Space.ID) async throws -> StoredPhoto {
        try await photos(space) { store in try await store.createPhoto(photo, in: space) }
    }

    func pendingPhotos(in space: Space.ID) async throws -> [StoredPhoto] {
        try await photos(space) { store in try await store.pendingPhotos(in: space) }
    }

    func failedPhotos(in space: Space.ID) async throws -> [StoredPhoto] {
        try await photos(space) { store in try await store.failedPhotos(in: space) }
    }

    func photo(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        try await photos(space) { store in try await store.photo(id, in: space) }
    }

    func setPhotoReading(_ reading: PhotoReadingDraft, of id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        try await photos(space) { store in try await store.setPhotoReading(reading, of: id, in: space) }
    }

    func failPhoto(_ id: Node.ID, error: String, in space: Space.ID) async throws -> StoredPhoto {
        try await photos(space) { store in try await store.failPhoto(id, error: error, in: space) }
    }

    func retryPhoto(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        try await photos(space) { store in try await store.retryPhoto(id, in: space) }
    }

    private func photos<T: Sendable>(_ space: Space.ID, _ work: (any PhotoStore) async throws -> T) async throws -> T {
        try await route(space) { store in
            guard let store = store as? any PhotoStore else {
                throw SpaceStoreError.unsupported("this space keeps no photos")
            }
            return try await work(store)
        }
    }
}
