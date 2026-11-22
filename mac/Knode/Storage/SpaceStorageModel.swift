import Foundation
import Observation

/// The storage view of one space: what it takes, by kind and by source, its largest files and its
/// limits. The sizes come from the core; removing a file is the attachment's own removal, which
/// the history can undo.
@MainActor
@Observable
final class SpaceStorageModel {
    /// The soft limits offered, in bytes; `0` never warns.
    static let warnChoices: [Int64] = [0, 1, 5, 10, 25, 50, 100].map { Int64($0) * 1024 * 1024 * 1024 }
    /// How many of the largest files are listed.
    static let top = 10

    let spaceID: Space.ID
    private(set) var summary: StorageSummary?
    private(set) var isBusy = false
    var errorMessage: String?

    @ObservationIgnored private let storage: any SpaceStorage
    @ObservationIgnored private let attachments: (any AttachmentStore)?
    @ObservationIgnored private let showNode: @MainActor (Node.ID) async -> Void

    init(
        space: Space.ID, storage: any SpaceStorage, attachments: (any AttachmentStore)?,
        show: @escaping @MainActor (Node.ID) async -> Void = { _ in }
    ) {
        spaceID = space
        self.storage = storage
        self.attachments = attachments
        showNode = show
    }

    /// Whether the largest files can be removed here.
    var canRemove: Bool { attachments != nil }

    /// The kinds, largest first, without the empty ones.
    var kinds: [(name: String, size: Int64)] {
        guard let k = summary?.kinds else { return [] }
        return [("Notes", k.notes), ("Photos", k.photos), ("Videos", k.videos), ("Audio", k.audio),
                ("Other files", k.other), ("Deleted, freed by a purge", k.deleted)]
            .filter { $0.1 > 0 }
            .sorted { $0.1 > $1.1 }
    }

    func load() async {
        await busy { self.summary = try await self.storage.storageSummary(top: Self.top, in: self.spaceID) }
    }

    /// Opens the node of a file in the main window.
    func show(_ file: StorageFile) async {
        await showNode(file.node)
    }

    /// Removes a file from its node, keeping only the node's text. Undoable from the history.
    func remove(_ file: StorageFile) async {
        guard let attachments else { return }
        await busy {
            try await attachments.removeAttachment(named: file.name, from: file.node, in: self.spaceID)
            self.summary = try await self.storage.storageSummary(top: Self.top, in: self.spaceID)
        }
    }

    func setWarnAt(_ bytes: Int64) async {
        await busy {
            _ = try await self.storage.setStorageLimits(warnAt: bytes, keepLargeOriginals: nil, in: self.spaceID)
            self.summary = try await self.storage.storageSummary(top: Self.top, in: self.spaceID)
        }
    }

    func setKeepLargeOriginals(_ on: Bool) async {
        await busy {
            let limits = try await self.storage.setStorageLimits(warnAt: nil, keepLargeOriginals: on, in: self.spaceID)
            self.summary?.limits = limits
        }
    }

    private func busy(_ work: @escaping @MainActor () async throws -> Void) async {
        isBusy = true
        defer { isBusy = false }
        do {
            try await work()
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
