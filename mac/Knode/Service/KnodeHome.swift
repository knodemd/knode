import Foundation

/// Where Knode keeps its own files, and where Knode Service listens.
///
/// The Knode home is `$KNODE_HOME` when it is set, else `~/Library/Application Support/Knode`.
public struct KnodeHome: Sendable, Equatable {
    public var url: URL

    public init(url: URL) {
        self.url = url
    }

    /// The Knode home of this process.
    public static var current: KnodeHome {
        resolve(environment: ProcessInfo.processInfo.environment, userHome: FileManager.default.homeDirectoryForCurrentUser)
    }

    static func resolve(environment: [String: String], userHome: URL) -> KnodeHome {
        if let home = environment["KNODE_HOME"], !home.isEmpty {
            return KnodeHome(url: URL(filePath: home, directoryHint: .isDirectory))
        }
        return KnodeHome(url: userHome.appending(path: "Library/Application Support/Knode", directoryHint: .isDirectory))
    }

    /// The Unix socket of Knode Service: `$KNODE_HOME/service.sock`.
    public var socketURL: URL { url.appending(path: "service.sock", directoryHint: .notDirectory) }
}
