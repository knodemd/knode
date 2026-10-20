import Foundation
import Observation

/// The graph of a space: its nodes and typed links laid out by force, filtered, searched, and
/// saved as named graph views with their viewport and positions.
@MainActor
@Observable
final class GraphModel {
    let spaceID: Space.ID

    /// The nodes shown, and their links.
    private(set) var nodes: [GraphNodeInfo] = []
    private(set) var edges: [GraphEdgeInfo] = []
    private(set) var linkKinds: [LinkKindInfo] = LinkKindInfo.builtIns
    /// The saved graph views of the space.
    private(set) var views: [MapView] = []
    /// The saved view shown, or nil for the whole space.
    private(set) var currentView: MapView?
    /// Which nodes and links are shown.
    var filter = GraphFilter() {
        didSet { if filter != oldValue, !suppressReload { Task { await reload() } } }
    }
    /// What can be filtered on: every kind, tag and author in the space.
    private(set) var facets = Facets()
    /// The part of the plane on screen.
    var viewport = Viewport() {
        didSet { if viewport != oldValue { scheduleViewportSave() } }
    }
    /// The size of the view on screen, set by the view.
    var size = CGSize(width: 800, height: 600)
    private(set) var isLoading = false
    var errorMessage: String?

    /// The node under the pointer, the link under it, and the node found by a search.
    var hovered: Int?
    var hoveredEdge: Int?
    private(set) var focused: Int?
    var searchText = "" {
        didSet { if searchText != oldValue { updateMatches() } }
    }
    private(set) var matches: [GraphNodeInfo] = []
    /// Whether the layout is still moving; the view draws every frame while it is.
    private(set) var isSimulating = false
    /// Bumped when the layout changes outside the animation, so the view draws again.
    private(set) var revision = 0
    /// The filter differs from the one saved in the current view.
    var hasUnsavedFilter: Bool { currentView.map { $0.filter != filter } ?? false }

    /// The layout, read by the view every frame; not observed.
    @ObservationIgnored private(set) var layout = GraphLayout(count: 0, links: [])
    /// Each edge's kind, by index in `linkKinds`.
    @ObservationIgnored private(set) var edgeKinds: [Int] = []
    @ObservationIgnored private(set) var index: [Node.ID: Int] = [:]
    /// Each node's neighbours, by index.
    @ObservationIgnored private(set) var neighbours: [[Int]] = []

    @ObservationIgnored private let store: any MapStore
    @ObservationIgnored private var dragging: Int?
    /// How each node is drawn, by index.
    @ObservationIgnored private(set) var glyphs: [NodeGlyph] = []
    /// The node being dragged.
    var isDragging: Int? { dragging }
    @ObservationIgnored private var positionsChanged = false
    @ObservationIgnored private var pendingViewportSave: Task<Void, Never>?
    @ObservationIgnored private var pendingReload: Task<Void, Never>?
    /// The viewport is set once the graph shows: the saved one, or one that fits every node.
    @ObservationIgnored private var needsViewport = true
    @ObservationIgnored private let saveDelay: Duration
    /// Opens a node, such as when it is clicked.
    @ObservationIgnored var onOpen: (@MainActor (Node.ID) -> Void)?
    /// How long one frame may spend stepping the layout.
    @ObservationIgnored var frameBudget: Duration = .milliseconds(8)

    struct Facets: Equatable {
        var kinds: [String] = []
        var tags: [String] = []
        var actors: [String] = []
    }

    init(spaceID: Space.ID, store: any MapStore, saveDelay: Duration = .seconds(1)) {
        self.spaceID = spaceID
        self.store = store
        self.saveDelay = saveDelay
    }

    // MARK: Loading

    /// Loads the views, what can be filtered on, and the graph.
    func load() async {
        do {
            views = try await store.mapViews(in: spaceID).filter { $0.kind == .graph }
            let all = try await store.graph(in: spaceID, view: nil, filter: GraphFilter())
            facets = Facets(
                kinds: Set(all.nodes.map(\.kind)).sorted(),
                tags: Set(all.nodes.flatMap(\.tags)).sorted(),
                actors: Set(all.nodes.flatMap { [$0.createdBy, $0.updatedBy] }).sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
            )
            if currentView == nil, filter.isEmpty {
                show(all)
                await warmUp()
            } else {
                await reload()
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Reads the graph again with the current view and filter, keeping where nodes are.
    func reload() async {
        isLoading = true
        defer { isLoading = false }
        do {
            let data = try await store.graph(in: spaceID, view: currentView?.id, filter: filter)
            show(data)
            await warmUp()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Reads the saved views again, after one was made, renamed or removed elsewhere.
    func reloadViews() async {
        guard let found = try? await store.mapViews(in: spaceID) else { return }
        views = found.filter { $0.kind == .graph }
    }

    /// Reloads once for a burst of changes made elsewhere.
    func reloadSoon() {
        guard pendingReload == nil else { return }
        pendingReload = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(300))
            self?.pendingReload = nil
            await self?.reload()
        }
    }

    private func show(_ data: GraphData) {
        // Nodes keep where they are on screen; saved positions win; new nodes start next to a
        // neighbour, or on the spiral.
        var known: [Node.ID: CGPoint] = [:]
        for (id, i) in index where i < layout.count { known[id] = layout.position(i) }
        for position in data.positions { known[position.nodeID] = position.center }
        nodes = data.nodes
        edges = data.edges
        linkKinds = data.linkKinds
        index = Dictionary(data.nodes.enumerated().map { ($1.id, $0) }, uniquingKeysWith: { first, _ in first })
        let links = data.edges.compactMap { edge -> (Int, Int)? in
            guard let s = index[edge.from], let t = index[edge.to] else { return nil }
            return (s, t)
        }
        var adjacency = [[Int]](repeating: [], count: nodes.count)
        for (s, t) in links where s != t {
            adjacency[s].append(t)
            adjacency[t].append(s)
        }
        neighbours = adjacency
        glyphs = nodes.map { NodeGlyph.of(kind: $0.kind) }
        var start: [Int: CGPoint] = [:]
        for (i, node) in nodes.enumerated() {
            if let point = known[node.id] { start[i] = point }
        }
        let placed = start.count
        for i in nodes.indices where start[i] == nil {
            if let neighbour = adjacency[i].first(where: { start[$0] != nil }), let near = start[neighbour] {
                let angle = Double(i) * 2.399963
                start[i] = CGPoint(x: near.x + 20 * cos(angle), y: near.y + 20 * sin(angle))
            }
        }
        var layout = GraphLayout(count: nodes.count, links: links, positions: start)
        let kindIndex = Dictionary(linkKinds.enumerated().map { ($1.kind, $0) }, uniquingKeysWith: { first, _ in first })
        edgeKinds = data.edges.filter { index[$0.from] != nil && index[$0.to] != nil }.map { kindIndex[$0.kind] ?? -1 }
        if placed == nodes.count {
            layout.alpha = 0
        } else if placed > 0 {
            layout.alpha = 0.5
        }
        self.layout = layout
        isSimulating = !layout.isSettled
        positionsChanged = placed < nodes.count
        hovered = nil
        hoveredEdge = nil
        focused = focused.flatMap { $0 < nodes.count ? $0 : nil }
        updateMatches()
        if needsViewport {
            if let saved = currentView?.viewport {
                needsViewport = false
                viewport = saved
            } else if layout.isSettled {
                needsViewport = false
                fit()
            }
        }
        revision += 1
    }

    // MARK: Simulation

    /// Steps the layout for one frame, within the frame budget. Returns whether it still moves.
    @discardableResult
    func advance() -> Bool {
        guard isSimulating else { return false }
        let clock = ContinuousClock()
        let start = clock.now
        repeat {
            layout.step()
        } while !layout.isSettled && clock.now - start < frameBudget / 2
        positionsChanged = true
        revision += 1
        if layout.isSettled {
            isSimulating = false
            if needsViewport {
                needsViewport = false
                fit()
            }
            Task { await savePositions() }
        }
        return isSimulating
    }

    /// A large graph takes its first steps off the main thread, so it opens already in shape.
    private func warmUp() async {
        guard isSimulating, nodes.count > 1500, layout.alpha > 0.5 else { return }
        await settle(maxSteps: 120)
    }

    /// Runs the layout to rest at once, off the main thread.
    func settle(maxSteps: Int = 400) async {
        var layout = self.layout
        let settled = await Task.detached(priority: .userInitiated) {
            layout.run(steps: maxSteps)
            return layout
        }.value
        guard settled.count == self.layout.count else { return }
        self.layout = settled
        isSimulating = !settled.isSettled
        positionsChanged = true
        if settled.isSettled, needsViewport {
            needsViewport = false
            fit()
        }
        revision += 1
    }

    // MARK: Pointer

    /// The node at a point of the screen, if any.
    func node(atScreen point: CGPoint) -> Int? {
        let plane = viewport.toPlane(point, in: size)
        return layout.node(near: plane, radius: max(8, 10 / viewport.zoom))
    }

    /// The link at a point of the screen, if any.
    func edge(atScreen point: CGPoint) -> Int? {
        let plane = viewport.toPlane(point, in: size)
        return layout.link(near: plane, radius: 5 / viewport.zoom)
    }

    /// Shows what is under the pointer.
    func hover(at point: CGPoint?) {
        guard let point else {
            hovered = nil
            hoveredEdge = nil
            return
        }
        let node = node(atScreen: point)
        hovered = node
        hoveredEdge = node == nil ? edge(atScreen: point) : nil
    }

    func beginDrag(_ node: Int) {
        dragging = node
        layout.alphaTarget = 0.3
        layout.reheat()
        isSimulating = true
    }

    func drag(to point: CGPoint) {
        guard let dragging else { return }
        layout.place(dragging, at: viewport.toPlane(point, in: size), fixed: true)
        revision += 1
    }

    func endDrag() {
        guard let dragging else { return }
        layout.place(dragging, at: layout.position(dragging), fixed: false)
        layout.alphaTarget = 0
        self.dragging = nil
        positionsChanged = true
    }

    /// Opens a node: a click on it.
    func open(_ node: Int) {
        guard nodes.indices.contains(node) else { return }
        onOpen?(nodes[node].id)
    }

    func pan(by translation: CGSize) {
        viewport = viewport.panned(by: translation)
    }

    func zoom(by factor: Double, at anchor: CGPoint? = nil) {
        viewport = viewport.zoomed(by: factor, at: anchor ?? CGPoint(x: size.width / 2, y: size.height / 2), in: size)
    }

    /// Shows every node.
    func fit() {
        viewport = Viewport.fitting(layout.bounds, in: size)
    }

    // MARK: Search

    private func updateMatches() {
        let query = searchText.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else {
            matches = []
            return
        }
        matches = Array(nodes.lazy.filter { $0.title.localizedCaseInsensitiveContains(query) }.prefix(8))
    }

    /// Centres the view on a node and marks it.
    func focus(_ id: Node.ID) {
        guard let node = index[id] else { return }
        focused = node
        let point = layout.position(node)
        viewport = Viewport(x: point.x, y: point.y, zoom: max(viewport.zoom, 1.2))
        revision += 1
    }

    /// Focuses the first match of the search.
    func focusFirstMatch() {
        if let first = matches.first { focus(first.id) }
    }

    func clearFocus() {
        focused = nil
        revision += 1
    }

    // MARK: Filters

    func toggle(_ value: String, in keyPath: WritableKeyPath<GraphFilter, [String]?>) {
        var list = filter[keyPath: keyPath] ?? []
        if let at = list.firstIndex(of: value) { list.remove(at: at) } else { list.append(value) }
        filter[keyPath: keyPath] = list.isEmpty ? nil : list.sorted()
    }

    func isOn(_ value: String, in keyPath: KeyPath<GraphFilter, [String]?>) -> Bool {
        filter[keyPath: keyPath]?.contains(value) ?? false
    }

    /// Shows only nodes changed in the last `days` days, or any time with nil.
    func showChanged(inLast days: Int?, now: Date = Date()) {
        filter.since = days.map { now.addingTimeInterval(-Double($0) * 86_400) }
        filter.until = nil
    }

    // MARK: Saved views

    /// Shows a saved view, or the whole space with nil.
    func select(_ view: MapView?) async {
        await savePositions()
        currentView = view
        needsViewport = true
        let wanted = view?.filter ?? GraphFilter()
        if filter != wanted {
            // Set without the reload the observer starts: one reload below is enough.
            withoutReload { filter = wanted }
        }
        await reload()
    }

    /// Saves what is shown as a new named view: its filter, viewport and positions.
    func saveAsNewView(named name: String) async {
        do {
            let view = try await store.saveGraphView(id: nil, name: name, filter: filter, in: spaceID)
            currentView = view
            positionsChanged = true
            await savePositions()
            try await store.setViewport(viewport, of: view.id, in: spaceID)
            currentView?.viewport = viewport
            views = try await store.mapViews(in: spaceID).filter { $0.kind == .graph }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Saves the current filter in the current view.
    func updateCurrentView() async {
        guard let view = currentView else { return }
        await rename(view, to: view.name)
    }

    func rename(_ view: MapView, to name: String) async {
        do {
            let filter = view.id == currentView?.id ? self.filter : view.filter
            let saved = try await store.saveGraphView(id: view.id, name: name, filter: filter, in: spaceID)
            if currentView?.id == saved.id { currentView = saved }
            views = try await store.mapViews(in: spaceID).filter { $0.kind == .graph }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func delete(_ view: MapView) async {
        do {
            try await store.removeGraphView(view.id, in: spaceID)
            views.removeAll { $0.id == view.id }
            if currentView?.id == view.id {
                currentView = nil
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Saves where every node is in the current view, if anything moved. Adds no change.
    func savePositions() async {
        guard let view = currentView, positionsChanged, !nodes.isEmpty else { return }
        positionsChanged = false
        let positions = nodes.indices.map { i in
            let point = layout.position(i)
            return ViewPosition(nodeID: nodes[i].id, x: point.x, y: point.y)
        }
        do {
            try await store.setPositions(positions, in: view.id, space: spaceID)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func scheduleViewportSave() {
        guard let view = currentView else { return }
        pendingViewportSave?.cancel()
        let viewport = viewport, space = spaceID, store = store, delay = saveDelay
        pendingViewportSave = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled else { return }
            do {
                try await store.setViewport(viewport, of: view.id, in: space)
                self?.currentView?.viewport = viewport
            } catch {
                self?.errorMessage = error.localizedDescription
            }
        }
    }

    /// Saves what is pending now, such as before the view closes.
    func flush() async {
        if let pending = pendingViewportSave {
            pendingViewportSave = nil
            pending.cancel()
            if let view = currentView {
                try? await store.setViewport(viewport, of: view.id, in: spaceID)
            }
        }
        await savePositions()
    }

    @ObservationIgnored private var suppressReload = false

    private func withoutReload(_ change: () -> Void) {
        suppressReload = true
        change()
        suppressReload = false
    }

    // MARK: Drawing helpers

    /// The kind of link of edge `k`, if the space knows it.
    func linkKind(ofEdge k: Int) -> LinkKindInfo? {
        guard edgeKinds.indices.contains(k), edgeKinds[k] >= 0 else { return nil }
        return linkKinds[edgeKinds[k]]
    }

    /// The nodes to label: those near the pointer, the focus and their neighbours, and at a high
    /// enough zoom every node on screen, up to `limit`.
    func labelled(limit: Int = 160) -> [Int] {
        var chosen: [Int] = []
        var seen = Set<Int>()
        func add(_ i: Int) { if seen.insert(i).inserted { chosen.append(i) } }
        for i in [hovered, focused].compactMap({ $0 }) where i < nodes.count {
            add(i)
            for n in neighbours[i].prefix(24) { add(n) }
        }
        if let edge = hoveredEdge, edge < layout.sources.count {
            add(Int(layout.sources[edge]))
            add(Int(layout.targets[edge]))
        }
        guard viewport.zoom >= 0.9 else { return chosen }
        let visible = CGRect(origin: viewport.toPlane(.zero, in: size), size: CGSize(width: size.width / viewport.zoom, height: size.height / viewport.zoom))
        for i in 0..<layout.count where chosen.count < limit && visible.contains(layout.position(i)) {
            add(i)
        }
        return chosen
    }
}
