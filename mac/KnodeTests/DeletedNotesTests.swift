import Foundation
import Testing
@testable import Knode

/// The notes whose file was deleted from the live Markdown folder, against a fake Knode Service.
@MainActor
@Suite(.serialized) final class DeletedNotesTests {
    let service: FakeKnodeService
    let path = "/tmp/Deleted.knode"

    init() throws {
        service = try FakeKnodeService()
    }

    deinit {
        service.stop()
    }

    func makeModel() async -> AppModel {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let model = AppModel(store: store, searchDelay: .zero, saveDelay: .seconds(60))
        await model.openSpace(at: URL(filePath: path, directoryHint: .isDirectory))
        return model
    }

    @Test func theAppAsksAboutNotesWhoseFileWasDeleted() async throws {
        let model = await makeModel()
        let deleted = try #require(model.deletedNotes)
        #expect(deleted.waiting.isEmpty)

        let plan = service.agentCreatesNode(in: path, title: "Plan", body: "Draft.")
        let idea = service.agentCreatesNode(in: path, title: "Idea", body: "Maybe.")
        service.folderDeletesFile(of: plan, in: path, file: "Plan.md")
        service.folderDeletesFile(of: idea, in: path, file: "Idea.md")
        #expect(try await eventually { deleted.waiting.count == 2 })
        #expect(Set(deleted.waiting.map(\.path)) == ["Plan.md", "Idea.md"])
        #expect(deleted.waiting.first { $0.id == plan }?.title == "Plan")
    }

    @Test func keepingANoteClearsItsMark() async throws {
        let model = await makeModel()
        let plan = service.agentCreatesNode(in: path, title: "Plan", body: "Draft.")
        service.folderDeletesFile(of: plan, in: path, file: "Plan.md")
        let deleted = try #require(model.deletedNotes)
        #expect(try await eventually { deleted.waiting.count == 1 })

        await deleted.keep([plan])
        #expect(deleted.waiting.isEmpty)
        #expect(service.deletedFiles(in: path).isEmpty)
        #expect(service.node(plan, in: path) != nil)
        #expect(service.params(of: "markdown.deleted.keep")?["id"] as? String == plan)
    }

    @Test func deletingANoteDeletesItAsThePersonAndCanBeUndone() async throws {
        let model = await makeModel()
        let plan = service.agentCreatesNode(in: path, title: "Plan", body: "Draft.")
        service.folderDeletesFile(of: plan, in: path, file: "Plan.md")
        let deleted = try #require(model.deletedNotes)
        #expect(try await eventually { deleted.waiting.count == 1 })

        await deleted.delete([plan])
        #expect(deleted.waiting.isEmpty)
        #expect(deleted.lastDeleted == [plan])
        #expect(service.node(plan, in: path) == nil)
        let by = service.params(of: "node.delete")?["by"] as? [String: Any]
        #expect(by?["kind"] as? String == "person")
        #expect(by?["actor"] as? String == "Ada")
        #expect(try await eventually { !model.nodes.contains { $0.id == plan } })

        // Undone through the history of the note: the delete is the newest change.
        service.setChanges([
            ["id": 7, "changed_at": 1_792_489_800_700, "changed_by": ["kind": "person", "actor": "Ada"], "action": "node.delete", "subject": plan, "detail": [String: Any]()],
        ], for: plan)
        await deleted.undoDelete()
        #expect((service.params(of: "undo")?["change"] as? NSNumber)?.int64Value == 7)
        #expect(deleted.lastDeleted.isEmpty)
        #expect(deleted.errorMessage == nil)
    }

    @Test func decidingLaterLeavesTheNoteMarked() async throws {
        let model = await makeModel()
        let plan = service.agentCreatesNode(in: path, title: "Plan", body: "Draft.")
        service.folderDeletesFile(of: plan, in: path, file: "Plan.md")
        let deleted = try #require(model.deletedNotes)
        #expect(try await eventually { deleted.waiting.count == 1 })

        deleted.decideLater([plan])
        #expect(deleted.waiting.isEmpty)
        #expect(deleted.notes.map(\.id) == [plan])
        #expect(service.deletedFiles(in: path) == [plan: "Plan.md"])
        #expect(service.node(plan, in: path) != nil)
    }
}
