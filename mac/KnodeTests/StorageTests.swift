import Foundation
import Testing
@testable import Knode

/// The storage of a space in memory.
actor FakeSpaceStorage: SpaceStorage {
    var summary: StorageSummary
    var limitsSet: [(warnAt: Int64?, keep: Bool?)] = []

    init(summary: StorageSummary) { self.summary = summary }

    func storageSummary(top: Int, in space: Space.ID) async throws -> StorageSummary {
        var s = summary
        s.largest = Array(s.largest.prefix(top))
        return s
    }

    func storageCheck(adding size: Int64, in space: Space.ID) async throws -> StorageCheck {
        let after = summary.total + size
        return StorageCheck(total: summary.total, after: after, warnAt: summary.limits.warnAt,
                            warn: summary.limits.warnAt > 0 && after > summary.limits.warnAt)
    }

    func setStorageLimits(warnAt: Int64?, keepLargeOriginals: Bool?, in space: Space.ID) async throws -> StorageLimits {
        limitsSet.append((warnAt, keepLargeOriginals))
        if let warnAt { summary.limits.warnAt = warnAt }
        if let keepLargeOriginals { summary.limits.keepLargeOriginals = keepLargeOriginals }
        return summary.limits
    }

    func drop(_ name: String) {
        summary.largest.removeAll { $0.name == name }
    }
}

/// Removing attachments, in memory, telling the storage.
actor FakeAttachmentRemover: AttachmentStore {
    let storage: FakeSpaceStorage
    var removed: [(name: String, node: Node.ID)] = []

    init(storage: FakeSpaceStorage) { self.storage = storage }

    func attachments(of node: Node.ID, in space: Space.ID) async throws -> [AttachmentInfo] { [] }
    func attachFile(at url: URL, to node: Node.ID, in space: Space.ID) async throws -> AttachmentInfo { throw SpaceStoreError.unsupported("") }
    func attach(_ data: Data, named name: String, to node: Node.ID, in space: Space.ID) async throws -> AttachmentInfo { throw SpaceStoreError.unsupported("") }
    func removeAttachment(named name: String, from node: Node.ID, in space: Space.ID) async throws {
        removed.append((name, node))
        await storage.drop(name)
    }
    func attachmentURL(named name: String, of node: Node.ID, in space: Space.ID) async throws -> URL? { nil }
}

private func summary(total: Int64 = 3_000, warnAt: Int64 = 10 * 1024 * 1024 * 1024) -> StorageSummary {
    StorageSummary(
        total: total,
        kinds: StorageKinds(notes: 1_000, photos: 1_500, videos: 0, audio: 500, other: 0, deleted: 0),
        nodes: 3, files: 2, photos: 1, recordings: 1,
        sources: [StorageSource(actor: "photos-library", kind: "system", size: 1_500, files: 1)],
        largest: [
            StorageFile(node: "n1", title: "Trip", kind: "photo", name: "IMG_1.HEIC", size: 1_500, mediaType: "image/heic", addedBy: "photos-library"),
            StorageFile(node: "n2", title: "Talk", kind: "recording", name: "Talk.m4a", size: 500, mediaType: "audio/mp4", addedBy: "Ada"),
        ],
        limits: StorageLimits(maxFile: 100 * 1024 * 1024, maxOriginal: 4 * 1024 * 1024 * 1024, keepLargeOriginals: false, warnAt: warnAt),
        over: false
    )
}

@MainActor
struct SpaceStorageModelTests {
    @Test func loadsSortsAndRemovesFiles() async throws {
        let storage = FakeSpaceStorage(summary: summary())
        let attachments = FakeAttachmentRemover(storage: storage)
        let shown = ShownNodes()
        let model = SpaceStorageModel(space: "space-1", storage: storage, attachments: attachments) { await shown.add($0) }
        await model.load()
        #expect(model.summary?.total == 3_000)
        #expect(model.kinds.map(\.name) == ["Photos", "Notes", "Audio"])
        #expect(model.canRemove)

        let file = try #require(model.summary?.largest.first)
        await model.show(file)
        #expect(await shown.nodes == ["n1"])
        await model.remove(file)
        #expect(await attachments.removed.map(\.name) == ["IMG_1.HEIC"])
        #expect(model.summary?.largest.map(\.name) == ["Talk.m4a"])
        #expect(model.errorMessage == nil)
    }

    @Test func changesTheLimits() async throws {
        let storage = FakeSpaceStorage(summary: summary())
        let model = SpaceStorageModel(space: "space-1", storage: storage, attachments: nil)
        await model.load()
        #expect(!model.canRemove)
        await model.setKeepLargeOriginals(true)
        #expect(model.summary?.limits.keepLargeOriginals == true)
        await model.setWarnAt(0)
        #expect(model.summary?.limits.warnAt == 0)
        #expect(await storage.limitsSet.count == 2)
        #expect(SpaceStorageSection.warnOptions(current: 0).first?.title == "Never")
        #expect(SpaceStorageSection.warnOptions(current: 3).count == SpaceStorageModel.warnChoices.count + 1)
    }

    @Test func importsPastTheLimitAskFirst() async throws {
        let folder = try mediaFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let photo = folder.appending(path: "big.jpg")
        try Data(repeating: 1, count: 2_000).write(to: photo)
        let storage = FakeSpaceStorage(summary: summary(total: 3_000, warnAt: 4_000))
        let importer = MediaImporter(store: FakeMediaStore(), storage: storage, inspector: FakeInspector(), extractor: FakeExtractor(size: 1), limit: 100_000, scratch: folder.appending(path: "scratch"))
        let warning = try #require(await importer.largeImportWarning(for: [photo], in: "space-1"))
        #expect(warning.contains("past its limit"))
        let small = folder.appending(path: "small.jpg")
        try Data(repeating: 1, count: 10).write(to: small)
        #expect(await importer.largeImportWarning(for: [small], in: "space-1") == nil)
        // A store that cannot tell never warns.
        let plain = MediaImporter(store: FakeMediaStore(), limit: 100_000, scratch: folder.appending(path: "scratch"))
        #expect(await plain.largeImportWarning(for: [photo], in: "space-1") == nil)
    }
}

actor ShownNodes {
    var nodes: [Node.ID] = []
    func add(_ id: Node.ID) { nodes.append(id) }
}
