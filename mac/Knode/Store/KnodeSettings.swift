import Foundation
import KnodeCore
import Synchronization

/// The Knode settings shared with the `knode` command and Knode Service: the default space in
/// `$KNODE_HOME/config.toml`, which agents and capture write to, and where new spaces go.
protocol KnodeSettings: Sendable {
    /// The default space for agents and capture, or `nil` when there is none.
    func defaultSpace() throws -> URL?
    /// Makes a space the default for agents and capture, or leaves none with `nil`.
    func setDefaultSpace(_ url: URL?) throws
    /// The folder new spaces go to by default, `~/Knode`.
    var spacesFolder: URL { get }
    /// The spaces in `spacesFolder`.
    func spacesInFolder() -> [URL]
}

/// The settings in `$KNODE_HOME/config.toml`, read and written by the Rust core
/// (`knode-config`), so the app, the `knode` command and Knode Service agree.
struct CoreKnodeSettings: KnodeSettings {
    func defaultSpace() throws -> URL? {
        do {
            return try KnodeCore.defaultSpace().map { URL(filePath: $0, directoryHint: .isDirectory).standardizedFileURL }
        } catch {
            throw CoreSpaceStore.storeError(error)
        }
    }

    func setDefaultSpace(_ url: URL?) throws {
        do {
            try KnodeCore.setDefaultSpace(path: url.map { ServiceSpaceStore.path(of: $0) })
        } catch {
            throw CoreSpaceStore.storeError(error)
        }
    }

    var spacesFolder: URL {
        let path = (try? KnodeCore.spacesFolder()) ?? NSHomeDirectory() + "/Knode"
        return URL(filePath: path, directoryHint: .isDirectory)
    }

    func spacesInFolder() -> [URL] {
        ((try? KnodeCore.spacesInFolder()) ?? []).map { URL(filePath: $0, directoryHint: .isDirectory).standardizedFileURL }
    }
}

/// Settings kept in memory, for tests and previews.
final class MemoryKnodeSettings: KnodeSettings {
    private let state: Mutex<URL?>
    let spacesFolder: URL

    init(defaultSpace: URL? = nil, spacesFolder: URL = URL(filePath: "/tmp/Knode", directoryHint: .isDirectory)) {
        state = Mutex(defaultSpace)
        self.spacesFolder = spacesFolder
    }

    func defaultSpace() throws -> URL? {
        state.withLock { $0 }
    }

    func setDefaultSpace(_ url: URL?) throws {
        state.withLock { $0 = url?.standardizedFileURL }
    }

    func spacesInFolder() -> [URL] {
        []
    }
}
