import CryptoKit
import Foundation
import Photos

/// Whether the app may read the Photos library of this Mac.
enum PhotoLibraryAccess: Equatable, Sendable {
    /// Never asked: connecting the source asks.
    case notDetermined
    case denied
    /// A profile or parental controls forbid it.
    case restricted
    /// The whole library, or only the photos the person chose for Knode (limited).
    case authorized
}

/// An album of the Photos library.
struct PhotoAlbum: Hashable, Sendable, Identifiable {
    /// Its `localIdentifier`.
    var id: String
    var name: String
}

/// A photo or a video of the library, as the app read it.
struct PhotoAsset: Hashable, Sendable, Identifiable {
    enum Kind: String, Hashable, Sendable { case photo, video }

    /// Its `localIdentifier`: stable for a library on one Mac.
    var id: String
    var kind: Kind
    var createdAt: Date?
    /// How long a video lasts.
    var duration: TimeInterval
    /// A name for its original file when the export has none, such as `IMG_0042.HEIC`. Listing
    /// assets never reads their resources (that is slow): the real name comes with the export.
    var originalName: String
    var latitude: Double?
    var longitude: Double?
    var width: Int?
    var height: Int?
}

/// The assets of the albums asked for, each once, and which of those albums were found: an album
/// picked on this Mac can be deleted since.
struct PhotoAlbumContents: Hashable, Sendable {
    var assets: [PhotoAsset]
    var found: Set<String>
}

/// What changed in the library since a change token.
struct PhotoLibraryChanges: Hashable, Sendable {
    /// Assets inserted, updated or deleted.
    var assets: Set<String> = []
    /// Albums inserted, updated (such as what they hold) or deleted.
    var albums: Set<String> = []
}

/// An original written by `exportOriginal`, with the hash of its bytes, computed while writing.
struct ExportedOriginal: Hashable, Sendable {
    var file: URL
    /// `sha256:` and the hex SHA-256 of the file.
    var content: String
}

/// Hashes bytes as they come: `sha256:<hex>`.
struct ContentHasher {
    private var hasher = SHA256()

    mutating func update(_ data: Data) { hasher.update(data: data) }

    mutating func finish() -> String {
        "sha256:" + hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    /// The content of a file, read in chunks.
    static func content(of url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = ContentHasher()
        while let chunk = try handle.read(upToCount: 1 << 20), !chunk.isEmpty { hasher.update(chunk) }
        return hasher.finish()
    }
}

enum PhotoExportError: Error, LocalizedError, Equatable {
    /// The original is only in iCloud and downloading it is not allowed.
    case inCloud
    case noOriginal
    case failed(String)

    var errorDescription: String? {
        switch self {
        case .inCloud: "its original is only in iCloud"
        case .noOriginal: "it has no original to export"
        case .failed(let message): message
        }
    }
}

/// The Photos library of this Mac, read only. `PhotoKitLibrary` reads PhotoKit; tests use a fake,
/// so no test ever asks for access to a real library.
protocol PhotoLibrary: Sendable {
    var access: PhotoLibraryAccess { get }
    /// Asks the person for access: only when they connect the source.
    func requestAccess() async -> PhotoLibraryAccess
    func albums() -> [PhotoAlbum]
    /// Every photo and video in these albums, each once, and which albums were found.
    func assets(inAlbums albums: Set<String>) -> PhotoAlbumContents
    /// The change token of the library now, archived as text, when the system has one: a run
    /// with the same token as the last one has nothing new to read.
    func changeToken() -> String?
    /// What changed since `token` (from `changeToken()`): `nil` when the system cannot tell any
    /// more (the token expired or is not one), and everything must be read.
    func changes(since token: String) -> PhotoLibraryChanges?
    /// Says when the library changed.
    func changes() -> AsyncStream<Void>
    /// Writes the asset's original into `folder` and returns it with its hash. Without
    /// `allowNetwork`, an original kept only in iCloud throws `PhotoExportError.inCloud`.
    func exportOriginal(of asset: PhotoAsset, into folder: URL, allowNetwork: Bool) async throws -> ExportedOriginal
}

/// The Photos library through PhotoKit.
final class PhotoKitLibrary: PhotoLibrary, @unchecked Sendable {
    var access: PhotoLibraryAccess {
        Self.access(PHPhotoLibrary.authorizationStatus(for: .readWrite))
    }

    func requestAccess() async -> PhotoLibraryAccess {
        Self.access(await PHPhotoLibrary.requestAuthorization(for: .readWrite))
    }

    private static func access(_ status: PHAuthorizationStatus) -> PhotoLibraryAccess {
        switch status {
        case .authorized, .limited: .authorized
        case .denied: .denied
        case .restricted: .restricted
        case .notDetermined: .notDetermined
        @unknown default: .denied
        }
    }

    func albums() -> [PhotoAlbum] {
        guard access == .authorized else { return [] }
        var out: [PhotoAlbum] = []
        for type in [PHAssetCollectionType.album, .smartAlbum] {
            let collections = PHAssetCollection.fetchAssetCollections(with: type, subtype: .any, options: nil)
            collections.enumerateObjects { collection, _, _ in
                // Smart albums only where the person keeps something: favourites, videos, screenshots...
                if type == .smartAlbum, PHAsset.fetchAssets(in: collection, options: nil).count == 0 { return }
                out.append(PhotoAlbum(id: collection.localIdentifier, name: collection.localizedTitle ?? "Untitled"))
            }
        }
        return out.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    func assets(inAlbums albums: Set<String>) -> PhotoAlbumContents {
        guard access == .authorized, !albums.isEmpty else { return PhotoAlbumContents(assets: [], found: []) }
        let collections = PHAssetCollection.fetchAssetCollections(withLocalIdentifiers: Array(albums), options: nil)
        let options = PHFetchOptions()
        options.predicate = NSPredicate(format: "mediaType == %d || mediaType == %d", PHAssetMediaType.image.rawValue, PHAssetMediaType.video.rawValue)
        var seen = Set<String>()
        var found = Set<String>()
        var out: [PhotoAsset] = []
        collections.enumerateObjects { collection, _, _ in
            found.insert(collection.localIdentifier)
            PHAsset.fetchAssets(in: collection, options: options).enumerateObjects { asset, _, _ in
                guard seen.insert(asset.localIdentifier).inserted else { return }
                out.append(Self.asset(asset))
            }
        }
        return PhotoAlbumContents(assets: out, found: found)
    }

    private static func asset(_ asset: PHAsset) -> PhotoAsset {
        PhotoAsset(
            id: asset.localIdentifier, kind: asset.mediaType == .video ? .video : .photo,
            createdAt: asset.creationDate, duration: asset.duration,
            originalName: asset.mediaType == .video ? "Video.mov" : "Photo.heic",
            latitude: asset.location?.coordinate.latitude, longitude: asset.location?.coordinate.longitude,
            width: asset.pixelWidth, height: asset.pixelHeight
        )
    }

    func changeToken() -> String? {
        guard access == .authorized else { return nil }
        let token = PHPhotoLibrary.shared().currentChangeToken
        return (try? NSKeyedArchiver.archivedData(withRootObject: token, requiringSecureCoding: true))?.base64EncodedString()
    }

    func changes(since token: String) -> PhotoLibraryChanges? {
        guard access == .authorized, let data = Data(base64Encoded: token),
              let saved = try? NSKeyedUnarchiver.unarchivedObject(ofClass: PHPersistentChangeToken.self, from: data)
        else { return nil }
        do {
            let result = try PHPhotoLibrary.shared().fetchPersistentChanges(since: saved)
            var out = PhotoLibraryChanges()
            // The overlay makes the result a sequence of changes, oldest first.
            for change in result {
                let assets = try change.changeDetails(for: .asset)
                out.assets.formUnion(assets.insertedLocalIdentifiers)
                out.assets.formUnion(assets.updatedLocalIdentifiers)
                out.assets.formUnion(assets.deletedLocalIdentifiers)
                let albums = try change.changeDetails(for: .assetCollection)
                out.albums.formUnion(albums.insertedLocalIdentifiers)
                out.albums.formUnion(albums.updatedLocalIdentifiers)
                out.albums.formUnion(albums.deletedLocalIdentifiers)
            }
            return out
        } catch {
            // The token expired or its details are gone: read everything.
            return nil
        }
    }

    func changes() -> AsyncStream<Void> {
        AsyncStream { continuation in
            let observer = ChangeObserver { continuation.yield() }
            PHPhotoLibrary.shared().register(observer)
            continuation.onTermination = { _ in PHPhotoLibrary.shared().unregisterChangeObserver(observer) }
        }
    }

    func exportOriginal(of asset: PhotoAsset, into folder: URL, allowNetwork: Bool) async throws -> ExportedOriginal {
        guard let found = PHAsset.fetchAssets(withLocalIdentifiers: [asset.id], options: nil).firstObject else {
            throw PhotoExportError.noOriginal
        }
        let resources = PHAssetResource.assetResources(for: found)
        let wanted: PHAssetResourceType = asset.kind == .video ? .video : .photo
        guard let resource = resources.first(where: { $0.type == wanted }) else { throw PhotoExportError.noOriginal }
        let name = resource.originalFilename.isEmpty ? asset.originalName : resource.originalFilename
        let output = folder.appending(path: name)
        try? FileManager.default.removeItem(at: output)
        let options = PHAssetResourceRequestOptions()
        options.isNetworkAccessAllowed = allowNetwork
        guard FileManager.default.createFile(atPath: output.path(percentEncoded: false), contents: nil),
              let handle = try? FileHandle(forWritingTo: output)
        else { throw PhotoExportError.failed("cannot write \(name)") }
        // Written and hashed as the bytes come, so the original is read once.
        let writer = ExportWriter(handle: handle)
        do {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                PHAssetResourceManager.default().requestData(for: resource, options: options) { data in
                    writer.write(data)
                } completionHandler: { error in
                    if let error { continuation.resume(throwing: error) } else { continuation.resume() }
                }
            }
            let content = try writer.finish()
            return ExportedOriginal(file: output, content: content)
        } catch let error as NSError {
            try? writer.finish()
            try? FileManager.default.removeItem(at: output)
            if error.domain == PHPhotosErrorDomain, error.code == PHPhotosError.Code.networkAccessRequired.rawValue {
                throw PhotoExportError.inCloud
            }
            throw PhotoExportError.failed(error.localizedDescription)
        }
    }
}

/// Writes the chunks of an export to its file and hashes them. PhotoKit calls `write` one chunk
/// at a time, in order.
private final class ExportWriter: @unchecked Sendable {
    private let lock = NSLock()
    private let handle: FileHandle
    private var hasher = ContentHasher()
    private var error: Error?

    init(handle: FileHandle) { self.handle = handle }

    func write(_ data: Data) {
        lock.withLock {
            guard error == nil else { return }
            do {
                try handle.write(contentsOf: data)
                hasher.update(data)
            } catch {
                self.error = error
            }
        }
    }

    @discardableResult
    func finish() throws -> String {
        try lock.withLock {
            try? handle.close()
            if let error { throw error }
            return hasher.finish()
        }
    }
}

private final class ChangeObserver: NSObject, PHPhotoLibraryChangeObserver, @unchecked Sendable {
    let changed: @Sendable () -> Void

    init(_ changed: @escaping @Sendable () -> Void) {
        self.changed = changed
    }

    func photoLibraryDidChange(_ changeInstance: PHChange) { changed() }
}
