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
    /// What adding the last project folder did to its project, for the screen to say.
    private(set) var notice: String?
    /// Called when adding a folder created a project, so the sidebar lists it.
    @ObservationIgnored var projectsChanged: (@MainActor () async -> Void)?

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

    /// Adds a folder; an agent folder names the agent whose sessions it holds. A project folder
    /// also adds the project named after it, or finds the one the space has (the core does it).
    func addFolder(_ kind: SpaceFolder.Kind, at url: URL, agent: String? = nil, capture: Bool = true) async {
        notice = nil
        var added: SpaceFolder?
        await change {
            var draft = FolderDraft.adding(kind, at: url, agent: agent)
            draft.capture = capture
            added = try await self.store.setFolder(draft, in: self.spaceID)
        }
        guard let project = added?.project else { return }
        notice = Self.notice(for: project)
        if project.created { await projectsChanged?() }
    }

    /// Removes a folder. Nothing captured from it is removed, and a project folder's project stays.
    func remove(_ folder: SpaceFolder) async {
        notice = nil
        await change { try await self.store.removeFolder(folder.id, from: self.spaceID) }
    }

    /// What adding a project folder did, in one sentence.
    static func notice(for project: SpaceFolder.Project) -> String {
        let title = "\u{201C}\(project.title)\u{201D}"
        var notice: String
        if let other = project.passedOver {
            notice = "Added the project \(title), named after its parent folder too: \u{201C}\(other)\u{201D} already holds the files of \(project.passedOverFolders.joined(separator: ", ")). Removing the folder later keeps the project and its notes."
        } else if project.created {
            notice = "Added the project \(title). Removing the folder later keeps the project and its notes."
        } else if project.alreadyHeld {
            notice = "This folder already holds the files of \(title)."
        } else {
            notice = "This folder holds the files of \(title), the project this space already had."
        }
        if !project.sharedWith.isEmpty {
            notice += " \(title) also holds the files of \(project.sharedWith.joined(separator: ", "))."
        }
        return notice
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
