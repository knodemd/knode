import Foundation
import Observation

/// One open note: its title and Markdown body as they are being edited.
///
/// Through Knode Service the body is edited together with the agents: what the person types is
/// sent as splices based on the version of the text the editor has (300 ms after the last
/// keystroke, at most a second apart while typing goes on), and what others write appears in the
/// open text as it happens, merged by the service. Without the service the whole body is saved
/// shortly after the last keystroke, as before.
@MainActor
@Observable
final class NoteEditorModel {
    let spaceID: Space.ID
    let nodeID: Node.ID

    var title: String {
        didSet {
            guard title != oldValue, !applyingRemote else { return }
            titleChanged = true
            hasUnsavedChanges = true
            scheduleSave()
        }
    }

    /// The body as the editor shows it.
    var body: String {
        didSet {
            guard body != oldValue, !applyingRemote else { return }
            recordLocalEdit(from: oldValue)
        }
    }

    private(set) var updatedAt: Date
    private(set) var hasUnsavedChanges = false
    private(set) var errorMessage: String?
    /// Someone else who changed the note in the last few seconds, such as an agent writing
    /// through Knode Service while the person edits.
    private(set) var otherWriter: Author?
    /// Set by the text view while an input method composes text: what others write waits until
    /// the composition ends.
    var isComposing = false {
        didSet { if !isComposing, oldValue, behind { scheduleSend(now: true) } }
    }

    /// Whether the body is edited together through Knode Service.
    var isShared: Bool { base != nil }

    @ObservationIgnored private let store: any SpaceStore
    @ObservationIgnored private let saveDelay: Duration
    @ObservationIgnored private let typingDelay: Duration
    @ObservationIgnored private let typingLimit: Duration
    @ObservationIgnored private let presenceDuration: Duration
    @ObservationIgnored private let createdAt: Date
    @ObservationIgnored private let kind: String
    @ObservationIgnored private var pendingSave: Task<Void, Never>?
    @ObservationIgnored var onSave: (@MainActor (Node) -> Void)?

    // Editing together.
    /// The version of the text shown, without the edits waiting to be sent; `nil` when the body
    /// is not edited together.
    @ObservationIgnored private var base: String?
    /// What the person typed since `base`: splices to apply in order.
    @ObservationIgnored private var pending: [TextSplice] = []
    @ObservationIgnored private var firstPending: ContinuousClock.Instant?
    @ObservationIgnored private var sending = false
    @ObservationIgnored private var recovering = false
    /// Someone else wrote a version the editor does not show yet.
    @ObservationIgnored private var behind = false
    /// A version announced by the service while an edit was being sent.
    @ObservationIgnored private var announced: String?
    @ObservationIgnored private var pendingSend: Task<Void, Never>?
    @ObservationIgnored private var presence: Task<Void, Never>?
    @ObservationIgnored private var titleChanged = false
    @ObservationIgnored private var bodyChanged = false
    @ObservationIgnored private var applyingRemote = false
    /// The splice of the edit the text view reported, used instead of a diff when it matches.
    @ObservationIgnored private var reported: [TextSplice]?

    /// - Parameter document: the body opened for editing together, or `nil` to save it whole.
    init(
        node: Node,
        spaceID: Space.ID,
        store: any SpaceStore,
        document: BodyDocument? = nil,
        saveDelay: Duration = .milliseconds(600),
        typingDelay: Duration = .milliseconds(300),
        typingLimit: Duration = .seconds(1),
        presenceDuration: Duration = .seconds(5)
    ) {
        self.spaceID = spaceID
        self.nodeID = node.id
        self.title = node.title
        self.body = document?.text ?? node.body
        self.base = document?.version
        self.updatedAt = node.updatedAt
        self.createdAt = node.createdAt
        self.kind = node.kind
        self.store = store
        self.saveDelay = saveDelay
        self.typingDelay = typingDelay
        self.typingLimit = typingLimit
        self.presenceDuration = presenceDuration
    }

    /// An editor for a note: editing its body together through Knode Service when the store can,
    /// on its own otherwise.
    static func open(_ node: Node, spaceID: Space.ID, store: any SpaceStore, saveDelay: Duration = .milliseconds(600)) async -> NoteEditorModel {
        let document = try? await (store as? any CoeditingStore)?.openBody(node.id, in: spaceID)
        return NoteEditorModel(node: node, spaceID: spaceID, store: store, document: document, saveDelay: saveDelay)
    }

    // MARK: Local edits

    /// The text view changed the text to `text`: `range` (UTF-16, in the text before) was replaced
    /// by `replacement`, when it says so.
    func viewDidEdit(text: String, range: NSRange?, replacement: String?) {
        guard text != body else { return }
        if let range, let replacement,
           let splice = TextDiff.splice(in: body, replacing: range, with: replacement),
           TextDiff.apply([splice], to: body) == text {
            reported = [splice]
        }
        body = text
        reported = nil
    }

    private func recordLocalEdit(from old: String) {
        hasUnsavedChanges = true
        guard isShared else {
            bodyChanged = true
            scheduleSave()
            return
        }
        pending += reported ?? TextDiff.splices(from: old, to: body)
        if firstPending == nil { firstPending = .now }
        scheduleSend()
    }

    private func scheduleSend(now: Bool = false) {
        pendingSend?.cancel()
        let waited = firstPending.map { ContinuousClock.now - $0 } ?? .zero
        let delay: Duration = now ? .zero : max(.zero, min(typingDelay, typingLimit - waited))
        pendingSend = Task { [weak self] in
            if delay > .zero { try? await Task.sleep(for: delay) }
            guard !Task.isCancelled else { return }
            await self?.send()
        }
    }

    /// Sends what was typed, if anything, and catches up with what others wrote.
    private func send() async {
        guard let base, !sending else { return }
        guard !pending.isEmpty || (behind && !isComposing) else { return }
        let splices = pending
        pending = []
        firstPending = nil
        pendingSend?.cancel()
        sending = true
        announced = nil
        let result: BodyEditResult
        do {
            guard let store = store as? any CoeditingStore else { throw SpaceStoreError.notCoediting }
            result = try await store.editBody(nodeID, in: spaceID, version: base, splices: splices)
        } catch {
            sending = false
            await recover(from: error, unsent: splices)
            return
        }
        sending = false
        self.base = result.seen
        errorMessage = nil
        if !splices.isEmpty {
            updatedAt = Date()
            onSave?(currentNode)
        }
        let announcedOther = announced.map { $0 != result.version && $0 != result.seen } ?? false
        announced = nil
        behind = result.text != nil || announcedOther
        if let text = result.text, pending.isEmpty, !isComposing {
            // Nothing typed meanwhile: the text shown is exactly the one at `seen`.
            applyRemote(text)
            self.base = result.version
            behind = announcedOther
        }
        if !pending.isEmpty || (behind && !isComposing) {
            // Typed meanwhile: sent first, based on `seen`; its answer brings the text.
            await send()
            return
        }
        if !titleChanged, !bodyChanged { hasUnsavedChanges = false }
    }

    /// After an edit failed: without Knode Service the whole body is saved through the core in the
    /// app; with it, the body is opened again and the text shown written over it.
    private func recover(from error: any Error, unsent: [TextSplice]) async {
        let stopped = (error as? ServiceError)?.meansNotRunning == true || (error as? SpaceStoreError) == .notCoediting
        if stopped {
            base = nil
            pending = []
            bodyChanged = true
            hasUnsavedChanges = true
            await save()
            return
        }
        guard case SpaceStoreError.invalid = error, !recovering, let store = store as? any CoeditingStore else {
            // Kept, to be sent with the next edit.
            pending = unsent + pending
            errorMessage = error.localizedDescription
            return
        }
        recovering = true
        defer { recovering = false }
        do {
            let document = try await store.openBody(nodeID, in: spaceID)
            base = document.version
            // The text shown already holds every edit typed: written over the body as it is now.
            pending = TextDiff.splices(from: document.text, to: body)
            await send()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: Changes made by others

    /// Someone changed the note: the body catches up, merged with what the person types, and who
    /// it was shows for a few seconds. Returns false when the body is not edited together.
    @discardableResult
    func remoteChanged(version: String?, author: Author?) -> Bool {
        if let author, !isMe(author) { show(author) }
        guard isShared else { return false }
        if let version, version == base { return true }
        if sending {
            announced = version ?? "unknown"
            return true
        }
        behind = true
        if !isComposing { scheduleSend(now: pending.isEmpty) }
        return true
    }

    /// Shows the title someone else gave the note, unless the person is changing it.
    func remoteTitle(_ title: String) {
        guard !titleChanged, title != self.title else { return }
        applyingRemote = true
        self.title = title
        applyingRemote = false
    }

    private func applyRemote(_ text: String) {
        applyingRemote = true
        body = text
        applyingRemote = false
    }

    private func isMe(_ author: Author) -> Bool {
        author.kind == .person && author.name == store.person
    }

    private func show(_ author: Author) {
        otherWriter = author
        presence?.cancel()
        let duration = presenceDuration
        presence = Task { [weak self] in
            try? await Task.sleep(for: duration)
            guard !Task.isCancelled else { return }
            self?.otherWriter = nil
        }
    }

    // MARK: Saving

    private func scheduleSave() {
        pendingSave?.cancel()
        let delay = saveDelay
        pendingSave = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled else { return }
            await self?.save()
        }
    }

    /// Saves now what changed: sends what was typed when the body is edited together, saves the
    /// title, and the whole body when it is not.
    func save() async {
        pendingSave?.cancel()
        pendingSave = nil
        if isShared, !pending.isEmpty { await send() }
        guard titleChanged || bodyChanged else {
            if pending.isEmpty, !sending { hasUnsavedChanges = false }
            return
        }
        let title = self.title
        let body = bodyChanged ? self.body : nil
        do {
            let node = try await store.updateNode(nodeID, in: spaceID, title: titleChanged ? title : nil, body: body)
            updatedAt = node.updatedAt
            if self.title == title { titleChanged = false }
            if let body, self.body == body { bodyChanged = false }
            if !titleChanged, !bodyChanged, pending.isEmpty { hasUnsavedChanges = false }
            errorMessage = nil
            onSave?(node)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private var currentNode: Node {
        Node(id: nodeID, title: title, body: body, createdAt: createdAt, updatedAt: updatedAt, kind: kind)
    }
}
