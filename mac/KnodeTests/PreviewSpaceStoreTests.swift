import Foundation
import Testing
@testable import Knode

/// A clock that moves one second forward every time it is read.
final class TestClock: @unchecked Sendable {
    private let lock = NSLock()
    private var current = Date(timeIntervalSince1970: 1_800_000_000)

    func now() -> Date {
        lock.withLock {
            current += 1
            return current
        }
    }
}

@Suite struct PreviewSpaceStoreTests {
    let clock = TestClock()
    let url = URL(filePath: "/tmp/Test.knode")

    func makeStore(examples: Bool = true) -> PreviewSpaceStore {
        let clock = clock
        return PreviewSpaceStore(seedExamples: examples, now: { clock.now() })
    }

    @Test func opensAKnodePackageWithLabelledExamples() async throws {
        let store = makeStore()
        let space = try await store.openSpace(at: url)
        #expect(space.name == "Test")
        #expect(space.url == url)

        let nodes = try await store.listNodes(in: space.id)
        #expect(nodes.count == 4)
        #expect(nodes.allSatisfy { $0.title.hasPrefix("Example: ") })
    }

    @Test func opensTheSameSpaceOnce() async throws {
        let store = makeStore()
        let first = try await store.openSpace(at: url)
        let second = try await store.openSpace(at: url)
        #expect(first.id == second.id)
    }

    @Test func refusesFilesThatAreNotSpaces() async {
        let store = makeStore()
        let url = URL(filePath: "/tmp/notes.txt")
        await #expect(throws: SpaceStoreError.notASpace(url)) {
            try await store.openSpace(at: url)
        }
    }

    @Test func createsAnEmptySpace() async throws {
        let store = makeStore()
        let space = try await store.createSpace(at: url, name: "Work")
        #expect(space.name == "Work")
        #expect(try await store.listNodes(in: space.id).isEmpty)
    }

    @Test func listsNewestFirst() async throws {
        let store = makeStore(examples: false)
        let space = try await store.createSpace(at: url, name: "Work")
        let first = try await store.createNode(in: space.id, title: "First", body: "")
        let second = try await store.createNode(in: space.id, title: "Second", body: "")
        #expect(try await store.listNodes(in: space.id).map(\.id) == [second.id, first.id])

        _ = try await store.updateNode(first.id, in: space.id, title: nil, body: "Changed")
        #expect(try await store.listNodes(in: space.id).map(\.id) == [first.id, second.id])
    }

    @Test func updatesOnlyTheGivenFields() async throws {
        let store = makeStore(examples: false)
        let space = try await store.createSpace(at: url, name: "Work")
        let node = try await store.createNode(in: space.id, title: "Title", body: "Body")

        let renamed = try await store.updateNode(node.id, in: space.id, title: "Renamed", body: nil)
        #expect(renamed.title == "Renamed")
        #expect(renamed.body == "Body")
        #expect(renamed.updatedAt > node.updatedAt)
        #expect(try await store.node(node.id, in: space.id) == renamed)
    }

    @Test func searchesTitlesBeforeBodies() async throws {
        let store = makeStore(examples: false)
        let space = try await store.createSpace(at: url, name: "Work")
        let inBody = try await store.createNode(in: space.id, title: "Notes", body: "About sqlite and search")
        let inTitle = try await store.createNode(in: space.id, title: "SQLite", body: "FTS5")
        _ = try await store.createNode(in: space.id, title: "Other", body: "Nothing here")

        #expect(try await store.search("sqlite", in: space.id).map(\.id) == [inTitle.id, inBody.id])
        #expect(try await store.search("sqlite search", in: space.id).map(\.id) == [inBody.id])
        #expect(try await store.search("missing", in: space.id).isEmpty)
        #expect(try await store.search("  ", in: space.id).count == 3)
    }

    @Test func reportsUnknownSpacesAndNodes() async throws {
        let store = makeStore()
        await #expect(throws: SpaceStoreError.spaceNotFound("nope")) {
            try await store.listNodes(in: "nope")
        }
        let space = try await store.openSpace(at: url)
        await #expect(throws: SpaceStoreError.nodeNotFound("nope")) {
            try await store.node("nope", in: space.id)
        }
    }

    @Test func idsAreTimeOrdered() {
        let early = ULID.make(at: Date(timeIntervalSince1970: 1_000))
        let late = ULID.make(at: Date(timeIntervalSince1970: 2_000))
        #expect(early.count == 26)
        #expect(early < late)
    }

    @Test func excerptSkipsMarkdownMarkers() {
        #expect(NodeSummary.excerpt(of: "# Title\n\nText") == "Title")
        #expect(NodeSummary.excerpt(of: "```\ncode\n```") == "code")
        #expect(NodeSummary.excerpt(of: "> quoted") == "quoted")
        #expect(NodeSummary.excerpt(of: "") == "")
    }
}
