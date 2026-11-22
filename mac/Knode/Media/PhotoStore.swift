import Foundation

/// Photos kept in a space, with what this Mac read in them (`PROTOCOL.md`, "Photos"). The space
/// says what is still to read; the app reads each photo on the device with Vision and stores what
/// it found.
///
/// `ServiceSpaceStore` implements it through Knode Service, `CoreSpaceStore` with the core in the
/// app, and `RoutingSpaceStore` with whichever of the two holds the space. Adding and retrying are
/// the person's; readings and failures are written by Knode itself, as `PhotoEngine.vision`.
protocol PhotoStore: Sendable {
    /// Adds a photo: its node, its file copied in unchanged and a link to its project. The same
    /// file the space already holds returns that photo.
    func createPhoto(_ photo: NewPhotoDraft, in space: Space.ID) async throws -> StoredPhoto
    /// The photos still to read, oldest first.
    func pendingPhotos(in space: Space.ID) async throws -> [StoredPhoto]
    /// The photos whose reading failed, newest first.
    func failedPhotos(in space: Space.ID) async throws -> [StoredPhoto]
    func photo(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto
    /// Writes what was read, replacing what was read before.
    func setPhotoReading(_ reading: PhotoReadingDraft, of id: Node.ID, in space: Space.ID) async throws -> StoredPhoto
    /// Marks the reading failed, with why.
    func failPhoto(_ id: Node.ID, error: String, in space: Space.ID) async throws -> StoredPhoto
    /// Puts the reading back to pending, as the person.
    func retryPhoto(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto
}

/// What the photos pipeline needs from the app's store.
protocol PhotosBackend: PhotoStore {
    func openSpace(at url: URL) async throws -> Space
    func attachmentURL(named name: String, of node: Node.ID, in space: Space.ID) async throws -> URL?
    func changes() -> AsyncStream<SpaceChange>
}

/// Who writes a photo's reading: Knode itself, as the on-device engine. Its provenance is
/// `{ "kind": "system", "actor": "photos", "source": "apple-vision" }`.
enum PhotoEngine {
    static let actor = "photos"
    static let source = "apple-vision"
    static let name = "Apple Vision"
}

/// What a photo's file says about it. Every part is optional.
struct PhotoMetadataDraft: Hashable, Sendable {
    var takenAt: Date?
    var latitude: Double?
    var longitude: Double?
    /// Only when this Mac named the place without the network.
    var place: String?
    var camera: String?
    var width: Int?
    var height: Int?
}

/// A photo to add.
struct NewPhotoDraft: Hashable, Sendable {
    /// Empty for the file's name without its extension.
    var title: String
    var url: URL
    var project: Node.ID?
    var metadata: PhotoMetadataDraft
}

/// What this Mac read in a photo.
struct PhotoReadingDraft: Hashable, Sendable {
    var text: String
    var labels: [String]
    var description: String?
    var metadata: PhotoMetadataDraft
}

/// A photo as the space keeps it.
struct StoredPhoto: Identifiable, Hashable, Sendable {
    enum Reading: String, Hashable, Sendable { case pending, done, failed }

    var id: Node.ID
    var title: String
    /// The name of the attached original.
    var file: String
    var metadata: PhotoMetadataDraft
    var reading: Reading
    var error: String?
    var project: Node.ID?
}

/// The JSON of the `photos.*` params.
enum PhotoWire {
    static var provenance: JSONValue {
        ["kind": "system", "actor": .string(PhotoEngine.actor), "source": .string(PhotoEngine.source)]
    }

    static func metadata(_ m: PhotoMetadataDraft) -> JSONValue {
        var object: [String: JSONValue] = [:]
        if let takenAt = m.takenAt { object["taken_at"] = .int(RecordingWire.milliseconds(takenAt)) }
        if let latitude = m.latitude { object["latitude"] = .double(latitude) }
        if let longitude = m.longitude { object["longitude"] = .double(longitude) }
        if let place = m.place { object["place"] = .string(place) }
        if let camera = m.camera { object["camera"] = .string(camera) }
        if let width = m.width { object["width"] = .int(Int64(width)) }
        if let height = m.height { object["height"] = .int(Int64(height)) }
        return .object(object)
    }

    static func reading(_ r: PhotoReadingDraft) -> JSONValue {
        var object: [String: JSONValue] = [
            "engine": .string(PhotoEngine.name), "text": .string(r.text),
            "labels": .array(r.labels.map(JSONValue.string)), "metadata": metadata(r.metadata),
        ]
        if let description = r.description { object["description"] = .string(description) }
        return .object(object)
    }
}

/// A photo as Knode Service answers it.
struct WirePhoto: Decodable, Sendable {
    struct Metadata: Decodable, Sendable {
        var takenAt: Int64?
        var latitude: Double?
        var longitude: Double?
        var place: String?
        var camera: String?
        var width: Int?
        var height: Int?
    }

    var id: String
    var title: String
    var file: String
    var metadata: Metadata
    var reading: String
    var error: String?
    var project: String?

    func photo() throws -> StoredPhoto {
        guard let reading = StoredPhoto.Reading(rawValue: reading) else { throw ServiceError.badResponse("a photo with an unknown reading") }
        let m = metadata
        return StoredPhoto(
            id: id, title: title, file: file,
            metadata: PhotoMetadataDraft(
                takenAt: m.takenAt.map(Date.init(unixMilliseconds:)), latitude: m.latitude, longitude: m.longitude,
                place: m.place, camera: m.camera, width: m.width, height: m.height
            ),
            reading: reading, error: error, project: project
        )
    }
}
