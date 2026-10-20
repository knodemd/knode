import KnodeBrand
import SwiftUI

/// The graph of a space: every node and typed link, laid out by force. Drag the background to pan,
/// pinch or scroll with Command to zoom, drag a node to move it, click one to open it.
struct GraphScreen: View {
    @Bindable var graph: GraphModel
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            GraphBar(graph: graph)
            Divider()
            ZStack(alignment: .bottomLeading) {
                GraphCanvas(graph: graph)
                if graph.nodes.isEmpty, !graph.isLoading {
                    ContentUnavailableView(
                        graph.filter.isEmpty ? "No Notes Yet" : "Nothing Matches",
                        systemImage: "point.3.connected.trianglepath.dotted",
                        description: Text(graph.filter.isEmpty ? "Notes and their links appear here." : "No note passes every filter.")
                    )
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                GraphLegend(graph: graph)
                    .padding(12)
            }
        }
        .background(MapInks(scheme).background)
        .alert(
            "Something went wrong",
            isPresented: Binding(get: { graph.errorMessage != nil }, set: { if !$0 { graph.errorMessage = nil } }),
            actions: { Button("OK") { graph.errorMessage = nil } },
            message: { Text(graph.errorMessage ?? "") }
        )
        .onDisappear { Task { await graph.flush() } }
    }
}

/// Draws the graph and takes the pointer.
struct GraphCanvas: View {
    @Bindable var graph: GraphModel
    @Environment(\.colorScheme) private var scheme
    @State private var gesture: DragKind?
    @State private var startViewport: Viewport?
    @State private var magnifyStart: Viewport?

    private enum DragKind { case node(Int), pan }

    var body: some View {
        GeometryReader { proxy in
            Canvas(rendersAsynchronously: false) { context, size in
                draw(in: &context, size: size)
            }
            .onAppear { graph.size = proxy.size }
            .onChange(of: proxy.size) { _, size in graph.size = size }
        }
        .contentShape(Rectangle())
        .gesture(drag)
        .simultaneousGesture(magnify)
        .onContinuousHover { phase in
            switch phase {
            case .active(let point):
                lastHover = point
                graph.hover(at: point)
                if graph.hovered != nil { NSCursor.pointingHand.set() } else { NSCursor.arrow.set() }
            case .ended:
                lastHover = nil
                graph.hover(at: nil)
                NSCursor.arrow.set()
            }
        }
        .onScrollWheel { event in
            if event.modifierFlags.contains(.command) || !event.hasPreciseScrollingDeltas {
                let factor = pow(1.0025, event.scrollingDeltaY * (event.hasPreciseScrollingDeltas ? 1 : 8))
                graph.zoom(by: factor, at: lastHover)
            } else {
                graph.pan(by: CGSize(width: -event.scrollingDeltaX, height: -event.scrollingDeltaY))
            }
            return true
        }
        .task(id: graph.isSimulating) {
            while graph.isSimulating, !Task.isCancelled {
                graph.advance()
                try? await Task.sleep(for: .milliseconds(16))
            }
        }
        .accessibilityElement()
        .accessibilityLabel("Graph of \(graph.nodes.count) notes and \(graph.edges.count) links")
    }

    /// The pointer, to zoom around it.
    @State private var lastHover: CGPoint?

    private var drag: some Gesture {
        DragGesture(minimumDistance: 0)
            .onChanged { value in
                if gesture == nil {
                    if let node = graph.node(atScreen: value.startLocation) {
                        gesture = .node(node)
                    } else {
                        gesture = .pan
                        startViewport = graph.viewport
                    }
                }
                let moved = hypot(value.translation.width, value.translation.height) > 3
                switch gesture {
                case .node(let node):
                    guard moved else { return }
                    if graph.isDragging != node { graph.beginDrag(node) }
                    graph.drag(to: value.location)
                case .pan:
                    if let startViewport { graph.viewport = startViewport.panned(by: value.translation) }
                case nil:
                    break
                }
            }
            .onEnded { value in
                let moved = hypot(value.translation.width, value.translation.height) > 3
                switch gesture {
                case .node(let node):
                    if moved { graph.endDrag() } else { graph.open(node) }
                case .pan:
                    if !moved { graph.clearFocus() }
                case nil:
                    break
                }
                gesture = nil
                startViewport = nil
            }
    }

    private var magnify: some Gesture {
        MagnifyGesture()
            .onChanged { value in
                if magnifyStart == nil { magnifyStart = graph.viewport }
                guard let start = magnifyStart else { return }
                graph.viewport = start.zoomed(by: value.magnification, at: value.startLocation, in: graph.size)
            }
            .onEnded { _ in magnifyStart = nil }
    }

    // MARK: Drawing

    private func draw(in context: inout GraphicsContext, size: CGSize) {
        _ = graph.revision
        let inks = MapInks(scheme)
        let layout = graph.layout
        let viewport = graph.viewport
        let zoom = viewport.zoom
        let n = layout.count
        guard n > 0 else { return }
        let visible = CGRect(origin: .zero, size: size).insetBy(dx: -20, dy: -20)
        var screen = [CGPoint](repeating: .zero, count: n)
        for i in 0..<n { screen[i] = viewport.toScreen(layout.position(i), in: size) }

        // Links: `mentions` thin, typed links stronger; those of the node under the pointer in
        // indigo.
        let highlight = graph.hovered ?? graph.focused
        var mentions = Path(), typed = Path(), lit = Path()
        let mentionsIndex = graph.linkKinds.firstIndex { $0.kind == LinkKindInfo.mentions }
        for k in 0..<layout.sources.count {
            let s = Int(layout.sources[k]), t = Int(layout.targets[k])
            let a = screen[s], b = screen[t]
            if !visible.contains(a), !visible.contains(b), !visible.intersects(CGRect(x: min(a.x, b.x), y: min(a.y, b.y), width: abs(a.x - b.x) + 1, height: abs(a.y - b.y) + 1)) { continue }
            if s == highlight || t == highlight || k == graph.hoveredEdge {
                lit.move(to: a)
                lit.addLine(to: b)
            } else if k < graph.edgeKinds.count, graph.edgeKinds[k] == mentionsIndex {
                mentions.move(to: a)
                mentions.addLine(to: b)
            } else {
                typed.move(to: a)
                typed.addLine(to: b)
            }
        }
        let width = min(1.2, max(0.4, zoom))
        context.stroke(mentions, with: .color(inks.foreground.opacity(0.14)), lineWidth: width * 0.8)
        context.stroke(typed, with: .color(inks.foreground.opacity(0.32)), style: StrokeStyle(lineWidth: width, dash: [4, 3]))
        context.stroke(lit, with: .color(inks.indigo), lineWidth: max(1.2, width * 1.6))

        // Nodes, one path per glyph.
        var paths: [NodeGlyph: Path] = [:]
        let base = max(1.6, min(7, 4.2 * zoom))
        for i in 0..<n where visible.contains(screen[i]) {
            let glyph = graph.glyphs[i]
            let degree = Double(layout.degree[i])
            let r = base * (1 + min(degree, 36).squareRoot() * 0.18)
            glyph.add(to: &paths[glyph, default: Path()], at: screen[i], radius: r)
        }
        for (glyph, path) in paths {
            let ink = glyph.indigo ? inks.indigo : inks.foreground
            if glyph.filled {
                context.fill(path, with: .color(ink))
            } else {
                context.fill(path, with: .color(inks.background))
                context.stroke(path, with: .color(ink), lineWidth: 1.2)
            }
        }

        // The focus and the node under the pointer.
        for (index, ring) in [(graph.focused, 9.0), (graph.hovered, 7.0)] {
            guard let index, index < n else { continue }
            let p = screen[index]
            let r = base + ring
            context.stroke(Path(ellipseIn: CGRect(x: p.x - r, y: p.y - r, width: 2 * r, height: 2 * r)), with: .color(inks.indigo), lineWidth: 2)
        }

        // Titles.
        for i in graph.labelled() where i < n && visible.contains(screen[i]) {
            let strong = i == graph.hovered || i == graph.focused
            let text = Text(graph.nodes[i].title)
                .font(.system(size: strong ? 12 : 10.5, weight: strong ? .semibold : .regular))
                .foregroundStyle(strong ? inks.foreground : inks.foreground.opacity(0.7))
            context.draw(text, at: CGPoint(x: screen[i].x, y: screen[i].y + base + 9), anchor: .top)
        }

        // Link labels: of the link under the pointer, or of every link of the node under it.
        var labelled: [Int] = []
        if let edge = graph.hoveredEdge { labelled = [edge] }
        if let node = graph.hovered {
            labelled = (0..<layout.sources.count).filter { Int(layout.sources[$0]) == node || Int(layout.targets[$0]) == node }.prefix(30).map { $0 }
        }
        for k in labelled {
            let s = Int(layout.sources[k]), t = Int(layout.targets[k])
            let mid = CGPoint(x: (screen[s].x + screen[t].x) / 2, y: (screen[s].y + screen[t].y) / 2)
            let kind = graph.linkKind(ofEdge: k)
            let label = kind?.label ?? graph.edges[k].kind
            let resolved = context.resolve(Text(label).font(.knodeLine(10)).foregroundStyle(KnodeColor.ink))
            let textSize = resolved.measure(in: CGSize(width: 240, height: 40))
            let box = CGRect(x: mid.x - textSize.width / 2 - 5, y: mid.y - textSize.height / 2 - 2, width: textSize.width + 10, height: textSize.height + 4)
            context.fill(Path(roundedRect: box, cornerRadius: box.height / 2), with: .color(inks.indigo))
            context.draw(resolved, at: mid)
        }
    }
}

/// The bar above the graph: saved views, filters, search, zoom.
struct GraphBar: View {
    @Bindable var graph: GraphModel
    @State private var isNamingView = false
    @State private var isRenamingView = false
    @State private var viewName = ""
    @FocusState private var searchFocused: Bool

    var body: some View {
        HStack(spacing: 10) {
            viewsMenu
            filterMenu
            searchField
            Spacer(minLength: 8)
            Text("\(graph.nodes.count.formatted()) notes, \(graph.edges.count.formatted()) links")
                .font(.caption)
                .foregroundStyle(.secondary)
                .monospacedDigit()
            ControlGroup {
                Button("Zoom Out", systemImage: "minus.magnifyingglass") { graph.zoom(by: 1 / 1.4) }
                Button("Zoom In", systemImage: "plus.magnifyingglass") { graph.zoom(by: 1.4) }
                Button("Fit", systemImage: "arrow.up.left.and.down.right.magnifyingglass") { graph.fit() }
            }
            .labelStyle(.iconOnly)
            .controlSize(.small)
            .fixedSize()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .alert("Save View", isPresented: $isNamingView) {
            TextField("Name", text: $viewName)
            Button("Save") { Task { await graph.saveAsNewView(named: viewName) } }
                .disabled(viewName.trimmingCharacters(in: .whitespaces).isEmpty)
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Saves the filter, where every note is and what is on screen.")
        }
        .alert("Rename View", isPresented: $isRenamingView) {
            TextField("Name", text: $viewName)
            Button("Rename") {
                if let view = graph.currentView { Task { await graph.rename(view, to: viewName) } }
            }
            .disabled(viewName.trimmingCharacters(in: .whitespaces).isEmpty)
            Button("Cancel", role: .cancel) {}
        }
    }

    private var viewsMenu: some View {
        Menu {
            Button("Whole Space", systemImage: graph.currentView == nil ? "checkmark" : "circle.dashed") {
                Task { await graph.select(nil) }
            }
            if !graph.views.isEmpty {
                Section("Saved Views") {
                    ForEach(graph.views) { view in
                        Button(view.name, systemImage: graph.currentView?.id == view.id ? "checkmark" : "square.on.square") {
                            Task { await graph.select(view) }
                        }
                    }
                }
            }
            Divider()
            Button("Save as New View…", systemImage: "plus.square.on.square") {
                viewName = ""
                isNamingView = true
            }
            if let current = graph.currentView {
                Button("Save Filter to “\(current.name)”", systemImage: "square.and.arrow.down") {
                    Task { await graph.updateCurrentView() }
                }
                .disabled(!graph.hasUnsavedFilter)
                Button("Rename “\(current.name)”…", systemImage: "pencil") {
                    viewName = current.name
                    isRenamingView = true
                }
                Button("Delete “\(current.name)”", systemImage: "trash", role: .destructive) {
                    Task { await graph.delete(current) }
                }
            }
        } label: {
            Label(graph.currentView?.name ?? "Whole Space", systemImage: "square.on.square")
        }
        .fixedSize()
        .help("Saved graph views")
    }

    private var filterMenu: some View {
        Menu {
            Section("Kinds") {
                ForEach(graph.facets.kinds, id: \.self) { kind in
                    Toggle(isOn: Binding(get: { graph.isOn(kind, in: \.nodeKinds) }, set: { _ in graph.toggle(kind, in: \.nodeKinds) })) {
                        Text(kind.capitalized)
                    }
                }
            }
            Menu("Links", systemImage: "link") {
                ForEach(graph.linkKinds) { kind in
                    Toggle(kind.label, isOn: Binding(get: { graph.isOn(kind.kind, in: \.linkKinds) }, set: { _ in graph.toggle(kind.kind, in: \.linkKinds) }))
                }
            }
            if !graph.facets.tags.isEmpty {
                Menu("Tags", systemImage: "tag") {
                    ForEach(graph.facets.tags, id: \.self) { tag in
                        Toggle(tag, isOn: Binding(get: { graph.isOn(tag, in: \.tags) }, set: { _ in graph.toggle(tag, in: \.tags) }))
                    }
                }
            }
            Menu("People and Agents", systemImage: "person.2") {
                ForEach(graph.facets.actors, id: \.self) { actor in
                    Toggle(actor, isOn: Binding(get: { graph.isOn(actor, in: \.actors) }, set: { _ in graph.toggle(actor, in: \.actors) }))
                }
            }
            Menu("Changed", systemImage: "calendar") {
                Button("Any Time", systemImage: "clock") { graph.showChanged(inLast: nil) }
                Button("Today", systemImage: "sun.max") { graph.showChanged(inLast: 1) }
                Button("Past 7 Days", systemImage: "calendar") { graph.showChanged(inLast: 7) }
                Button("Past 30 Days", systemImage: "calendar") { graph.showChanged(inLast: 30) }
                Button("Past Year", systemImage: "calendar") { graph.showChanged(inLast: 365) }
            }
            Divider()
            Button("Clear Filters", systemImage: "xmark.circle") { graph.filter = GraphFilter() }
                .disabled(graph.filter.isEmpty)
        } label: {
            Label(graph.filter.count == 0 ? "Filter" : "Filter (\(graph.filter.count))", systemImage: graph.filter.isEmpty ? "line.3.horizontal.decrease.circle" : "line.3.horizontal.decrease.circle.fill")
        }
        .fixedSize()
        .help("Show only some kinds, tags, people, agents, dates or links")
    }

    private var searchField: some View {
        TextField("Find a note", text: $graph.searchText)
            .textFieldStyle(.roundedBorder)
            .frame(width: 200)
            .focused($searchFocused)
            .onSubmit { graph.focusFirstMatch() }
            .popover(isPresented: Binding(get: { searchFocused && !graph.matches.isEmpty }, set: { if !$0 { searchFocused = false } }), arrowEdge: .bottom) {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(graph.matches) { node in
                        Button {
                            graph.focus(node.id)
                        } label: {
                            Label {
                                Text(node.title).lineLimit(1)
                            } icon: {
                                NodeGlyphView(kind: node.kind)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .buttonStyle(.borderless)
                        .padding(.vertical, 3)
                    }
                }
                .padding(10)
                .frame(width: 260)
            }
            .help("Type to find a note on the graph; Return centres it")
    }
}

/// Which glyph is which kind.
struct GraphLegend: View {
    let graph: GraphModel

    var body: some View {
        let kinds = graph.facets.kinds.filter { kind in graph.nodes.contains { $0.kind == kind } }
        if !kinds.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(kinds, id: \.self) { kind in
                    HStack(spacing: 6) {
                        NodeGlyphView(kind: kind)
                        Text(kind.capitalized)
                    }
                }
                HStack(spacing: 6) {
                    Rectangle().frame(width: 12, height: 1).opacity(0.4)
                    Text("Mentions")
                }
                HStack(spacing: 6) {
                    Rectangle().frame(width: 12, height: 1).opacity(0.8)
                    Text("Typed link")
                }
            }
            .font(.caption)
            .padding(8)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
        }
    }
}
