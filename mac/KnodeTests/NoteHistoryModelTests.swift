import Foundation
import Testing
@testable import Knode

@MainActor
@Suite struct NoteHistoryModelTests {
    let url = URL(filePath: "/tmp/History.knode")

    /// A note written three times: "One", then "Two" and "Three".
    func makeHistory() async throws -> (NoteHistoryModel, PreviewSpaceStore, Space.ID, Node.ID) {
        let clock = TestClock()
        let store = PreviewSpaceStore(seedExamples: false, person: "Ada", now: { clock.now() })
        let space = try await store.openSpace(at: url)
        let node = try await store.createNode(in: space.id, title: "Garden", body: "One")
        _ = try await store.updateNode(node.id, in: space.id, title: nil, body: "One\nTwo")
        _ = try await store.updateNode(node.id, in: space.id, title: nil, body: "One\nTwo\nThree")
        let history = NoteHistoryModel(nodeID: node.id, spaceID: space.id, store: store)
        history.currentTitle = { "Garden" }
        history.currentBody = { "One\nTwo\nThree" }
        return (history, store, space.id, node.id)
    }

    @Test func listsVersionsNewestFirstWithWhoAndWhat() async throws {
        let (history, _, _, _) = try await makeHistory()
        await history.load()
        #expect(history.versions.map(\.body) == ["One\nTwo\nThree", "One\nTwo", "One"])
        #expect(history.versions.allSatisfy { $0.author == Author(kind: .person, name: "Ada") })
        #expect(history.versions.last?.operation == .create)
        #expect(history.latest == history.versions.first)
        let newest = try #require(history.versions.first)
        #expect(history.changeSummary(newest) == LineDiff.Summary(added: 1, removed: 0))
        #expect(history.isCurrent(newest))
        #expect(history.canUndo(newest))
        #expect(!history.canUndo(try #require(history.versions.last)))
    }

    @Test func comparesAVersionWithTheNoteNow() async throws {
        let (history, _, _, _) = try await makeHistory()
        await history.load()
        let first = try #require(history.versions.last)
        let lines = history.diff(first)
        #expect(lines.map(\.kind) == [.same, .added, .added])
        #expect(lines.map(\.text) == ["One", "Two", "Three"])
        #expect(!history.isCurrent(first))
    }

    @Test func restoresAVersionAsANewOne() async throws {
        let (history, store, space, node) = try await makeHistory()
        var saved = 0, changed = 0
        history.willChange = { saved += 1 }
        history.didChange = { changed += 1 }
        await history.load()
        let first = try #require(history.versions.last)
        history.selectedVersionID = first.id

        await history.restore(first)
        #expect(try await store.node(node, in: space).body == "One")
        #expect(history.versions.count == 4)
        #expect(history.versions.first?.restoredFrom == first.seq)
        #expect(history.selectedVersionID == nil)
        #expect((saved, changed) == (1, 1))
    }

    @Test func undoAsksBeforeOverwritingLaterChanges() async throws {
        let (history, store, space, node) = try await makeHistory()
        await history.load()
        let middle = history.versions[1]

        // A later change touched the note: the store refuses and the model asks.
        await history.undo(middle)
        #expect(history.conflict?.change == middle.changeID)
        #expect(history.conflict?.message.isEmpty == false)
        #expect(try await store.node(node, in: space).body == "One\nTwo\nThree")

        await history.confirmForcedUndo(try #require(history.conflict))
        #expect(history.conflict == nil)
        #expect(try await store.node(node, in: space).body == "One")
        #expect(history.errorMessage == nil)
    }

    @Test func undoesTheLastChangeAtOnce() async throws {
        let (history, store, space, node) = try await makeHistory()
        await history.load()
        await history.undo(try #require(history.versions.first))
        #expect(history.conflict == nil)
        #expect(try await store.node(node, in: space).body == "One\nTwo")

        // A change is undone once.
        let undone = try #require(history.versions.dropFirst().first { $0.body == "One\nTwo\nThree" })
        await history.undo(undone)
        #expect(history.errorMessage != nil)
    }
}

@Suite struct LineDiffTests {
    @Test func marksRemovedAndAddedLines() {
        let lines = LineDiff.lines(from: "a\nb\nc", to: "a\nB\nc\nd")
        #expect(lines.map(\.kind) == [.same, .removed, .added, .same, .added])
        #expect(lines.map(\.text) == ["a", "b", "B", "c", "d"])
        #expect(lines[1].old == 2 && lines[1].new == nil)
        #expect(lines[2].old == nil && lines[2].new == 2)
        #expect(lines[3].old == 3 && lines[3].new == 3)
        #expect(LineDiff.summary(from: "a\nb\nc", to: "a\nB\nc\nd") == LineDiff.Summary(added: 2, removed: 1))
    }

    @Test func emptyTextsHaveNoLines() {
        #expect(LineDiff.lines(from: "", to: "").isEmpty)
        #expect(LineDiff.lines(from: "", to: "x").map(\.kind) == [.added])
        #expect(LineDiff.lines(from: "x", to: "").map(\.kind) == [.removed])
    }
}

@Suite struct SpaceIconTests {
    @Test func acceptsOneEmojiOnly() throws {
        #expect(SpaceIcon.isEmoji("🌱"))
        #expect(SpaceIcon.isEmoji("✈️"))
        #expect(SpaceIcon.isEmoji("👩🏽‍💻"))
        #expect(!SpaceIcon.isEmoji("a"))
        #expect(!SpaceIcon.isEmoji("1"))
        #expect(!SpaceIcon.isEmoji("🌱🌱"))
        #expect(try SpaceIcon.normalized("  🌱 ") == "🌱")
        #expect(try SpaceIcon.normalized("  ") == nil)
        #expect(throws: SpaceStoreError.self) { try SpaceIcon.normalized("Work") }
    }
}
