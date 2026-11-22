import Foundation
import KnodeCore

/// How much room a space takes, what fills it and its limits (`PROTOCOL.md`, "Storage"). Every size
/// is computed by the core on this Mac; the app only shows it.
///
/// `ServiceSpaceStore` implements it through Knode Service, `CoreSpaceStore` with the core in the
/// app, and `RoutingSpaceStore` with whichever of the two holds the space.
protocol SpaceStorage: Sendable {
    /// What the space takes, with its `top` largest files.
    func storageSummary(top: Int, in space: Space.ID) async throws -> StorageSummary
    /// What the space would take after adding `size` bytes, and whether to ask first.
    func storageCheck(adding size: Int64, in space: Space.ID) async throws -> StorageCheck
    /// Changes the soft limit (`0` never warns) or whether large originals are kept; `nil` leaves
    /// one as it is.
    func setStorageLimits(warnAt: Int64?, keepLargeOriginals: Bool?, in space: Space.ID) async throws -> StorageLimits
}

/// The bytes a space takes, by kind.
struct StorageKinds: Hashable, Sendable, Decodable {
    var notes: Int64
    var photos: Int64
    var videos: Int64
    var audio: Int64
    var other: Int64
    /// Files only deleted nodes use: a purge frees them.
    var deleted: Int64
}

/// What one source, agent or person added.
struct StorageSource: Hashable, Sendable, Decodable {
    var actor: String
    var kind: String
    var size: Int64
    var files: Int
}

/// One of the largest files of a space.
struct StorageFile: Hashable, Sendable, Decodable, Identifiable {
    var node: Node.ID
    var title: String
    var kind: String
    var name: String
    var size: Int64
    var mediaType: String
    var addedBy: String

    var id: String { "\(node)/\(name)" }
}

/// The limits of a space.
struct StorageLimits: Hashable, Sendable, Decodable {
    /// The largest file a space attaches: 100 MB.
    var maxFile: Int64
    /// The largest original kept when the space keeps large originals: 4 GB.
    var maxOriginal: Int64
    var keepLargeOriginals: Bool
    /// The soft limit; `0` never warns.
    var warnAt: Int64
}

/// How much room a space takes: `storage.summary`.
struct StorageSummary: Hashable, Sendable, Decodable {
    var total: Int64
    var kinds: StorageKinds
    var nodes: Int
    var files: Int
    var photos: Int
    var recordings: Int
    var sources: [StorageSource]
    var largest: [StorageFile]
    var limits: StorageLimits
    var over: Bool
}

/// What a space would take after adding some bytes: `storage.check`.
struct StorageCheck: Hashable, Sendable, Decodable {
    var total: Int64
    var after: Int64
    var warnAt: Int64
    var warn: Bool
}

enum StorageText {
    /// A size for people, such as `1.2 GB`.
    static func size(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    /// Who added files, for people: a source id becomes its name.
    static func actor(_ actor: String) -> String {
        switch actor {
        case "photos-library": "Photos library"
        case "voice-memos": "Voice Memos"
        case "recorder": "Meetings"
        case "photos": "Photos"
        default: actor
        }
    }
}

// MARK: - Knode Service

extension ServiceSpaceStore: SpaceStorage {
    func storageSummary(top: Int, in space: Space.ID) async throws -> StorageSummary {
        let path = try path(space)
        return try await call("storage.summary", ["space": .string(path), "top": .int(Int64(top))], as: StorageSummary.self)
    }

    func storageCheck(adding size: Int64, in space: Space.ID) async throws -> StorageCheck {
        let path = try path(space)
        return try await call("storage.check", ["space": .string(path), "size": .int(max(size, 0))], as: StorageCheck.self)
    }

    func setStorageLimits(warnAt: Int64?, keepLargeOriginals: Bool?, in space: Space.ID) async throws -> StorageLimits {
        let path = try path(space)
        var params: [String: JSONValue] = ["space": .string(path), "by": author]
        if let warnAt { params["warn_at"] = .int(max(warnAt, 0)) }
        if let keepLargeOriginals { params["keep_large_originals"] = .bool(keepLargeOriginals) }
        return try await call("storage.configure", params, as: StorageLimits.self)
    }
}

// MARK: - The core in the app

extension CoreSpaceStore: SpaceStorage {
    func storageSummary(top: Int, in space: Space.ID) async throws -> StorageSummary {
        let core = try self.core(space)
        let s = try await run { try core.storageSummary(top: UInt32(clamping: max(top, 0))) }
        return StorageSummary(
            total: Int64(clamping: s.total),
            kinds: StorageKinds(
                notes: Int64(clamping: s.kinds.notes), photos: Int64(clamping: s.kinds.photos),
                videos: Int64(clamping: s.kinds.videos), audio: Int64(clamping: s.kinds.audio),
                other: Int64(clamping: s.kinds.other), deleted: Int64(clamping: s.kinds.deleted)
            ),
            nodes: Int(clamping: s.nodes), files: Int(clamping: s.files),
            photos: Int(clamping: s.photos), recordings: Int(clamping: s.recordings),
            sources: s.sources.map { StorageSource(actor: $0.actor, kind: $0.kind, size: Int64(clamping: $0.size), files: Int(clamping: $0.files)) },
            largest: s.largest.map {
                StorageFile(node: $0.node, title: $0.title, kind: $0.kind, name: $0.name, size: Int64(clamping: $0.size),
                            mediaType: $0.mediaType, addedBy: $0.addedBy)
            },
            limits: StorageLimits(s.limits), over: s.over
        )
    }

    func storageCheck(adding size: Int64, in space: Space.ID) async throws -> StorageCheck {
        let core = try self.core(space)
        let c = try await run { try core.storageCheck(size: UInt64(max(size, 0))) }
        return StorageCheck(total: Int64(clamping: c.total), after: Int64(clamping: c.after), warnAt: Int64(clamping: c.warnAt), warn: c.warn)
    }

    func setStorageLimits(warnAt: Int64?, keepLargeOriginals: Bool?, in space: Space.ID) async throws -> StorageLimits {
        let core = try self.core(space)
        let author = author
        let warn = warnAt.map { UInt64(max($0, 0)) }
        return StorageLimits(try await run { try core.setStorageLimits(warnAt: warn, keepLargeOriginals: keepLargeOriginals, by: author) })
    }
}

extension StorageLimits {
    init(_ l: KnodeCore.StorageLimits) {
        self.init(maxFile: Int64(clamping: l.maxFile), maxOriginal: Int64(clamping: l.maxOriginal),
                  keepLargeOriginals: l.keepLargeOriginals, warnAt: Int64(clamping: l.warnAt))
    }
}

// MARK: - Whichever holds the space

extension RoutingSpaceStore: SpaceStorage {
    func storageSummary(top: Int, in space: Space.ID) async throws -> StorageSummary {
        try await storage(space) { try await $0.storageSummary(top: top, in: space) }
    }

    func storageCheck(adding size: Int64, in space: Space.ID) async throws -> StorageCheck {
        try await storage(space) { try await $0.storageCheck(adding: size, in: space) }
    }

    func setStorageLimits(warnAt: Int64?, keepLargeOriginals: Bool?, in space: Space.ID) async throws -> StorageLimits {
        try await storage(space) { try await $0.setStorageLimits(warnAt: warnAt, keepLargeOriginals: keepLargeOriginals, in: space) }
    }

    private func storage<T: Sendable>(_ space: Space.ID, _ work: (any SpaceStorage) async throws -> T) async throws -> T {
        try await route(space) { store in
            guard let store = store as? any SpaceStorage else {
                throw SpaceStoreError.unsupported("this space cannot say how much room it takes")
            }
            return try await work(store)
        }
    }
}
