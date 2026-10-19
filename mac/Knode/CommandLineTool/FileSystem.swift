import Foundation

/// The few file system questions the `knode` command installer asks, so tests can answer them.
public protocol FileSystem: Sendable {
    /// Whether an executable file is at `url`, following symbolic links.
    func isExecutableFile(at url: URL) -> Bool
    /// Whether `url` is a folder this process can create files in.
    func isWritableDirectory(at url: URL) -> Bool
    /// Where the symbolic link at `url` points, or `nil` if `url` is not a symbolic link.
    func destinationOfSymbolicLink(at url: URL) -> URL?
    /// `url` with every symbolic link in it resolved.
    func resolvingSymbolicLinks(_ url: URL) -> URL
    func createSymbolicLink(at url: URL, pointingTo destination: URL) throws
    func removeItem(at url: URL) throws
}

/// The real file system.
public struct LocalFileSystem: FileSystem {
    public init() {}

    private var manager: FileManager { .default }

    public func isExecutableFile(at url: URL) -> Bool {
        manager.isExecutableFile(atPath: url.path(percentEncoded: false))
    }

    public func isWritableDirectory(at url: URL) -> Bool {
        var isDirectory: ObjCBool = false
        let path = url.path(percentEncoded: false)
        return manager.fileExists(atPath: path, isDirectory: &isDirectory) && isDirectory.boolValue
            && manager.isWritableFile(atPath: path)
    }

    public func destinationOfSymbolicLink(at url: URL) -> URL? {
        guard let destination = try? manager.destinationOfSymbolicLink(atPath: url.path(percentEncoded: false)) else { return nil }
        return URL(filePath: destination, relativeTo: url.deletingLastPathComponent()).absoluteURL.standardizedFileURL
    }

    public func resolvingSymbolicLinks(_ url: URL) -> URL {
        url.resolvingSymlinksInPath()
    }

    public func createSymbolicLink(at url: URL, pointingTo destination: URL) throws {
        try manager.createSymbolicLink(at: url, withDestinationURL: destination)
    }

    public func removeItem(at url: URL) throws {
        try manager.removeItem(at: url)
    }
}
