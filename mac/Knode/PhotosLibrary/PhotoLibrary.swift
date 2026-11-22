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
    /// The name of its original file, such as `IMG_0042.HEIC`.
    var originalName: String
    var latitude: Double?
    var longitude: Double?
    var width: Int?
    var height: Int?
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
    /// Every photo and video in these albums, each once.
    func assets(inAlbums albums: Set<String>) -> [PhotoAsset]
    /// The change token of the library now, archived as text, when the system has one: a run
    /// with the same token as the last one has nothing new to read.
    func changeToken() -> String?
    /// Says when the library changed.
    func changes() -> AsyncStream<Void>
    /// Writes the asset's original into `folder` and returns it. Without `allowNetwork`, an
    /// original kept only in iCloud throws `PhotoExportError.inCloud`.
    func exportOriginal(of asset: PhotoAsset, into folder: URL, allowNetwork: Bool) async throws -> URL
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

    func assets(inAlbums albums: Set<String>) -> [PhotoAsset] {
        guard access == .authorized, !albums.isEmpty else { return [] }
        let collections = PHAssetCollection.fetchAssetCollections(withLocalIdentifiers: Array(albums), options: nil)
        let options = PHFetchOptions()
        options.predicate = NSPredicate(format: "mediaType == %d || mediaType == %d", PHAssetMediaType.image.rawValue, PHAssetMediaType.video.rawValue)
        var seen = Set<String>()
        var out: [PhotoAsset] = []
        collections.enumerateObjects { collection, _, _ in
            PHAsset.fetchAssets(in: collection, options: options).enumerateObjects { asset, _, _ in
                guard seen.insert(asset.localIdentifier).inserted else { return }
                out.append(Self.asset(asset))
            }
        }
        return out
    }

    private static func asset(_ asset: PHAsset) -> PhotoAsset {
        let resources = PHAssetResource.assetResources(for: asset)
        let original = resources.first { [.photo, .video].contains($0.type) } ?? resources.first
        return PhotoAsset(
            id: asset.localIdentifier, kind: asset.mediaType == .video ? .video : .photo,
            createdAt: asset.creationDate, duration: asset.duration,
            originalName: original?.originalFilename ?? (asset.mediaType == .video ? "Video.mov" : "Photo.heic"),
            latitude: asset.location?.coordinate.latitude, longitude: asset.location?.coordinate.longitude,
            width: asset.pixelWidth, height: asset.pixelHeight
        )
    }

    func changeToken() -> String? {
        guard access == .authorized else { return nil }
        let token = PHPhotoLibrary.shared().currentChangeToken
        return (try? NSKeyedArchiver.archivedData(withRootObject: token, requiringSecureCoding: true))?.base64EncodedString()
    }

    func changes() -> AsyncStream<Void> {
        AsyncStream { continuation in
            let observer = ChangeObserver { continuation.yield() }
            PHPhotoLibrary.shared().register(observer)
            continuation.onTermination = { _ in PHPhotoLibrary.shared().unregisterChangeObserver(observer) }
        }
    }

    func exportOriginal(of asset: PhotoAsset, into folder: URL, allowNetwork: Bool) async throws -> URL {
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
        do {
            try await PHAssetResourceManager.default().writeData(for: resource, toFile: output, options: options)
        } catch let error as NSError {
            if error.domain == PHPhotosErrorDomain, error.code == PHPhotosError.Code.networkAccessRequired.rawValue {
                throw PhotoExportError.inCloud
            }
            throw PhotoExportError.failed(error.localizedDescription)
        }
        return output
    }
}

private final class ChangeObserver: NSObject, PHPhotoLibraryChangeObserver, @unchecked Sendable {
    let changed: @Sendable () -> Void

    init(_ changed: @escaping @Sendable () -> Void) {
        self.changed = changed
    }

    func photoLibraryDidChange(_ changeInstance: PHChange) { changed() }
}
