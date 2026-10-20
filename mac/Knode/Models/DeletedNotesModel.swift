import Foundation
import Observation

/// The notes of the open space whose file was deleted from its live Markdown folder, and what
/// the person decides for each: delete it in Knode too, keep it (its file is written again), or
/// decide later.
@MainActor
@Observable
final class DeletedNotesModel {
    let spaceID: Space.ID

    /// Every note waiting, newest first.
    private(set) var notes: [DeletedNote] = []
    /// The notes left for later in this session: the banner no longer asks about them.
    private(set) var postponed: Set<Node.ID> = []
    /// The notes deleted by the last delete, which `undoDelete()` brings back.
    private(set) var lastDeleted: [Node.ID] = []
    private(set) var isWorking = false
    var errorMessage: String?
    /// Called after the notes changed, so the list of notes follows.
    @ObservationIgnored var onChange: (@MainActor () async -> Void)?

    @ObservationIgnored private let store: any DeletedNotesStore
    @ObservationIgnored private let history: any SpaceStore

    init(spaceID: Space.ID, store: any DeletedNotesStore & SpaceStore) {
        self.spaceID = spaceID
        self.store = store
        history = store
    }

    /// The notes the banner asks about: every note waiting, but those left for later.
    var waiting: [DeletedNote] {
        notes.filter { !postponed.contains($0.id) }
    }

    /// Reads the notes waiting again. A service too old to know them shows none.
    func load() async {
        do {
            notes = try await store.deletedNotes(in: spaceID)
        } catch {
            notes = []
        }
        postponed.formIntersection(notes.map(\.id))
    }

    /// Keeps the notes: their files are written to the folder again.
    func keep(_ ids: [Node.ID]) async {
        await work {
            for id in ids {
                try await self.store.keepDeletedNote(id, in: self.spaceID)
            }
        }
    }

    /// Deletes the notes in Knode too, as the person. `undoDelete()` brings them back.
    func delete(_ ids: [Node.ID]) async {
        var deleted: [Node.ID] = []
        await work {
            for id in ids {
                try await self.store.deleteNode(id, in: self.spaceID)
                deleted.append(id)
            }
        }
        lastDeleted = deleted
    }

    /// Leaves the notes for later: they stay as they are, and the banner stops asking about
    /// them until Knode opens the space again.
    func decideLater(_ ids: [Node.ID]) {
        postponed.formUnion(ids)
    }

    /// Undoes the last delete, through the history of each note.
    func undoDelete() async {
        let ids = lastDeleted
        await work {
            for id in ids {
                let changes = try await self.history.changes(of: id, in: self.spaceID, limit: 1)
                guard let change = changes.first, change.action == ChangeRecord.Action.delete else { continue }
                try await self.history.undo(change: change.id, in: self.spaceID, force: false)
            }
        }
        lastDeleted = []
    }

    private func work(_ body: () async throws -> Void) async {
        isWorking = true
        defer { isWorking = false }
        do {
            try await body()
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
        await load()
        await onChange?()
    }
}
