import Foundation
import Observation

/// A canvas: a board of notes arranged by hand, with groups, zoom and pan.
///
/// Every move, resize, add or removal is saved to the space at once, as positions, which add no
/// change to the history. Within the session each of them can be undone and redone.
@MainActor
@Observable
final class CanvasModel {
    let spaceID: Space.ID
    let nodeID: Node.ID

    private(set) var view: MapView?
    private(set) var items: [CanvasItem] = []
    private(set) var groups: [ViewGroupInfo] = []
    private(set) var edges: [GraphEdgeInfo] = []
    private(set) var linkKinds: [LinkKindInfo] = LinkKindInfo.builtIns
    var selection: Set<Node.ID> = []
    var viewport = Viewport() {
        didSet { if viewport != oldValue { scheduleViewportSave() } }
    }
    var size = CGSize(width: 800, height: 600)
    private(set) var isMissing = false
    var errorMessage: String?
    private(set) var canUndo = false
    private(set) var canRedo = false

    /// Opens a node of the board, such as on a double click.
    @ObservationIgnored var onOpen: (@MainActor (Node.ID) -> Void)?
    /// Undo and redo through the window, as Edit > Undo does; optional.
    @ObservationIgnored weak var undoManager: UndoManager?

    @ObservationIgnored private let store: any MapStore
    @ObservationIgnored private let saveDelay: Duration
    @ObservationIgnored private var pendingViewportSave: Task<Void, Never>?
    @ObservationIgnored private var undoStack: [Edit] = []
    @ObservationIgnored private var redoStack: [Edit] = []
    /// The board as it was when a drag started.
    @ObservationIgnored private var dragStart: Snapshot?

    static let defaultSize = CGSize(width: 240, height: 120)
    static let collapsedHeight = 40.0
    static let minimumSize = CGSize(width: 120, height: 40)

    /// The positions and groups of the board, at one moment.
    struct Snapshot: Equatable {
        var positions: [Node.ID: ViewPosition]
        var groups: [ViewGroupInfo.ID: ViewGroupInfo]

        func renamingGroups(_ renamed: [ViewGroupInfo.ID: ViewGroupInfo.ID]) -> Snapshot {
            var copy = self
            copy.groups = Dictionary(groups.values.map { group -> (ViewGroupInfo.ID, ViewGroupInfo) in
                var group = group
                group.id = renamed[group.id] ?? group.id
                return (group.id, group)
            }, uniquingKeysWith: { first, _ in first })
            for (id, position) in positions {
                if let old = position.groupID, let new = renamed[old] { copy.positions[id]?.groupID = new }
            }
            return copy
        }
    }

    /// One edit, as it can be undone: what changed, before and after. A node or group missing
    /// from one side was not on the board then.
    struct Edit: Equatable {
        var name: String
        var before: Snapshot
        var after: Snapshot
    }

    init(spaceID: Space.ID, nodeID: Node.ID, store: any MapStore, saveDelay: Duration = .seconds(1)) {
        self.spaceID = spaceID
        self.nodeID = nodeID
        self.store = store
        self.saveDelay = saveDelay
    }

    // MARK: Loading

    /// Reads the board from the space. The first time, the saved viewport is shown, or one that
    /// fits the board.
    func load() async {
        do {
            guard let board = try await store.canvasBoard(of: nodeID, in: spaceID) else {
                isMissing = true
                return
            }
            let first = view == nil
            view = board.view
            items = board.items
            groups = board.groups
            edges = board.edges
            linkKinds = board.linkKinds
            selection = selection.filter { id in items.contains { $0.id == id } }
            if first {
                if let saved = board.view.viewport {
                    viewport = saved
                } else if !items.isEmpty {
                    fit()
                }
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: Geometry

    /// The box of a node on the board, in points of the plane.
    func frame(of item: CanvasItem) -> CGRect {
        let size = Self.boxSize(of: item.position)
        return CGRect(x: item.position.x - size.width / 2, y: item.position.y - size.height / 2, width: size.width, height: size.height)
    }

    static func boxSize(of position: ViewPosition) -> CGSize {
        let size = position.size ?? defaultSize
        return position.collapsed ? CGSize(width: size.width, height: collapsedHeight) : size
    }

    /// Whether a node is hidden in a collapsed group.
    func isHidden(_ item: CanvasItem) -> Bool {
        guard let group = item.position.groupID else { return false }
        return groups.first { $0.id == group }?.collapsed ?? false
    }

    /// The group whose box holds a point of the plane, the smallest first.
    func group(containing point: CGPoint) -> ViewGroupInfo? {
        groups.filter { !$0.collapsed && $0.frame.contains(point) }.min { $0.width * $0.height < $1.width * $1.height }
    }

    var bounds: CGRect {
        (items.map(frame(of:)) + groups.map(\.frame)).reduce(CGRect.null) { $0.union($1) }
    }

    func fit() {
        viewport = Viewport.fitting(bounds, in: size, margin: 60)
    }

    func pan(by translation: CGSize) {
        viewport = viewport.panned(by: translation)
    }

    func zoom(by factor: Double, at anchor: CGPoint? = nil) {
        viewport = viewport.zoomed(by: factor, at: anchor ?? CGPoint(x: size.width / 2, y: size.height / 2), in: size)
    }

    private var topZ: Int64 { items.map(\.position.z).max() ?? 0 }

    // MARK: Adding and removing

    /// Puts notes on the board around a point of the plane, in front of the others. Notes already
    /// on it move there.
    func add(_ nodes: [NodeSummary], at point: CGPoint) async {
        guard !nodes.isEmpty else { return }
        let before = snapshot()
        var z = topZ
        let group = group(containing: point)?.id
        for (offset, node) in nodes.enumerated() {
            z += 1
            let step = Double(offset) * 28
            var position = ViewPosition(nodeID: node.id, x: point.x + step, y: point.y + step, z: z, groupID: group)
            if let at = items.firstIndex(where: { $0.id == node.id }) {
                position.size = items[at].position.size
                position.collapsed = items[at].position.collapsed
                items[at].position = position
            } else {
                items.append(CanvasItem(position: position, kind: node.kind, title: node.title, excerpt: node.excerpt))
            }
        }
        selection = Set(nodes.map(\.id))
        await commit(nodes.count == 1 ? "Add Note" : "Add Notes", from: before)
        await reloadEdges()
    }

    /// Takes notes off the board; the notes stay in the space.
    func remove(_ ids: Set<Node.ID>) async {
        guard !ids.isEmpty else { return }
        let before = snapshot()
        for item in items where ids.contains(item.id) { removedItems[item.id] = item }
        items.removeAll { ids.contains($0.id) }
        selection.subtract(ids)
        edges.removeAll { ids.contains($0.from) || ids.contains($0.to) }
        await commit("Remove from Canvas", from: before)
    }

    // MARK: Moving and resizing

    /// Starts dragging: what the board looks like now is what an undo goes back to.
    func beginDrag() {
        if dragStart == nil { dragStart = snapshot() }
    }

    /// Moves notes by a distance of the plane, from where they were when the drag began.
    func move(_ ids: Set<Node.ID>, by delta: CGSize) {
        guard let start = dragStart else { return }
        for index in items.indices where ids.contains(items[index].id) {
            guard let origin = start.positions[items[index].id] else { continue }
            items[index].position.x = origin.x + delta.width
            items[index].position.y = origin.y + delta.height
        }
    }

    /// Ends a move: a note dropped inside a group joins it, one dropped outside leaves it, and the
    /// moved notes come to the front.
    func endMove(_ ids: Set<Node.ID>) async {
        guard let before = dragStart else { return }
        dragStart = nil
        var z = topZ
        for index in items.indices where ids.contains(items[index].id) {
            items[index].position.groupID = group(containing: items[index].position.center)?.id
            if before.positions[items[index].id] != items[index].position {
                z += 1
                items[index].position.z = z
            }
        }
        await commit(ids.count == 1 ? "Move Note" : "Move Notes", from: before)
    }

    /// Resizes a note's box, from its size when the drag began, keeping its top-left corner.
    func resize(_ id: Node.ID, by delta: CGSize) {
        guard let start = dragStart?.positions[id], let index = items.firstIndex(where: { $0.id == id }) else { return }
        let old = Self.boxSize(of: start)
        let width = max(Self.minimumSize.width, old.width + delta.width)
        let height = start.collapsed ? (start.size ?? Self.defaultSize).height : max(Self.minimumSize.height, old.height + delta.height)
        let shown = start.collapsed ? Self.collapsedHeight : height
        items[index].position.size = CGSize(width: width, height: height)
        items[index].position.x = start.x - old.width / 2 + width / 2
        items[index].position.y = start.y - old.height / 2 + shown / 2
    }

    func endResize() async {
        guard let before = dragStart else { return }
        dragStart = nil
        await commit("Resize Note", from: before)
    }

    /// Shows only the title of a note, or its content again.
    func toggleCollapsed(_ id: Node.ID) async {
        guard let index = items.firstIndex(where: { $0.id == id }) else { return }
        let before = snapshot()
        items[index].position.collapsed.toggle()
        await commit(items[index].position.collapsed ? "Collapse Note" : "Expand Note", from: before)
    }

    // MARK: Groups

    /// Adds a group around the selected notes, or at the centre of the screen.
    func addGroup(label: String = "Group") async {
        guard let view else { return }
        let chosen = items.filter { selection.contains($0.id) }
        let box: CGRect = chosen.isEmpty
            ? CGRect(origin: viewport.toPlane(CGPoint(x: size.width / 2 - 180, y: size.height / 2 - 120), in: size), size: CGSize(width: 360 / viewport.zoom, height: 240 / viewport.zoom))
            : chosen.map(frame(of:)).reduce(CGRect.null) { $0.union($1) }.insetBy(dx: -24, dy: -40)
        let before = snapshot()
        do {
            let group = try await store.setGroup(GroupDraft(label: label, x: box.midX, y: box.midY, width: box.width, height: box.height), in: view.id, space: spaceID)
            groups.append(group)
            for index in items.indices where selection.contains(items[index].id) {
                items[index].position.groupID = group.id
            }
            await commit("Add Group", from: before)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Moves a group and the notes in it, from where they were when the drag began.
    func moveGroup(_ id: ViewGroupInfo.ID, by delta: CGSize) {
        guard let start = dragStart, let origin = start.groups[id], let index = groups.firstIndex(where: { $0.id == id }) else { return }
        groups[index].x = origin.x + delta.width
        groups[index].y = origin.y + delta.height
        for item in items.indices where start.positions[items[item].id]?.groupID == id {
            guard let from = start.positions[items[item].id] else { continue }
            items[item].position.x = from.x + delta.width
            items[item].position.y = from.y + delta.height
        }
    }

    /// Resizes a group from its size when the drag began, keeping its top-left corner.
    func resizeGroup(_ id: ViewGroupInfo.ID, by delta: CGSize) {
        guard let origin = dragStart?.groups[id], let index = groups.firstIndex(where: { $0.id == id }) else { return }
        let width = max(160, origin.width + delta.width)
        let height = max(80, origin.height + delta.height)
        groups[index].width = width
        groups[index].height = height
        groups[index].x = origin.x - origin.width / 2 + width / 2
        groups[index].y = origin.y - origin.height / 2 + height / 2
    }

    func endGroupDrag(_ name: String = "Move Group") async {
        guard let before = dragStart else { return }
        dragStart = nil
        await commit(name, from: before)
    }

    func renameGroup(_ id: ViewGroupInfo.ID, to label: String) async {
        guard let index = groups.firstIndex(where: { $0.id == id }) else { return }
        let before = snapshot()
        groups[index].label = label.trimmingCharacters(in: .whitespacesAndNewlines)
        await commit("Rename Group", from: before)
    }

    func toggleGroupCollapsed(_ id: ViewGroupInfo.ID) async {
        guard let index = groups.firstIndex(where: { $0.id == id }) else { return }
        let before = snapshot()
        groups[index].collapsed.toggle()
        await commit(groups[index].collapsed ? "Collapse Group" : "Expand Group", from: before)
    }

    /// Removes a group; its notes stay on the board.
    func removeGroup(_ id: ViewGroupInfo.ID) async {
        let before = snapshot()
        groups.removeAll { $0.id == id }
        for index in items.indices where items[index].position.groupID == id {
            items[index].position.groupID = nil
        }
        await commit("Remove Group", from: before)
    }

    // MARK: Links

    /// Links two notes of the board with a kind.
    func link(from: Node.ID, to: Node.ID, kind: String) async {
        do {
            try await store.link(from: from, to: to, kind: kind, in: spaceID)
            await reloadEdges()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// The label of a link on the board, read from its source.
    func label(of edge: GraphEdgeInfo) -> String {
        linkKinds.first { $0.kind == edge.kind }?.label ?? edge.kind
    }

    private func reloadEdges() async {
        guard let board = try? await store.canvasBoard(of: nodeID, in: spaceID) else { return }
        edges = board.edges
        linkKinds = board.linkKinds
    }

    // MARK: Undo

    var undoName: String? { undoStack.last?.name }
    var redoName: String? { redoStack.last?.name }

    func undo() async {
        guard let edit = undoStack.popLast() else { return }
        redoStack.append(edit)
        await apply(edit.before, from: edit.after)
        updateUndoState()
    }

    func redo() async {
        guard let edit = redoStack.popLast() else { return }
        undoStack.append(edit)
        await apply(edit.after, from: edit.before)
        updateUndoState()
    }

    private func updateUndoState() {
        canUndo = !undoStack.isEmpty
        canRedo = !redoStack.isEmpty
    }

    // MARK: Saving

    func snapshot() -> Snapshot {
        Snapshot(
            positions: Dictionary(items.map { ($0.id, $0.position) }, uniquingKeysWith: { first, _ in first }),
            groups: Dictionary(groups.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        )
    }

    /// Saves what changed since `before` and makes it one edit that can be undone.
    private func commit(_ name: String, from before: Snapshot) async {
        let after = snapshot()
        guard after != before else { return }
        let edit = Edit(name: name, before: before, after: after)
        undoStack.append(edit)
        redoStack.removeAll()
        updateUndoState()
        registerUndo(name)
        await save(after, from: before)
    }

    private func registerUndo(_ name: String) {
        guard let undoManager else { return }
        undoManager.registerUndo(withTarget: self) { model in
            MainActor.assumeIsolated { model.undoFromManager() }
        }
        undoManager.setActionName(name)
    }

    /// Edit > Undo: registers the redo while the undo manager is undoing, then undoes.
    private func undoFromManager() {
        guard let edit = undoStack.last else { return }
        undoManager?.registerUndo(withTarget: self) { model in
            MainActor.assumeIsolated { model.redoFromManager() }
        }
        undoManager?.setActionName(edit.name)
        Task { await undo() }
    }

    private func redoFromManager() {
        guard let edit = redoStack.last else { return }
        registerUndo(edit.name)
        Task { await redo() }
    }

    /// Stops offering this board's edits in Edit > Undo, as when the board closes.
    func forgetUndo() {
        undoManager?.removeAllActions(withTarget: self)
    }

    /// Puts the board as it was in `target`, and saves it.
    private func apply(_ target: Snapshot, from current: Snapshot) async {
        groups = groups.compactMap { target.groups[$0.id] } + target.groups.values.filter { group in !groups.contains { $0.id == group.id } }.sorted { $0.id < $1.id }
        var kept: [CanvasItem] = []
        for var item in items {
            guard let position = target.positions[item.id] else {
                removedItems[item.id] = item
                continue
            }
            item.position = position
            kept.append(item)
        }
        // Notes an undo puts back come back with what the board knew of them.
        for (id, position) in target.positions where !kept.contains(where: { $0.id == id }) {
            let known = removedItems[id]
            kept.append(CanvasItem(position: position, kind: known?.kind ?? NodeKind.note, title: known?.title ?? "", excerpt: known?.excerpt ?? ""))
        }
        items = kept.sorted { ($0.position.z, $0.id) < ($1.position.z, $1.id) }
        await save(target, from: current)
        await reloadEdges()
    }

    /// What the board showed of notes taken off it, so an undo can show them again at once.
    @ObservationIgnored private var removedItems: [Node.ID: CanvasItem] = [:]

    /// Groups removed from the space in this session. A group an undo or a redo puts back is
    /// made again with a new id, as the space forgets a removed group.
    @ObservationIgnored private var removedGroups: Set<ViewGroupInfo.ID> = []

    /// Writes the difference between two states of the board: groups first, so positions can
    /// name them, then every changed position in one write.
    private func save(_ target: Snapshot, from before: Snapshot) async {
        guard let view else { return }
        var after = target
        do {
            var renamed: [ViewGroupInfo.ID: ViewGroupInfo.ID] = [:]
            for group in after.groups.values.sorted(by: { $0.id < $1.id }) where before.groups[group.id] != group {
                var draft = group.draft
                if removedGroups.contains(group.id) { draft.id = nil }
                let saved = try await store.setGroup(draft, in: view.id, space: spaceID)
                if saved.id != group.id { renamed[group.id] = saved.id }
            }
            if !renamed.isEmpty {
                renameGroups(renamed)
                after = after.renamingGroups(renamed)
            }
            let changed = after.positions.values.filter { before.positions[$0.nodeID] != $0 }
            try await store.setPositions(Array(changed), in: view.id, space: spaceID)
            let gone = before.positions.keys.filter { after.positions[$0] == nil }
            if !gone.isEmpty {
                try await store.removePositions(gone, from: view.id, space: spaceID)
            }
            for id in before.groups.keys where after.groups[id] == nil {
                try await store.removeGroup(id, in: spaceID)
                removedGroups.insert(id)
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Gives groups new ids everywhere: on the board and in the edits that can be undone.
    private func renameGroups(_ renamed: [ViewGroupInfo.ID: ViewGroupInfo.ID]) {
        for index in groups.indices {
            if let new = renamed[groups[index].id] { groups[index].id = new }
        }
        for index in items.indices {
            if let old = items[index].position.groupID, let new = renamed[old] { items[index].position.groupID = new }
        }
        let rename: (Edit) -> Edit = { edit in
            Edit(name: edit.name, before: edit.before.renamingGroups(renamed), after: edit.after.renamingGroups(renamed))
        }
        undoStack = undoStack.map(rename)
        redoStack = redoStack.map(rename)
    }

    private func scheduleViewportSave() {
        guard let view else { return }
        pendingViewportSave?.cancel()
        let viewport = viewport, space = spaceID, store = store, delay = saveDelay
        pendingViewportSave = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled else { return }
            do {
                try await store.setViewport(viewport, of: view.id, in: space)
            } catch {
                self?.errorMessage = error.localizedDescription
            }
        }
    }

    /// Saves the viewport now, if a save is pending.
    func flush() async {
        guard let pending = pendingViewportSave, let view else { return }
        pendingViewportSave = nil
        pending.cancel()
        try? await store.setViewport(viewport, of: view.id, in: spaceID)
    }
}
