import Foundation
import Synchronization
import Testing
@testable import Knode

/// A Photos library in memory: tests never touch PhotoKit or ask for access.
final class FakePhotoLibrary: PhotoLibrary {
    struct State {
        var access = PhotoLibraryAccess.authorized
        var grant = true
        var requests = 0
        var albums: [PhotoAlbum] = []
        /// The assets of each album.
        var contents: [String: [PhotoAsset]] = [:]
        /// Assets whose original is only in iCloud.
        var inCloud: Set<String> = []
        /// The bytes each export writes, by asset; 10 when absent.
        var sizes: [String: Int] = [:]
        var token: String? = "t1"
        /// What changed since each token; a token absent here cannot tell (read everything).
        var changesSince: [String: PhotoLibraryChanges] = [:]
        var exported: [(id: String, network: Bool)] = []
        /// How many times the albums were listed.
        var listed = 0
    }

    let state = Mutex(State())

    var access: PhotoLibraryAccess { state.withLock { $0.access } }

    func requestAccess() async -> PhotoLibraryAccess {
        state.withLock {
            $0.requests += 1
            $0.access = $0.grant ? .authorized : .denied
            return $0.access
        }
    }

    func albums() -> [PhotoAlbum] { state.withLock { $0.albums } }

    func assets(inAlbums albums: Set<String>) -> PhotoAlbumContents {
        state.withLock { s in
            s.listed += 1
            var seen = Set<String>()
            let found = s.albums.filter { albums.contains($0.id) }
            let assets = found.flatMap { s.contents[$0.id] ?? [] }.filter { seen.insert($0.id).inserted }
            return PhotoAlbumContents(assets: assets, found: Set(found.map(\.id)))
        }
    }

    func changeToken() -> String? { state.withLock { $0.token } }

    func changes(since token: String) -> PhotoLibraryChanges? { state.withLock { $0.changesSince[token] } }

    func changes() -> AsyncStream<Void> { AsyncStream { $0.finish() } }

    func exportOriginal(of asset: PhotoAsset, into folder: URL, allowNetwork: Bool) async throws -> ExportedOriginal {
        let (cloud, size) = state.withLock { s in
            s.exported.append((asset.id, allowNetwork))
            return (s.inCloud.contains(asset.id), s.sizes[asset.id] ?? 10)
        }
        if cloud && !allowNetwork { throw PhotoExportError.inCloud }
        let url = folder.appending(path: asset.originalName)
        try Data(repeating: 1, count: size).write(to: url)
        return ExportedOriginal(file: url, content: try ContentHasher.content(of: url))
    }
}

/// Knode Service's Photos library methods in memory.
actor FakePhotosLibraryClient: PhotosLibraryClient {
    struct Synced: Sendable {
        var space: URL
        var present: [String]?
        var failed: [PhotosLibraryFailure]
        var token: String?
        var added: Int
        var waiting: Int
        var held: Int
    }

    var spaces: [URL] = []
    var plans: [URL: PhotosLibraryPlan] = [:]
    var holding: [URL: Set<String>] = [:]
    var added: [(space: URL, export: PhotosLibraryExport, fileExisted: Bool, folder: [String])] = []
    /// The identifiers each `known` was asked about.
    var asked: [[String]] = []
    var synced: [Synced] = []
    var failures: [(space: URL, kind: String)] = []
    var reported: [[PhotoAlbum]] = []
    /// `add` answers no room after this many assets.
    var roomFor = Int.max
    var icloud = false

    func set(spaces: [URL]) { self.spaces = spaces }
    func set(plan: PhotosLibraryPlan, for space: URL) { plans[space] = plan }
    func set(roomFor count: Int) { roomFor = count }

    func connectedSpaces() async throws -> [URL] { spaces }

    func plan(in space: URL) async throws -> PhotosLibraryPlan {
        guard let plan = plans[space] else { throw ServiceError.remote(code: -32602, kind: nil, message: "not connected") }
        return plan
    }

    func reportAlbums(_ albums: [PhotoAlbum]) async throws { reported.append(albums) }

    func known(_ ids: [String], in space: URL) async throws -> Set<String> {
        asked.append(ids)
        return Set(ids).intersection(holding[space] ?? [])
    }

    func add(_ export: PhotosLibraryExport, in space: URL) async throws -> PhotosLibraryAdded {
        let existed = FileManager.default.fileExists(atPath: export.file.path(percentEncoded: false))
        let folder = (try? FileManager.default.contentsOfDirectory(atPath: export.file.deletingLastPathComponent().path(percentEncoded: false))) ?? []
        added.append((space, export, existed, folder.sorted()))
        let created = holding[space, default: []].insert(export.asset.id).inserted
        return PhotosLibraryAdded(node: .init(type: export.asset.kind == .photo ? "photo" : "recording", id: "n-\(export.asset.id)"),
                                  created: created, room: added.count < roomFor)
    }

    func sync(present: [String]?, failed: [PhotosLibraryFailure], albums: [PhotoAlbum], token: String?, added: Int, waiting: Int, held: Int, in space: URL) async throws {
        synced.append(Synced(space: space, present: present, failed: failed, token: token, added: added, waiting: waiting, held: held))
        // As the core: an asset that failed for good is known from now on.
        holding[space, default: []].formUnion(failed.map(\.id))
        plans[space]?.token = token
        plans[space]?.nextRun = Int64.max
    }

    func fail(kind: String, message: String, in space: URL) async throws { failures.append((space, kind)) }

    func options(in space: URL) async throws -> PhotosLibraryOptions { PhotosLibraryOptions(icloud: icloud, elsewhere: false) }

    func setICloud(_ on: Bool, in space: URL) async throws -> PhotosLibraryOptions {
        icloud = on
        return PhotosLibraryOptions(icloud: on, elsewhere: false)
    }
}

private let space = URL(filePath: "/tmp/Fake.knode", directoryHint: .isDirectory)

private func photo(_ id: String, at seconds: TimeInterval = 1_795_343_400) -> PhotoAsset {
    PhotoAsset(id: id, kind: .photo, createdAt: Date(timeIntervalSince1970: seconds), duration: 0, originalName: "\(id).HEIC",
               latitude: 40.4, longitude: -3.7, width: 4, height: 3)
}

private func video(_ id: String) -> PhotoAsset {
    PhotoAsset(id: id, kind: .video, createdAt: Date(timeIntervalSince1970: 1_795_343_400), duration: 61.5, originalName: "\(id).MOV")
}

private func plan(albums: [String], since: Int64? = nil, icloud: Bool = false, room: Bool = true) -> PhotosLibraryPlan {
    PhotosLibraryPlan(on: true, library: "LIB", albums: albums.map { .init(id: $0, name: $0) }, elsewhere: false, since: since, icloud: icloud,
                      room: room, token: nil, nextRun: nil)
}

@MainActor
struct PhotosLibrarySyncerTests {
    private func setUp(_ configure: (inout FakePhotoLibrary.State) -> Void) -> (FakePhotoLibrary, FakePhotosLibraryClient, URL) {
        let library = FakePhotoLibrary()
        library.state.withLock { s in
            s.albums = [PhotoAlbum(id: "trip", name: "Trip"), PhotoAlbum(id: "work", name: "Work")]
            configure(&s)
        }
        let scratch = FileManager.default.temporaryDirectory.appending(path: "KnodePhotosLibrary-\(UUID().uuidString)", directoryHint: .isDirectory)
        return (library, FakePhotosLibraryClient(), scratch)
    }

    @Test func readsOnlyPickedAlbumsAndAddsEachAssetOnce() async throws {
        let (library, client, scratch) = setUp { s in
            s.contents = ["trip": [photo("A"), photo("B")], "work": [photo("W")]]
        }
        defer { try? FileManager.default.removeItem(at: scratch) }
        await client.set(spaces: [space])
        await client.set(plan: plan(albums: ["trip"]), for: space)
        let syncer = PhotosLibrarySyncer(library: library, client: client, extractor: FakeExtractor(size: 1), limit: 100, scratch: scratch)

        await syncer.runOnce()
        let added = await client.added
        #expect(added.map(\.export.asset.id) == ["A", "B"])
        #expect(added.allSatisfy { $0.fileExisted })
        let first = try #require(added.first?.export)
        #expect(first.metadata.takenAt == Date(timeIntervalSince1970: 1_795_343_400))
        #expect(first.metadata.latitude == 40.4)
        #expect(first.metadata.width == 4)
        let synced = try #require(await client.synced.last)
        #expect(synced.present == ["A", "B"])
        #expect(synced.added == 2)
        #expect(synced.token == "t1")
        #expect(await client.reported.last?.map(\.id) == ["trip", "work"])
        // Exports are removed once handed over.
        #expect((try? FileManager.default.contentsOfDirectory(atPath: scratch.path(percentEncoded: false)))?.isEmpty ?? true)

        // Nothing changed: the next run reads nothing.
        await syncer.runOnce()
        #expect(await client.synced.count == 1)

        // The library changed: only the new asset is exported, and the one gone is no longer present.
        library.state.withLock { s in
            s.contents["trip"] = [photo("B"), photo("C")]
            s.token = "t2"
        }
        await syncer.libraryDidChange()
        await syncer.runOnce()
        #expect(await client.added.map(\.export.asset.id) == ["A", "B", "C"])
        #expect(library.state.withLock { $0.exported.map(\.id) } == ["A", "B", "C"])
        let last = try #require(await client.synced.last)
        #expect(last.present == ["B", "C"])
        #expect(last.added == 1)
        #expect(last.token == "t2")
    }

    @Test func iCloudOriginalsWaitUnlessAllowed() async throws {
        let (library, client, scratch) = setUp { s in
            s.contents = ["trip": [photo("Local"), photo("Cloud")]]
            s.inCloud = ["Cloud"]
        }
        defer { try? FileManager.default.removeItem(at: scratch) }
        await client.set(spaces: [space])
        await client.set(plan: plan(albums: ["trip"]), for: space)
        let syncer = PhotosLibrarySyncer(library: library, client: client, limit: 100, scratch: scratch)
        await syncer.runOnce()
        #expect(await client.added.map(\.export.asset.id) == ["Local"])
        #expect(await client.synced.last?.waiting == 1)
        #expect(library.state.withLock { $0.exported.allSatisfy { !$0.network } })

        await client.set(plan: plan(albums: ["trip"], icloud: true), for: space)
        await syncer.runOnce()
        #expect(await client.added.map(\.export.asset.id) == ["Local", "Cloud"])
        #expect(await client.synced.last?.waiting == 0)
        #expect(library.state.withLock { $0.exported.last?.network } == true)
    }

    @Test func fromTodayAndTheSizeLimitHoldBack() async throws {
        let (library, client, scratch) = setUp { s in
            s.contents = ["trip": [photo("Old", at: 1_000), photo("N1"), photo("N2"), photo("N3")]]
        }
        defer { try? FileManager.default.removeItem(at: scratch) }
        await client.set(spaces: [space])
        await client.set(plan: plan(albums: ["trip"], since: 1_700_000_000_000), for: space)
        await client.set(roomFor: 1)
        let syncer = PhotosLibrarySyncer(library: library, client: client, limit: 100, scratch: scratch)
        await syncer.runOnce()
        // Old is left out; after N1 the space is full, so N2 and N3 wait.
        #expect(await client.added.map(\.export.asset.id) == ["N1"])
        let synced = try #require(await client.synced.last)
        #expect(synced.held == 2)
        #expect(synced.present == ["Old", "N1", "N2", "N3"])

        // A space already past its limit adds nothing.
        await client.set(plan: plan(albums: ["trip"], room: false), for: space)
        await syncer.runOnce()
        #expect(await client.added.count == 1)
        #expect(await client.synced.last?.held == 3)
    }

    @Test func readsOnlyWhatChangedOnceReadInFull() async throws {
        let (library, client, scratch) = setUp { s in
            s.contents = ["trip": [photo("A"), photo("B")], "work": [photo("W")]]
        }
        defer { try? FileManager.default.removeItem(at: scratch) }
        await client.set(spaces: [space])
        await client.set(plan: plan(albums: ["trip"]), for: space)
        let syncer = PhotosLibrarySyncer(library: library, client: client, limit: 100, scratch: scratch)
        await syncer.runOnce()
        #expect(await client.asked == [["A", "B"]])

        // No asset or album changed: nothing is listed, exported or marked.
        library.state.withLock { s in
            s.token = "t1b"
            s.changesSince["t1"] = PhotoLibraryChanges()
        }
        await syncer.libraryDidChange()
        await syncer.runOnce()
        #expect(library.state.withLock { $0.listed } == 1)
        let quiet = try #require(await client.synced.last)
        #expect(quiet.present == nil)
        #expect(quiet.token == "t1b")

        // Only an asset outside the albums changed: nothing is asked about or exported.
        library.state.withLock { s in
            s.token = "t2"
            s.changesSince["t1b"] = PhotoLibraryChanges(assets: ["W"])
        }
        await syncer.libraryDidChange()
        await syncer.runOnce()
        #expect(await client.asked.count == 1)
        #expect(await client.synced.last?.token == "t2")

        // C is new and B changed: only they are asked about; the albums are listed for what is present.
        library.state.withLock { s in
            s.contents["trip"] = [photo("A"), photo("B"), photo("C")]
            s.token = "t3"
            s.changesSince["t2"] = PhotoLibraryChanges(assets: ["B", "C"])
        }
        await syncer.libraryDidChange()
        await syncer.runOnce()
        #expect(await client.asked.last == ["B", "C"])
        #expect(await client.added.map(\.export.asset.id) == ["A", "B", "C"])
        #expect(await client.synced.last?.present == ["A", "B", "C"])

        // A token that cannot tell any more reads everything.
        library.state.withLock { s in s.token = "t4" }
        await syncer.libraryDidChange()
        await syncer.runOnce()
        #expect(await client.asked.last == ["A", "B", "C"])
    }

    @Test func anAlbumNotFoundMarksNothing() async throws {
        let (library, client, scratch) = setUp { s in
            s.contents = ["trip": [photo("A")]]
        }
        defer { try? FileManager.default.removeItem(at: scratch) }
        await client.set(spaces: [space])
        // "gone" was picked (on this Mac, or its identifier is another library's): it is not found.
        await client.set(plan: plan(albums: ["trip", "gone"]), for: space)
        let syncer = PhotosLibrarySyncer(library: library, client: client, limit: 100, scratch: scratch)
        await syncer.runOnce()
        #expect(await client.added.map(\.export.asset.id) == ["A"])
        let synced = try #require(await client.synced.last)
        #expect(synced.present == nil)
    }

    @Test func scratchLeftByACrashIsCleared() async throws {
        let (library, client, scratch) = setUp { _ in }
        defer { try? FileManager.default.removeItem(at: scratch) }
        let stale = scratch.appending(path: "old-export", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: stale, withIntermediateDirectories: true)
        try Data([1]).write(to: stale.appending(path: "IMG.HEIC"))
        let syncer = PhotosLibrarySyncer(library: library, client: client, limit: 100, scratch: scratch)
        await syncer.runOnce()
        #expect(!FileManager.default.fileExists(atPath: stale.path(percentEncoded: false)))
    }

    @Test func assetsThatFailForGoodAreReportedAndNotExportedAgain() async throws {
        let (library, client, scratch) = setUp { s in
            s.contents = ["trip": [video("Huge")]]
            s.sizes = ["Huge": 500]
        }
        defer { try? FileManager.default.removeItem(at: scratch) }
        await client.set(spaces: [space])
        await client.set(plan: plan(albums: ["trip"]), for: space)
        // Even the audio is over the limit.
        let syncer = PhotosLibrarySyncer(library: library, client: client, extractor: FakeExtractor(size: 200), limit: 100, scratch: scratch)
        await syncer.runOnce()
        #expect(await client.added.isEmpty)
        #expect(await client.synced.last?.failed.map(\.id) == ["Huge"])
        await client.set(plan: plan(albums: ["trip"]), for: space)
        await syncer.runOnce()
        #expect(library.state.withLock { $0.exported.map(\.id) } == ["Huge"])
    }

    @Test func largeVideosKeepTheirAudioAndHandTheOriginalToTheCore() async throws {
        let (library, client, scratch) = setUp { s in
            s.contents = ["trip": [video("Small"), video("Big")]]
            s.sizes = ["Big": 500]
        }
        defer { try? FileManager.default.removeItem(at: scratch) }
        await client.set(spaces: [space])
        await client.set(plan: plan(albums: ["trip"]), for: space)
        let extractor = FakeExtractor(size: 40)
        let syncer = PhotosLibrarySyncer(library: library, client: client, extractor: extractor, limit: 100, scratch: scratch)
        await syncer.runOnce()
        let added = await client.added.map(\.export)
        #expect(added.count == 2)
        #expect(added[0].file.lastPathComponent == "Small.MOV")
        #expect(added[0].original == nil)
        #expect(added[1].file.lastPathComponent == "Big.m4a")
        #expect(added[1].original == "Big.MOV")
        // The original is deleted as soon as its audio is out; its hash goes to the core.
        let folders = await client.added.map(\.folder)
        #expect(folders[1] == ["Big.m4a"])
        #expect(added[1].content == (try ContentHasher.content(of: { () throws -> URL in
            let url = scratch.appending(path: "hash-check.bin")
            try Data(repeating: 1, count: 500).write(to: url)
            return url
        }())))
        #expect(extractor.calls.withLock { $0 } == ["Big.MOV"])
        let json = PhotosLibraryWire.asset(added[1])
        guard case .object(let object) = json else { Issue.record("not an object"); return }
        #expect(object["kind"] == .string("video"))
        #expect(object["duration_ms"] == .int(61_500))
        #expect(object["original"] == .string("Big.MOV"))
        #expect(object["title"] == .string("Big"))
        #expect(object["content"] == added[1].content.map(JSONValue.string))
        #expect(object["original_path"] == nil)
    }

    @Test func withoutAccessEachSpaceIsToldOnce() async throws {
        let (library, client, scratch) = setUp { s in s.access = .denied }
        await client.set(spaces: [space])
        await client.set(plan: plan(albums: ["trip"]), for: space)
        let syncer = PhotosLibrarySyncer(library: library, client: client, limit: 100, scratch: scratch)
        await syncer.runOnce()
        await syncer.runOnce()
        #expect(await client.failures.map(\.kind) == ["permission"])
        #expect(await client.added.isEmpty)
        #expect(library.state.withLock { $0.requests } == 0)
    }
}

struct PhotosLibraryAccessSignInTests {
    @Test func asksForAccessOnlyWhenConnecting() async throws {
        let library = FakePhotoLibrary()
        library.state.withLock { s in
            s.access = .notDetermined
            s.albums = [PhotoAlbum(id: "trip", name: "Trip")]
        }
        let client = FakePhotosLibraryClient()
        let signIn = PhotosLibraryAccessSignIn(library: library, client: client)
        let step = try #require(try await signIn.start(source: PhotosLibraryWire.source, space: space))
        #expect(step.id == "photos-access")
        #expect(library.state.withLock { $0.requests } == 0)
        let next = try await signIn.answer(step, with: [:], source: PhotosLibraryWire.source, space: space)
        #expect(next == nil)
        #expect(library.state.withLock { $0.requests } == 1)
        #expect(await client.reported.last?.map(\.id) == ["trip"])
    }

    @Test func aDeniedAccessSaysWhereToAllowIt() async throws {
        let library = FakePhotoLibrary()
        library.state.withLock { s in
            s.access = .notDetermined
            s.grant = false
        }
        let opened = Mutex<[URL]>([])
        let signIn = PhotosLibraryAccessSignIn(library: library, client: FakePhotosLibraryClient()) { url in opened.withLock { $0.append(url) } }
        let step = try #require(try await signIn.start(source: PhotosLibraryWire.source, space: space))
        await #expect(throws: PhotosLibraryAccessSignIn.Failure.self) {
            _ = try await signIn.answer(step, with: [:], source: PhotosLibraryWire.source, space: space)
        }
        let denied = try #require(try await signIn.start(source: PhotosLibraryWire.source, space: space))
        #expect(denied.id == "photos-denied")
        await #expect(throws: PhotosLibraryAccessSignIn.Failure.self) {
            _ = try await signIn.answer(denied, with: [:], source: PhotosLibraryWire.source, space: space)
        }
        #expect(opened.withLock { $0 } == [PhotosLibraryAccessSignIn.settingsURL])
    }
}
