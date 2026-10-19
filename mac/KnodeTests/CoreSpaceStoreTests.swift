import Foundation
import Testing
@testable import Knode

/// `CoreSpaceStore` against real `.knode` packages in a temporary folder.
@Suite final class CoreSpaceStoreTests {
    let folder: URL

    init() throws {
        folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    }

    deinit {
        try? FileManager.default.removeItem(at: folder)
    }

    var spaceURL: URL { folder.appending(path: "Test.knode", directoryHint: .isDirectory) }

    @Test func createsAPackageOnDisk() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: spaceURL, name: "Test")
        #expect(space.name == "Test")
        #expect(space.id.count == 26)
        #expect(space.url == spaceURL.standardizedFileURL)
        #expect(FileManager.default.fileExists(atPath: spaceURL.appending(path: "manifest.json").path(percentEncoded: false)))
        #expect(try await store.listNodes(in: space.id).isEmpty)
    }

    @Test func writesReopensAndSearches() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: spaceURL, name: "Test")
        let garden = try await store.createNode(in: space.id, title: "Garden", body: "# Plan\n\nPlant tomatoes in May.")
        let kitchen = try await store.createNode(in: space.id, title: "Kitchen", body: "Nothing here.")
        // Dates are kept to the millisecond: let the update come later than both.
        try await Task.sleep(for: .milliseconds(5))
        let renamed = try await store.updateNode(garden.id, in: space.id, title: "Garden 2026", body: nil)
        #expect(renamed.title == "Garden 2026")
        #expect(renamed.body == garden.body)

        // A new store reads what the first one wrote.
        let reopened = CoreSpaceStore(person: "Ada")
        let again = try await reopened.openSpace(at: spaceURL)
        #expect(again.id == space.id)
        let list = try await reopened.listNodes(in: again.id)
        #expect(list.map(\.id) == [garden.id, kitchen.id])
        #expect(list.first?.excerpt == "Plan")
        #expect(try await reopened.node(garden.id, in: again.id).title == "Garden 2026")

        let hits = try await reopened.search("tomato", in: again.id)
        #expect(hits.map(\.id) == [garden.id])
        #expect(hits.first?.excerpt.contains("tomatoes") == true)
        #expect(hits.first?.excerpt.contains("**") == false)
    }

    @Test func aBlankTitleKeepsTheTitle() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: spaceURL, name: "Test")
        let note = try await store.createNode(in: space.id, title: "Title", body: "")
        let updated = try await store.updateNode(note.id, in: space.id, title: "  ", body: "Body")
        #expect(updated.title == "Title")
        #expect(updated.body == "Body")
    }

    @Test func opensTheSameSpaceOnce() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let created = try await store.createSpace(at: spaceURL, name: "Test")
        let opened = try await store.openSpace(at: spaceURL)
        #expect(opened == created)
    }

    @Test func reportsErrorsAsSpaceStoreErrors() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let missing = folder.appending(path: "Missing.knode")
        await #expect(throws: SpaceStoreError.notASpace(missing)) {
            try await store.openSpace(at: missing)
        }
        let space = try await store.createSpace(at: spaceURL, name: "Test")
        await #expect(throws: SpaceStoreError.alreadyExists(spaceURL.standardizedFileURL)) {
            try await store.createSpace(at: spaceURL, name: "Again")
        }
        await #expect(throws: SpaceStoreError.nodeNotFound("01JABCDEFGHJKMNPQRSTVWXYZ0")) {
            try await store.node("01JABCDEFGHJKMNPQRSTVWXYZ0", in: space.id)
        }
        await #expect(throws: SpaceStoreError.spaceNotFound("nope")) {
            try await store.listNodes(in: "nope")
        }
    }

    @Test func runsOffTheMainThread() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: spaceURL, name: "Test")
        try await withThrowingTaskGroup(of: Void.self) { group in
            for index in 0 ..< 8 {
                group.addTask { _ = try await store.createNode(in: space.id, title: "Note \(index)", body: "") }
            }
            try await group.waitForAll()
        }
        #expect(try await store.listNodes(in: space.id).count == 8)
    }
}

@Suite struct RecentSpacesTests {
    @Test func remembersSpacesNewestFirst() throws {
        let suite = "KnodeTests-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let folder = FileManager.default.temporaryDirectory.appending(path: suite, directoryHint: .isDirectory)
        let first = folder.appending(path: "First.knode", directoryHint: .isDirectory)
        let second = folder.appending(path: "Second.knode", directoryHint: .isDirectory)
        for url in [first, second] {
            try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        }
        defer { try? FileManager.default.removeItem(at: folder) }

        let recents = RecentSpaces(defaults: defaults)
        recents.add(first)
        recents.add(second)
        recents.add(first)
        #expect(recents.urls.map(\.lastPathComponent) == ["First.knode", "Second.knode"])

        try FileManager.default.removeItem(at: second)
        #expect(recents.urls.map(\.lastPathComponent) == ["First.knode"])

        recents.clear()
        #expect(recents.urls.isEmpty)
    }
}
