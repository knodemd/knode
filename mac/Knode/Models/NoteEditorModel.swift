import Foundation
import Observation

/// One open note: its title and Markdown body as they are being edited, saved shortly after the
/// last keystroke.
@MainActor
@Observable
final class NoteEditorModel {
    let spaceID: Space.ID
    let nodeID: Node.ID

    var title: String {
        didSet { if title != oldValue { scheduleSave() } }
    }

    var body: String {
        didSet { if body != oldValue { scheduleSave() } }
    }

    private(set) var updatedAt: Date
    private(set) var hasUnsavedChanges = false
    private(set) var errorMessage: String?

    @ObservationIgnored private let store: any SpaceStore
    @ObservationIgnored private let saveDelay: Duration
    @ObservationIgnored private var pendingSave: Task<Void, Never>?
    @ObservationIgnored var onSave: (@MainActor (Node) -> Void)?

    init(node: Node, spaceID: Space.ID, store: any SpaceStore, saveDelay: Duration = .milliseconds(600)) {
        self.spaceID = spaceID
        self.nodeID = node.id
        self.title = node.title
        self.body = node.body
        self.updatedAt = node.updatedAt
        self.store = store
        self.saveDelay = saveDelay
    }

    private func scheduleSave() {
        hasUnsavedChanges = true
        pendingSave?.cancel()
        let delay = saveDelay
        pendingSave = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled else { return }
            await self?.save()
        }
    }

    /// Saves now, if anything changed.
    func save() async {
        pendingSave?.cancel()
        pendingSave = nil
        guard hasUnsavedChanges else { return }
        let (title, body) = (self.title, self.body)
        do {
            let node = try await store.updateNode(nodeID, in: spaceID, title: title, body: body)
            updatedAt = node.updatedAt
            if self.title == title, self.body == body { hasUnsavedChanges = false }
            errorMessage = nil
            onSave?(node)
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
