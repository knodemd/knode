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
    /// Every space Knode knows (`default_space` and `spaces` in `config.toml`): capture routes
    /// conversations over their folders.
    func knownSpaces() -> [URL]
    /// Adds a space to `spaces` in `config.toml`, unless Knode knows it already.
    func rememberSpace(_ url: URL) throws
    /// Takes a space out of `config.toml`, and out of the default if it was. Its files stay.
    func forgetSpace(_ url: URL) throws
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

    func knownSpaces() -> [URL] {
        ((try? KnodeCore.knownSpaces()) ?? []).map { URL(filePath: $0, directoryHint: .isDirectory).standardizedFileURL }
    }

    func rememberSpace(_ url: URL) throws {
        do {
            _ = try KnodeCore.rememberSpace(path: ServiceSpaceStore.path(of: url))
        } catch {
            throw CoreSpaceStore.storeError(error)
        }
    }

    func forgetSpace(_ url: URL) throws {
        do {
            _ = try KnodeCore.forgetSpace(path: ServiceSpaceStore.path(of: url))
        } catch {
            throw CoreSpaceStore.storeError(error)
        }
    }
}

/// Settings kept in memory, for tests and previews. They follow `config.toml`: a new default
/// keeps the previous one known.
final class MemoryKnodeSettings: KnodeSettings {
    private struct State {
        var defaultSpace: URL?
        var spaces: [URL] = []
    }

    private let state: Mutex<State>
    let spacesFolder: URL

    init(defaultSpace: URL? = nil, spaces: [URL] = [], spacesFolder: URL = URL(filePath: "/tmp/Knode", directoryHint: .isDirectory)) {
        state = Mutex(State(defaultSpace: defaultSpace?.standardizedFileURL, spaces: spaces.map(\.standardizedFileURL)))
        self.spacesFolder = spacesFolder
    }

    func defaultSpace() throws -> URL? {
        state.withLock { $0.defaultSpace }
    }

    func setDefaultSpace(_ url: URL?) throws {
        state.withLock { state in
            if let previous = state.defaultSpace, previous != url?.standardizedFileURL, !state.spaces.contains(previous) {
                state.spaces.insert(previous, at: 0)
            }
            state.defaultSpace = url?.standardizedFileURL
            state.spaces.removeAll { $0 == state.defaultSpace }
        }
    }

    func spacesInFolder() -> [URL] {
        []
    }

    func knownSpaces() -> [URL] {
        state.withLock { state in [state.defaultSpace].compactMap { $0 } + state.spaces }
    }

    func rememberSpace(_ url: URL) throws {
        let url = url.standardizedFileURL
        state.withLock { state in
            if state.defaultSpace != url, !state.spaces.contains(url) { state.spaces.append(url) }
        }
    }

    func forgetSpace(_ url: URL) throws {
        let url = url.standardizedFileURL
        state.withLock { state in
            state.spaces.removeAll { $0 == url }
            if state.defaultSpace == url { state.defaultSpace = nil }
        }
    }
}
