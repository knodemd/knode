import Foundation

/// What a space wants from the Photos library now: `photos_library.plan`.
struct PhotosLibraryPlan: Hashable, Sendable, Decodable {
    struct Album: Hashable, Sendable, Decodable {
        var id: String
        var name: String
    }

    /// Whether the source is on there; when off, nothing is read for it.
    var on: Bool
    /// The id of this Mac's library in Knode: albums are picked, and nodes found and marked, per
    /// library, as identifiers mean something only in one library.
    var library: String
    /// The albums picked on this Mac: only these are read.
    var albums: [Album]
    /// Whether albums are picked only on other Macs: this one reads nothing yet.
    var elsewhere: Bool
    /// With the history From today, older assets are left out (Unix milliseconds).
    var since: Int64?
    /// Whether originals kept only in iCloud are downloaded.
    var icloud: Bool
    /// Whether the space is under its size limit; when not, nothing new is added.
    var room: Bool
    /// The change token saved by the last run.
    var token: String?
    var nextRun: Int64?
}

/// What `photos_library.add` answers.
struct PhotosLibraryAdded: Sendable, Decodable {
    struct Node: Sendable, Decodable {
        var type: String
        var id: String
    }

    var node: Node
    var created: Bool
    var room: Bool
}

/// An asset exported by the app, for `photos_library.add`.
struct PhotosLibraryExport: Hashable, Sendable {
    var asset: PhotoAsset
    /// The original, or the audio taken from a video too large to attach.
    var file: URL
    var metadata: PhotoMetadataDraft
    /// The name of the original, as exported.
    var originalName: String
    /// A video's original's name when only its audio is in `file`; the original stays in the
    /// library.
    var original: String?
    /// The hash of the original (`sha256:<hex>`), computed while exporting it: how the core finds
    /// a video again after the library is rebuilt.
    var content: String?
}

/// An asset that can never be added, for `photos_library.sync`: it is not exported again.
struct PhotosLibraryFailure: Hashable, Sendable {
    var id: String
    var reason: String
}

/// The Photos library options of a space: `photos_library.options`.
struct PhotosLibraryOptions: Hashable, Sendable, Decodable {
    /// Whether originals kept only in iCloud are downloaded.
    var icloud: Bool
    /// Whether albums are picked only on other Macs.
    var elsewhere: Bool
}

/// The Photos library source in Knode Service (`PROTOCOL.md`, "Photos library"): the app reads
/// PhotoKit and hands what it exported to the service, which decides what to write.
protocol PhotosLibraryClient: Sendable {
    /// The spaces the source is connected to: `sources.status`.
    func connectedSpaces() async throws -> [URL]
    func plan(in space: URL) async throws -> PhotosLibraryPlan
    func reportAlbums(_ albums: [PhotoAlbum]) async throws
    /// The identifiers of `ids` the space holds already.
    func known(_ ids: [String], in space: URL) async throws -> Set<String>
    func add(_ export: PhotosLibraryExport, in space: URL) async throws -> PhotosLibraryAdded
    /// Ends a run. `present` is every asset of the albums picked, or `nil` when an album picked
    /// was not found: then nothing is marked.
    func sync(present: [String]?, failed: [PhotosLibraryFailure], albums: [PhotoAlbum], token: String?, added: Int, waiting: Int, held: Int, in space: URL) async throws
    func fail(kind: String, message: String, in space: URL) async throws
    func options(in space: URL) async throws -> PhotosLibraryOptions
    func setICloud(_ on: Bool, in space: URL) async throws -> PhotosLibraryOptions
}

/// The JSON of the `photos_library.*` params.
enum PhotosLibraryWire {
    static let source = "photos-library"

    static func albums(_ albums: [PhotoAlbum]) -> JSONValue {
        .array(albums.map { ["id": .string($0.id), "name": .string($0.name)] })
    }

    static func asset(_ export: PhotosLibraryExport) -> JSONValue {
        let a = export.asset
        var asset: [String: JSONValue] = [
            "id": .string(a.id), "kind": .string(a.kind.rawValue),
            "path": .string(export.file.standardizedFileURL.path(percentEncoded: false)),
            "title": .string((export.originalName as NSString).deletingPathExtension),
            "metadata": PhotoWire.metadata(export.metadata),
        ]
        if let createdAt = a.createdAt { asset["created_at"] = .int(RecordingWire.milliseconds(createdAt)) }
        if a.kind == .video { asset["duration_ms"] = .int(Int64((max(a.duration, 0) * 1000).rounded())) }
        if let original = export.original { asset["original"] = .string(original) }
        if a.kind == .video, let content = export.content { asset["content"] = .string(content) }
        return .object(asset)
    }
}

/// The `PhotosLibraryClient` of Knode Service, over its socket as the person.
struct ServicePhotosLibraryClient: PhotosLibraryClient {
    var socketURL: URL = KnodeHome.current.socketURL
    var person: String = CoreSpaceStore.currentUserName

    private struct SpaceSources: Decodable {
        var space: String
        var sources: [SourceStatus]
    }

    private func space(_ url: URL) -> JSONValue { .string(ServiceSpaceStore.path(of: url)) }

    func connectedSpaces() async throws -> [URL] {
        let list = try await call("sources.status", ["source": .string(PhotosLibraryWire.source)], as: [SpaceSources].self)
        return list.filter { !$0.sources.isEmpty }.map { URL(filePath: $0.space, directoryHint: .isDirectory) }
    }

    func plan(in space: URL) async throws -> PhotosLibraryPlan {
        try await call("photos_library.plan", ["space": self.space(space)], as: PhotosLibraryPlan.self)
    }

    func reportAlbums(_ albums: [PhotoAlbum]) async throws {
        _ = try await call("photos_library.albums", ["albums": PhotosLibraryWire.albums(albums)], as: Bool.self)
    }

    func known(_ ids: [String], in space: URL) async throws -> Set<String> {
        Set(try await call("photos_library.known", ["space": self.space(space), "ids": .array(ids.map(JSONValue.string))], as: [String].self))
    }

    func add(_ export: PhotosLibraryExport, in space: URL) async throws -> PhotosLibraryAdded {
        try await call("photos_library.add", ["space": self.space(space), "asset": PhotosLibraryWire.asset(export)], as: PhotosLibraryAdded.self)
    }

    func sync(present: [String]?, failed: [PhotosLibraryFailure], albums: [PhotoAlbum], token: String?, added: Int, waiting: Int, held: Int, in space: URL) async throws {
        var params: [String: JSONValue] = [
            "space": self.space(space), "albums": PhotosLibraryWire.albums(albums),
            "added": .int(Int64(added)), "waiting": .int(Int64(waiting)), "held": .int(Int64(held)),
            "failed": .array(failed.map { ["id": .string($0.id), "reason": .string($0.reason)] }),
        ]
        if let present { params["present"] = .array(present.map(JSONValue.string)) }
        if let token { params["token"] = .string(token) }
        _ = try await call("photos_library.sync", params, as: Synced.self)
    }

    func fail(kind: String, message: String, in space: URL) async throws {
        _ = try await call("photos_library.fail", ["space": self.space(space), "kind": .string(kind), "message": .string(message)], as: SourceStatus.self)
    }

    private struct Synced: Decodable {
        struct Marked: Decodable {
            var removed: Int
            var restored: Int
        }

        var marked: Marked
    }

    func options(in space: URL) async throws -> PhotosLibraryOptions {
        try await call("photos_library.options", ["space": self.space(space)], as: PhotosLibraryOptions.self)
    }

    func setICloud(_ on: Bool, in space: URL) async throws -> PhotosLibraryOptions {
        try await call("photos_library.configure", [
            "space": self.space(space), "icloud": .bool(on), "by": ["kind": "person", "actor": .string(person)],
        ], as: PhotosLibraryOptions.self)
    }

    private func call<Result: Decodable & Sendable>(_ method: String, _ params: [String: JSONValue], as type: Result.Type) async throws -> Result {
        let connection = try ServiceConnection.connect(to: socketURL)
        defer { connection.close() }
        return try await connection.call(method, params, as: type)
    }
}
