import Foundation
import Observation

/// The history of one note: its versions newest first, who made each and when, a comparison of a
/// version with the note as it is now, and restoring a version or undoing the change behind it.
@MainActor
@Observable
final class NoteHistoryModel {
    /// How a version is compared with the note as it is now.
    enum Comparison: String, CaseIterable, Identifiable {
        case changes, sideBySide
        var id: Self { self }

        var title: String {
            switch self {
            case .changes: "Changes"
            case .sideBySide: "Side by Side"
            }
        }
    }

    /// An undo the core refused because later changes would be overwritten, waiting for the
    /// person to confirm it.
    struct Conflict: Equatable {
        /// The change to undo.
        var change: Int64
        var message: String
        /// True when it redoes a change: it undoes an undo.
        var isRedo = false
    }

    let nodeID: Node.ID
    let spaceID: Space.ID

    /// Every version, newest first.
    private(set) var versions: [NoteVersion] = []
    private(set) var isLoading = false
    /// The version being compared with the note as it is now, if any.
    var selectedVersionID: NoteVersion.ID?
    var comparison: Comparison = .changes
    var conflict: Conflict?
    var errorMessage: String?

    @ObservationIgnored private let store: any SpaceStore
    /// The title and text in the editor, which may not be saved yet.
    @ObservationIgnored var currentTitle: @MainActor () -> String = { "" }
    @ObservationIgnored var currentBody: @MainActor () -> String = { "" }
    /// Runs before a restore or an undo: saves the editor.
    @ObservationIgnored var willChange: (@MainActor () async -> Void)?
    /// Runs after a restore or an undo changed the note.
    @ObservationIgnored var didChange: (@MainActor () async -> Void)?

    init(nodeID: Node.ID, spaceID: Space.ID, store: any SpaceStore) {
        self.nodeID = nodeID
        self.spaceID = spaceID
        self.store = store
    }

    var selectedVersion: NoteVersion? {
        versions.first { $0.id == selectedVersionID }
    }

    /// The newest version: the note as last saved.
    var latest: NoteVersion? { versions.first }

    func load() async {
        isLoading = true
        defer { isLoading = false }
        do {
            versions = try await store.history(of: nodeID, in: spaceID)
            if let selected = selectedVersionID, !versions.contains(where: { $0.id == selected }) { selectedVersionID = nil }
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// The version just before this one, if any: what the change behind it started from.
    func previous(_ version: NoteVersion) -> NoteVersion? {
        guard let index = versions.firstIndex(of: version), versions.indices.contains(index + 1) else { return nil }
        return versions[index + 1]
    }

    /// What the change behind a version did: its text compared with the version before it.
    func changeSummary(_ version: NoteVersion) -> LineDiff.Summary {
        LineDiff.summary(from: previous(version)?.body ?? "", to: version.body)
    }

    /// The lines of a version compared with the note as it is now: removed lines are in the
    /// version only, added lines in the note only.
    func diff(_ version: NoteVersion) -> [LineDiff.Line] {
        LineDiff.lines(from: version.body, to: currentBody())
    }

    /// Whether a version already holds what the note holds now.
    func isCurrent(_ version: NoteVersion) -> Bool {
        version.title == currentTitle() && version.body == currentBody()
    }

    /// Whether "Undo This Change" applies: the change wrote this version and did not create the
    /// note (undoing that would delete it).
    func canUndo(_ version: NoteVersion) -> Bool {
        version.changeID != nil && version.operation == .update
    }

    /// Puts the note back to a version, as a new version on top.
    func restore(_ version: NoteVersion) async {
        await willChange?()
        do {
            _ = try await store.restore(nodeID, to: version.seq, in: spaceID)
            selectedVersionID = nil
            await load()
            await didChange?()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Undoes the change behind a version. When later changes would be overwritten the core
    /// refuses, and `conflict` asks the person; `force` undoes it anyway.
    func undo(_ version: NoteVersion, force: Bool = false) async {
        guard let change = version.changeID else { return }
        await willChange?()
        await undo(change: change, force: force, isRedo: false)
    }

    /// Undoes the last change the person made to this note (`LastChange.undoTarget`), after saving
    /// what is in the editor. Returns false when there is nothing of theirs to undo.
    @discardableResult
    func undoLastChange() async -> Bool {
        await undoOrRedoLastChange(isRedo: false)
    }

    /// Redoes the change the person undid last in this note, by undoing that undo
    /// (`LastChange.redoTarget`). Returns false when there is nothing to redo.
    @discardableResult
    func redoLastChange() async -> Bool {
        await undoOrRedoLastChange(isRedo: true)
    }

    /// Undoes the change of a conflict the person confirmed, overwriting the later changes.
    func confirmForcedUndo(_ conflict: Conflict) async {
        self.conflict = nil
        await willChange?()
        await undo(change: conflict.change, force: true, isRedo: conflict.isRedo)
    }

    private func undoOrRedoLastChange(isRedo: Bool) async -> Bool {
        await willChange?()
        let changes: [ChangeRecord]
        do {
            changes = try await store.changes(of: nodeID, in: spaceID, limit: LastChange.limit)
        } catch {
            errorMessage = error.localizedDescription
            return false
        }
        let person = store.person
        let target = isRedo ? LastChange.redoTarget(in: changes, by: person) : LastChange.undoTarget(in: changes, by: person)
        guard let target else { return false }
        // Stepping back through one's own undos is never a conflict: the later changes the core
        // names are then only this person's undone edits and undos, so nothing is overwritten.
        await undo(change: target.id, force: false, isRedo: isRedo) { later in
            LastChange.areOwnUndoSteps(later, in: changes, by: person)
        }
        return true
    }

    /// Undoes a change. On a conflict, `forceWhen` may decide to go ahead; otherwise `conflict`
    /// asks the person.
    private func undo(change: Int64, force: Bool, isRedo: Bool, forceWhen: (([Int64]) -> Bool)? = nil) async {
        do {
            try await store.undo(change: change, in: spaceID, force: force)
            conflict = nil
            selectedVersionID = nil
            errorMessage = nil
            await load()
            await didChange?()
        } catch let error as SpaceStoreError {
            if case .undoConflict(_, let later, let message) = error, !force {
                if let forceWhen, forceWhen(later) {
                    await undo(change: change, force: true, isRedo: isRedo)
                } else {
                    conflict = Conflict(change: change, message: error.errorDescription ?? message, isRedo: isRedo)
                }
            } else {
                errorMessage = error.localizedDescription
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

/// Which change "Undo Last Change" and "Redo Last Change" work on, in one note.
///
/// The rule: undo takes the newest edit or restore of the note made by this person (the name the
/// app writes as) that is not undone now. Redo takes the newest undo of this person's that is in
/// effect and undid one of their edits, as long as they have not edited the note since; it
/// undoes that undo. Changes made by agents, or by Knode, are never picked: they are undone from
/// the history panel. Creating a note is not undone this way, as that would delete it.
///
/// When anyone else changed the note after the picked change, the person is asked first, as in
/// the history panel. Their own undone edits and undos in between do not count.
enum LastChange {
    /// How many changes of a note are read to decide.
    static let limit = 1000

    private static let edits: Set<String> = [ChangeRecord.Action.update, ChangeRecord.Action.restore]

    /// The change to undo, from the changes of a note, newest first.
    static func undoTarget(in changes: [ChangeRecord], by person: String) -> ChangeRecord? {
        let undone = UndoState(changes)
        return changes.first { isMine($0, person) && edits.contains($0.action) && !undone.isUndone($0.id) }
    }

    /// The undo to undo, which redoes what it undid, from the changes of a note, newest first.
    static func redoTarget(in changes: [ChangeRecord], by person: String) -> ChangeRecord? {
        let undone = UndoState(changes)
        let byID = Dictionary(changes.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        for change in changes where isMine(change, person) {
            if edits.contains(change.action) {
                // A new edit since the undo: there is nothing to redo, as in any editor.
                if !undone.isUndone(change.id) { return nil }
                continue
            }
            guard change.action == ChangeRecord.Action.undo, !undone.isUndone(change.id),
                  let target = change.undoes.flatMap({ byID[$0] }), edits.contains(target.action)
            else { continue }
            return change
        }
        return nil
    }

    /// Whether the later changes an undo would overwrite are only steps of this person's own
    /// undo and redo: their edits that are undone now, and their undos.
    static func areOwnUndoSteps(_ later: [Int64], in changes: [ChangeRecord], by person: String) -> Bool {
        let undone = UndoState(changes)
        let byID = Dictionary(changes.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        return !later.isEmpty && later.allSatisfy { id in
            guard let change = byID[id], isMine(change, person) else { return false }
            return change.action == ChangeRecord.Action.undo || undone.isUndone(id)
        }
    }

    private static func isMine(_ change: ChangeRecord, _ person: String) -> Bool {
        change.author.kind == .person && change.author.name == person
    }

    /// Which changes are undone now: those with an undo that is itself not undone.
    private struct UndoState {
        private var undos: [Int64: [Int64]] = [:]

        init(_ changes: [ChangeRecord]) {
            for change in changes where change.action == ChangeRecord.Action.undo {
                if let target = change.undoes { undos[target, default: []].append(change.id) }
            }
        }

        func isUndone(_ id: Int64, depth: Int = 0) -> Bool {
            guard depth < 64 else { return false }
            return (undos[id] ?? []).contains { !isUndone($0, depth: depth + 1) }
        }
    }
}

/// A line by line comparison of two texts.
enum LineDiff {
    struct Line: Identifiable, Hashable {
        enum Kind: Hashable { case same, removed, added }
        var id: Int
        var kind: Kind
        var text: String
        /// The line number in the old text, for kept and removed lines.
        var old: Int?
        /// The line number in the new text, for kept and added lines.
        var new: Int?
    }

    struct Summary: Hashable {
        var added: Int
        var removed: Int
    }

    /// Every line of both texts in order: kept lines once, removed lines before the lines added
    /// in their place.
    static func lines(from old: String, to new: String) -> [Line] {
        let a = split(old), b = split(new)
        let difference = b.difference(from: a)
        var removed = Set<Int>(), added = Set<Int>()
        for change in difference {
            switch change {
            case .remove(let offset, _, _): removed.insert(offset)
            case .insert(let offset, _, _): added.insert(offset)
            }
        }
        var result: [Line] = []
        var i = 0, j = 0
        while i < a.count || j < b.count {
            if i < a.count, removed.contains(i) {
                result.append(Line(id: result.count, kind: .removed, text: a[i], old: i + 1))
                i += 1
            } else if j < b.count, added.contains(j) {
                result.append(Line(id: result.count, kind: .added, text: b[j], new: j + 1))
                j += 1
            } else if i < a.count, j < b.count {
                result.append(Line(id: result.count, kind: .same, text: a[i], old: i + 1, new: j + 1))
                i += 1
                j += 1
            } else {
                break
            }
        }
        return result
    }

    static func summary(from old: String, to new: String) -> Summary {
        let difference = split(new).difference(from: split(old))
        return Summary(added: difference.insertions.count, removed: difference.removals.count)
    }

    private static func split(_ text: String) -> [String] {
        text.isEmpty ? [] : text.components(separatedBy: "\n")
    }
}
