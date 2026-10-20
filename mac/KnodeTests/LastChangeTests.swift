import Foundation
import Testing
@testable import Knode

/// "Undo Last Change" and "Redo Last Change": which change they pick, and what they do to a note.
@MainActor
@Suite struct LastChangeTests {
    let ada = Author(kind: .person, name: "Ada")
    let claude = Author(kind: .agent, name: "claude-code")

    func change(_ id: Int64, _ action: String, by author: Author? = nil, undoes: Int64? = nil) -> ChangeRecord {
        ChangeRecord(id: id, changedAt: Date(timeIntervalSince1970: Double(id)), author: author ?? ada, action: action, subject: "N", undoes: undoes)
    }

    @Test func undoPicksThePersonsNewestEditThatIsNotUndone() {
        let changes = [
            change(5, ChangeRecord.Action.update, by: claude),
            change(4, ChangeRecord.Action.undo, undoes: 3),
            change(3, ChangeRecord.Action.update),
            change(2, ChangeRecord.Action.restore),
            change(1, ChangeRecord.Action.create),
        ]
        // The agent's change is never picked, and change 3 is undone already.
        #expect(LastChange.undoTarget(in: changes, by: "Ada")?.id == 2)
        #expect(LastChange.undoTarget(in: changes, by: "Grace") == nil)
        // Creating the note is never undone this way.
        #expect(LastChange.undoTarget(in: [change(1, ChangeRecord.Action.create)], by: "Ada") == nil)
    }

    @Test func redoUndoesTheNewestUndoUntilTheNextEdit() {
        // Edits 1 and 2, both undone (3 then 4), then 4 redone by 5.
        var changes = [
            change(5, ChangeRecord.Action.undo, undoes: 4),
            change(4, ChangeRecord.Action.undo, undoes: 1),
            change(3, ChangeRecord.Action.undo, undoes: 2),
            change(2, ChangeRecord.Action.update),
            change(1, ChangeRecord.Action.update),
        ]
        // The undo of 2 is next to redo; 1 is no longer undone, so undo takes it again.
        #expect(LastChange.redoTarget(in: changes, by: "Ada")?.id == 3)
        #expect(LastChange.undoTarget(in: changes, by: "Ada")?.id == 1)

        // A new edit clears what can be redone, as in any editor.
        changes.insert(change(6, ChangeRecord.Action.update), at: 0)
        #expect(LastChange.redoTarget(in: changes, by: "Ada") == nil)
        #expect(LastChange.undoTarget(in: changes, by: "Ada")?.id == 6)

        #expect(LastChange.redoTarget(in: [change(1, ChangeRecord.Action.update)], by: "Ada") == nil)
    }

    @Test func ownUndoStepsAreNotAConflict() {
        let changes = [
            change(5, ChangeRecord.Action.update, by: claude),
            change(4, ChangeRecord.Action.undo, undoes: 3),
            change(3, ChangeRecord.Action.update),
            change(2, ChangeRecord.Action.update),
        ]
        #expect(LastChange.areOwnUndoSteps([3, 4], in: changes, by: "Ada"))
        // Someone else's change, an edit that is not undone, or a change not read: ask.
        #expect(!LastChange.areOwnUndoSteps([3, 4, 5], in: changes, by: "Ada"))
        #expect(!LastChange.areOwnUndoSteps([2], in: changes, by: "Ada"))
        #expect(!LastChange.areOwnUndoSteps([99], in: changes, by: "Ada"))
        #expect(!LastChange.areOwnUndoSteps([], in: changes, by: "Ada"))
    }

    /// The same steps against the real core, which keeps the changes and decides what is undone.
    @Test func undoAndRedoFollowTheHistoryInTheCore() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: folder.appending(path: "Undo.knode", directoryHint: .isDirectory), name: "Undo")
        let node = try await store.createNode(in: space.id, title: "Plan", body: "One")
        _ = try await store.updateNode(node.id, in: space.id, title: nil, body: "Two")
        _ = try await store.updateNode(node.id, in: space.id, title: nil, body: "Three")
        let history = NoteHistoryModel(nodeID: node.id, spaceID: space.id, store: store)

        #expect(await history.undoLastChange())
        #expect(try await store.node(node.id, in: space.id).body == "Two")
        #expect(await history.undoLastChange())
        #expect(try await store.node(node.id, in: space.id).body == "One")
        // Nothing left of Ada's but the creation, which is not undone this way.
        #expect(await !history.undoLastChange())

        #expect(await history.redoLastChange())
        #expect(try await store.node(node.id, in: space.id).body == "Two")
        #expect(await history.redoLastChange())
        #expect(try await store.node(node.id, in: space.id).body == "Three")
        #expect(await !history.redoLastChange())
        #expect(history.errorMessage == nil)
    }

    @MainActor
    @Test func undoSavesTheEditorFirstAndAsksBeforeOverwritingAnAgent() async throws {
        let store = PreviewSpaceStore(seedExamples: false, person: "Ada")
        let space = try await store.openSpace(at: URL(filePath: "/tmp/Last.knode"))
        let node = try await store.createNode(in: space.id, title: "Plan", body: "One")
        let model = AppModel(store: store, saveDelay: .seconds(60))
        await model.openSpace(at: URL(filePath: "/tmp/Last.knode"))
        await model.openNode(node.id)
        let editor = try #require(model.editor)

        // Typed but not saved yet: undoing saves it, then undoes it.
        editor.body = "One\nTwo"
        #expect(await model.undoLastChange())
        #expect(try await store.node(node.id, in: space.id).body == "One")
        #expect(model.editor?.body == "One")

        #expect(await model.redoLastChange())
        #expect(try await store.node(node.id, in: space.id).body == "One\nTwo")
        #expect(model.editor?.body == "One\nTwo")

        #expect(model.history?.conflict == nil)
    }

    @Test func undoAsksBeforeOverwritingSomeoneElsesLaterChange() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let url = folder.appending(path: "Shared.knode", directoryHint: .isDirectory)
        let ada = CoreSpaceStore(person: "Ada")
        let space = try await ada.createSpace(at: url, name: "Shared")
        let node = try await ada.createNode(in: space.id, title: "Plan", body: "One")
        _ = try await ada.updateNode(node.id, in: space.id, title: nil, body: "One\nTwo")
        // Grace writes to the same note after Ada.
        let grace = CoreSpaceStore(person: "Grace")
        _ = try await grace.openSpace(at: url)
        _ = try await grace.updateNode(node.id, in: space.id, title: nil, body: "One\nTwo\nThree")

        let history = NoteHistoryModel(nodeID: node.id, spaceID: space.id, store: ada)
        #expect(await history.undoLastChange())
        let conflict = try #require(history.conflict)
        #expect(!conflict.isRedo)
        #expect(try await ada.node(node.id, in: space.id).body == "One\nTwo\nThree")

        await history.confirmForcedUndo(conflict)
        #expect(history.conflict == nil)
        #expect(try await ada.node(node.id, in: space.id).body == "One")
    }
}
