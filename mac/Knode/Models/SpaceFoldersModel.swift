import Foundation
import Observation

/// The Folders and Capture screen of one space: the folders that feed it, capture into the space
/// and into each folder. It follows changes made elsewhere, such as with `knode folders`, while
/// Knode Service runs.
@MainActor
@Observable
final class SpaceFoldersModel {
    let spaceID: Space.ID
    /// The name of the space, for the screen's title.
    let spaceName: String
    /// The folders, oldest first.
    private(set) var folders: [SpaceFolder] = []
    /// Whether captured conversations are written to the space at all.
    private(set) var captureEnabled = true
    private(set) var isLoaded = false
    /// A change is being made.
    private(set) var isBusy = false
    var errorMessage: String?

    @ObservationIgnored private let store: any SpaceStore
    @ObservationIgnored private var changesTask: Task<Void, Never>?

    init(space: Space, store: any SpaceStore) {
        spaceID = space.id
        spaceName = space.name
        self.store = store
        let changes = store.changes()
        let id = space.id
        changesTask = Task { [weak self] in
            for await change in changes where change.space == id {
                switch change.kind {
                case .folders, .settings, .reload: await self?.load()
                default: break
                }
            }
        }
    }

    /// Stops following changes, when the screen closes.
    func stop() {
        changesTask?.cancel()
        changesTask = nil
    }

    func load() async {
        do {
            async let folders = store.folders(of: spaceID)
            async let capture = store.captureEnabled(in: spaceID)
            let loaded = try await (folders, capture)
            self.folders = loaded.0
            captureEnabled = loaded.1
            isLoaded = true
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Turns capture into the whole space on or off.
    func setCaptureEnabled(_ enabled: Bool) async {
        await change { try await self.store.setCaptureEnabled(enabled, in: self.spaceID) }
    }

    /// Turns capture of one folder on or off, keeping its rules.
    func setCapture(_ capture: Bool, for folder: SpaceFolder) async {
        await change { _ = try await self.store.setFolder(folder.draft(capture: capture), in: self.spaceID) }
    }

    /// Adds a folder; an agent folder names the agent whose sessions it holds.
    func addFolder(_ kind: SpaceFolder.Kind, at url: URL, agent: String? = nil) async {
        await change { _ = try await self.store.setFolder(.adding(kind, at: url, agent: agent), in: self.spaceID) }
    }

    /// Removes a folder. Nothing captured from it is removed.
    func remove(_ folder: SpaceFolder) async {
        await change { try await self.store.removeFolder(folder.id, from: self.spaceID) }
    }

    /// Makes a change, then reads the folders again.
    private func change(_ work: @escaping () async throws -> Void) async {
        isBusy = true
        defer { isBusy = false }
        do {
            try await work()
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
        await load()
    }
}
