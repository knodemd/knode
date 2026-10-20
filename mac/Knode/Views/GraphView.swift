import SwiftUI

/// The graph of a space: every node and typed link, laid out by force. Drag the background to pan,
/// pinch or scroll with Command to zoom, drag a node to move it, click one to open it.
struct GraphScreen: View {
    @Bindable var graph: GraphModel
    @Environment(\.kTheme) private var theme
    @State private var naming: GraphViewNaming?
    @State private var viewName = ""

    var body: some View {
        VStack(spacing: 0) {
            GraphBar(graph: graph, naming: $naming, viewName: $viewName)
            Rectangle().fill(theme.line).frame(height: 1)
            ZStack {
                GraphCanvas(graph: graph)
                if graph.nodes.isEmpty, !graph.isLoading {
                    KEmptyState(
                        icon: .map,
                        title: graph.filter.isEmpty ? "No notes yet" : "Nothing matches",
                        message: graph.filter.isEmpty ? "Notes and their links appear here." : "No note passes every filter."
                    ) {
                        if !graph.filter.isEmpty {
                            KButton("Clear filters", icon: .close) { graph.filter = GraphFilter() }
                        }
                    }
                    .allowsHitTesting(!graph.filter.isEmpty)
                }
            }
            .overlay(alignment: .topLeading) {
                GraphFinder(graph: graph).padding(KSpace.x3)
            }
            .overlay(alignment: .bottomLeading) {
                GraphLegend(graph: graph).padding(KSpace.x3)
            }
            .overlay(alignment: .bottomTrailing) {
                MapZoomControls(
                    zoomOut: { graph.zoom(by: 1 / 1.4) },
                    zoomIn: { graph.zoom(by: 1.4) },
                    fit: { graph.fit() }
                )
                .padding(KSpace.x3)
            }
            .overlay(alignment: .top) {
                if let error = graph.errorMessage {
                    KBanner(.error, title: "Something went wrong", message: error, onClose: { graph.errorMessage = nil })
                        .frame(maxWidth: 520)
                        .padding(KSpace.x3)
                }
            }
        }
        .background(theme.bg)
        .kSheet(item: $naming, width: 400) { kind in
            GraphViewNameSheet(kind: kind, name: $viewName) {
                naming = nil
            } save: {
                let name = viewName
                naming = nil
                switch kind {
                case .new:
                    Task { await graph.saveAsNewView(named: name) }
                case .rename:
                    if let view = graph.currentView { Task { await graph.rename(view, to: name) } }
                }
            }
        }
        .onDisappear { Task { await graph.flush() } }
        .knodeTheme()
    }
}

/// Naming a saved view: a new one, or the current one again.
enum GraphViewNaming: Identifiable {
    case new, rename
    var id: Self { self }
}

/// The sheet that names a saved view.
private struct GraphViewNameSheet: View {
    let kind: GraphViewNaming
    @Binding var name: String
    var cancel: () -> Void
    var save: () -> Void

    private var isEmpty: Bool { name.trimmingCharacters(in: .whitespaces).isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x5) {
            KSheetHeader(
                kicker: "Graph view",
                title: kind == .new ? "Save view" : "Rename view",
                message: kind == .new ? "Saves the filter, where every note is and what is on screen." : nil
            )
            KTextField("Name", text: $name) { if !isEmpty { save() } }
            HStack(spacing: KSpace.x2) {
                Spacer()
                KButton("Cancel", kind: .secondary, action: cancel)
                Button {
                    save()
                } label: {
                    HStack(spacing: KSpace.x2) {
                        KIcon(kind == .new ? .save : .edit, size: .sm)
                        Text(kind == .new ? "Save" : "Rename")
                    }
                }
                .buttonStyle(KButtonStyle(kind: .primary))
                .keyboardShortcut(.defaultAction)
                .disabled(isEmpty)
            }
        }
    }
}

/// Zoom out, zoom in and fit, stacked in a small card in a corner of a map.
struct MapZoomControls: View {
    var zoomOut: () -> Void
    var zoomIn: () -> Void
    var fit: () -> Void

    var body: some View {
        VStack(spacing: 2) {
            KIconButton(.zoomIn, title: "Zoom in", size: .small, action: zoomIn)
            KIconButton(.zoomOut, title: "Zoom out", size: .small, action: zoomOut)
            KIconButton(.expand, title: "Fit everything", size: .small, action: fit)
        }
        .padding(3)
        .mapChrome(radius: KRadius.row)
    }
}

/// Draws the graph and takes the pointer.
struct GraphCanvas: View {
    @Bindable var graph: GraphModel
    @Environment(\.kTheme) private var theme
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
        let inks = MapInks(theme)
        let layout = graph.layout
        let viewport = graph.viewport
        let zoom = viewport.zoom
        let n = layout.count
        guard n > 0 else { return }
        let visible = CGRect(origin: .zero, size: size).insetBy(dx: -20, dy: -20)
        var screen = [CGPoint](repeating: .zero, count: n)
        for i in 0..<n { screen[i] = viewport.toScreen(layout.position(i), in: size) }

        // Links: `mentions` in the hairline colour, typed links in indigo; those of the node under
        // the pointer in full indigo.
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
        let width = min(1.4, max(0.6, zoom))
        context.stroke(mentions, with: .color(inks.line), lineWidth: width)
        context.stroke(typed, with: .color(inks.indigo.opacity(theme.isDark ? 0.55 : 0.7)), lineWidth: width)
        context.stroke(lit, with: .color(inks.indigo), lineWidth: max(1.4, width * 1.6))

        // Nodes, one path per glyph.
        var paths: [NodeGlyph: Path] = [:]
        let base = max(2, min(7.5, 4.6 * zoom))
        for i in 0..<n where visible.contains(screen[i]) {
            let glyph = graph.glyphs[i]
            let degree = Double(layout.degree[i])
            let r = base * (1 + min(degree, 36).squareRoot() * 0.18)
            glyph.add(to: &paths[glyph, default: Path()], at: screen[i], radius: r)
        }
        for (glyph, path) in paths {
            inks.draw(glyph, path, in: &context, lineWidth: max(1, min(1.6, zoom * 1.3)))
        }

        // The focus and the node under the pointer.
        for (index, ring) in [(graph.focused, 9.0), (graph.hovered, 7.0)] {
            guard let index, index < n else { continue }
            let p = screen[index]
            let r = base + ring
            let circle = Path(ellipseIn: CGRect(x: p.x - r, y: p.y - r, width: 2 * r, height: 2 * r))
            context.fill(circle, with: .color(inks.indigo.opacity(0.12)))
            context.stroke(circle, with: .color(inks.indigo), lineWidth: 1.5)
        }

        // Titles.
        for i in graph.labelled() where i < n && visible.contains(screen[i]) {
            let strong = i == graph.hovered || i == graph.focused
            let text = Text(graph.nodes[i].title)
                .font(KFont.ui(strong ? 12 : 11, strong ? .semibold : .regular))
                .foregroundStyle(strong ? inks.foreground : inks.muted)
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
            MapLinkLabel.draw(label, at: mid, typed: kind?.kind != LinkKindInfo.mentions, inks: inks, in: &context)
        }
    }
}

/// The label of a link on a map: Knode Line in a pill, indigo for typed links.
enum MapLinkLabel {
    static func draw(_ label: String, at point: CGPoint, typed: Bool, inks: MapInks, in context: inout GraphicsContext) {
        let ink = Color(hex: KPalette.dark.bg)
        let resolved = context.resolve(Text(label.uppercased()).font(KFont.line(10)).tracking(0.2).foregroundStyle(typed ? ink : inks.muted))
        let textSize = resolved.measure(in: CGSize(width: 240, height: 40))
        let box = CGRect(x: point.x - textSize.width / 2 - 6, y: point.y - textSize.height / 2 - 3, width: textSize.width + 12, height: textSize.height + 6)
        let pill = Path(roundedRect: box, cornerRadius: box.height / 2)
        if typed {
            context.fill(pill, with: .color(inks.indigo))
        } else {
            context.fill(pill, with: .color(inks.panel))
            context.stroke(pill, with: .color(inks.line), lineWidth: 1)
        }
        context.draw(resolved, at: point)
    }
}

/// The bar above the graph: saved views, filters and how much is shown.
struct GraphBar: View {
    @Bindable var graph: GraphModel
    @Binding var naming: GraphViewNaming?
    @Binding var viewName: String
    @Environment(\.kTheme) private var theme
    @State private var isFiltering = false

    var body: some View {
        HStack(spacing: KSpace.x2) {
            viewsMenu
            filterButton
            if !graph.filter.isEmpty {
                KButton("Clear", icon: .close, kind: .ghost, size: .small) { graph.filter = GraphFilter() }
                    .kTooltip("Show every note again")
            }
            Spacer(minLength: KSpace.x2)
            if graph.isLoading { KSpinner(size: 12) }
            Text("\(graph.nodes.count.formatted()) notes  \(graph.edges.count.formatted()) links")
                .kLineStyle(10)
                .foregroundStyle(theme.muted)
                .monospacedDigit()
        }
        .padding(.horizontal, KSpace.x3)
        .frame(height: 44)
        .background(theme.bg)
    }

    private var viewsMenu: some View {
        KMenuButton(style: .secondary, entries: viewEntries) {
            HStack(spacing: 6) {
                KIcon(.grid, size: .xs)
                Text(graph.currentView?.name ?? "Whole space")
                KIcon(.down, size: .xs, color: theme.muted)
            }
        }
        .fixedSize()
        .kTooltip("Saved graph views")
    }

    private func viewEntries() -> [KMenuEntry] {
        var entries: [KMenuEntry] = [
            .action("Whole space", icon: .map, isChecked: graph.currentView == nil) { Task { await graph.select(nil) } }
        ]
        if !graph.views.isEmpty {
            entries.append(.section("Saved views"))
            for view in graph.views {
                entries.append(.action(view.name, icon: .grid, isChecked: graph.currentView?.id == view.id) {
                    Task { await graph.select(view) }
                })
            }
        }
        entries.append(.divider)
        entries.append(.action("Save as new view", icon: .add) {
            viewName = ""
            naming = .new
        })
        if let current = graph.currentView {
            entries.append(.action("Save filter to \u{201C}\(current.name)\u{201D}", icon: .save, isEnabled: graph.hasUnsavedFilter) {
                Task { await graph.updateCurrentView() }
            })
            entries.append(.action("Rename \u{201C}\(current.name)\u{201D}", icon: .edit) {
                viewName = current.name
                naming = .rename
            })
            entries.append(.action("Delete \u{201C}\(current.name)\u{201D}", icon: .delete, isDestructive: true) {
                Task { await graph.delete(current) }
            })
        }
        return entries
    }

    private var filterButton: some View {
        Button { isFiltering.toggle() } label: {
            HStack(spacing: 6) {
                KIcon(.filter, size: .xs, color: graph.filter.isEmpty ? nil : (theme.isDark ? theme.indigo : theme.fg))
                Text("Filter")
                if graph.filter.count > 0 {
                    Text("\(graph.filter.count)")
                        .font(KFont.ui(10, .semibold))
                        .foregroundStyle(theme.onAccent)
                        .frame(minWidth: 16, minHeight: 16)
                        .background(Capsule().fill(theme.isDark ? theme.indigo : theme.accent))
                }
            }
        }
        .buttonStyle(KButtonStyle(kind: .secondary, size: .small))
        .fixedSize()
        .kTooltip("Show only some kinds, tags, people, agents, dates or links")
        .kPopover(isPresented: $isFiltering) {
            GraphFilterPanel(graph: graph)
        }
    }
}

/// The filters of the graph: kinds, links, tags, people and agents, and when notes changed.
struct GraphFilterPanel: View {
    @Bindable var graph: GraphModel
    @Environment(\.kTheme) private var theme

    private static let periods: [(days: Int?, title: String)] = [(nil, "Any time"), (1, "Today"), (7, "7 days"), (30, "30 days"), (365, "Year")]

    var body: some View {
        let rows = graph.facets.kinds.count + graph.linkKinds.count + graph.facets.tags.count + graph.facets.actors.count
        KScrollView {
            VStack(alignment: .leading, spacing: KSpace.x4) {
                section("Kinds") {
                    ForEach(graph.facets.kinds, id: \.self) { kind in
                        check(kind.capitalized, on: graph.isOn(kind, in: \.nodeKinds)) {
                            graph.toggle(kind, in: \.nodeKinds)
                        } leading: {
                            NodeGlyphView(kind: kind)
                        }
                    }
                }
                section("Links") {
                    ForEach(graph.linkKinds) { kind in
                        check(kind.label, on: graph.isOn(kind.kind, in: \.linkKinds)) {
                            graph.toggle(kind.kind, in: \.linkKinds)
                        } leading: {
                            Capsule()
                                .fill(kind.kind == LinkKindInfo.mentions ? theme.line : theme.indigo)
                                .frame(width: 10, height: 2)
                        }
                    }
                }
                if !graph.facets.tags.isEmpty {
                    section("Tags") {
                        ForEach(graph.facets.tags, id: \.self) { tag in
                            check(tag, on: graph.isOn(tag, in: \.tags)) { graph.toggle(tag, in: \.tags) } leading: { EmptyView() }
                        }
                    }
                }
                if !graph.facets.actors.isEmpty {
                    section("People and agents") {
                        ForEach(graph.facets.actors, id: \.self) { actor in
                            check(actor, on: graph.isOn(actor, in: \.actors)) { graph.toggle(actor, in: \.actors) } leading: { EmptyView() }
                        }
                    }
                }
                section("Changed") {
                    periods
                }
                Rectangle().fill(theme.line).frame(height: 1)
                KButton("Clear filters", icon: .close, kind: .ghost, size: .small) { graph.filter = GraphFilter() }
                    .disabled(graph.filter.isEmpty)
            }
            .padding(KSpace.x1)
        }
        .frame(width: 264, height: min(440, CGFloat(rows) * 26 + 280))
    }

    private var currentPeriod: Int? {
        guard let since = graph.filter.since else { return nil }
        let days = Int((Date().timeIntervalSince(since) / 86_400).rounded())
        return Self.periods.compactMap(\.days).min { abs($0 - days) < abs($1 - days) }
    }

    private var periods: some View {
        let current = currentPeriod
        return FlowRow(spacing: 6) {
            ForEach(Self.periods, id: \.title) { period in
                let on = period.days == current
                Button { graph.showChanged(inLast: period.days) } label: {
                    KChip(period.title, kind: on ? .solid : .neutral)
                }
                .buttonStyle(KPlainButtonStyle())
                .accessibilityAddTraits(on ? .isSelected : [])
            }
        }
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: KSpace.x1) {
            KSectionLabel(title).padding(.bottom, 2)
            content()
        }
    }

    private func check<Leading: View>(_ title: String, on: Bool, toggle: @escaping () -> Void, @ViewBuilder leading: () -> Leading) -> some View {
        let lead = leading()
        return Button(action: toggle) {
            HStack(spacing: KSpace.x2) {
                KCheckmark(isOn: on, side: 14)
                lead
                Text(title).font(KFont.ui(13)).foregroundStyle(theme.fg).lineLimit(1)
                Spacer(minLength: 0)
            }
            .frame(height: 24)
        }
        .buttonStyle(KPlainButtonStyle())
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// Lays children out left to right, wrapping onto new lines.
struct FlowRow: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0, y: CGFloat = 0, line: CGFloat = 0, widest: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                y += line + spacing
                x = 0
                line = 0
            }
            x += size.width + spacing
            line = max(line, size.height)
            widest = max(widest, x - spacing)
        }
        return CGSize(width: proposal.width ?? widest, height: y + line)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX, y = bounds.minY, line: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX {
                y += line + spacing
                x = bounds.minX
                line = 0
            }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            line = max(line, size.height)
        }
    }
}

/// Finding a note on the graph: a search field over the board, with the matches below it.
struct GraphFinder: View {
    @Bindable var graph: GraphModel
    @Environment(\.kTheme) private var theme
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x1) {
            KSearchField(placeholder: "Find a note", text: $graph.searchText, size: .small, focus: $focused) {
                graph.focusFirstMatch()
            }
            .frame(width: 240)
            .kTooltip(focused ? nil : "Type to find a note on the graph; Return centres it")
            if focused, !graph.matches.isEmpty {
                VStack(alignment: .leading, spacing: 1) {
                    ForEach(graph.matches) { node in
                        KRow(action: { graph.focus(node.id) }) {
                            NodeGlyphView(kind: node.kind)
                            Text(node.title).font(KFont.ui(13)).foregroundStyle(theme.fg).lineLimit(1)
                        }
                    }
                }
                .padding(KSpace.x1)
                .frame(width: 240)
                .mapChrome()
                .shadow(color: .black.opacity(theme.isDark ? 0.35 : 0.08), radius: 12, y: 4)
            }
        }
    }
}

/// Which glyph is which kind, and which line is which link.
struct GraphLegend: View {
    let graph: GraphModel
    @Environment(\.kTheme) private var theme

    var body: some View {
        let kinds = graph.facets.kinds.filter { kind in graph.nodes.contains { $0.kind == kind } }
        if !kinds.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                KSectionLabel("Legend").padding(.bottom, 2)
                ForEach(kinds, id: \.self) { kind in
                    HStack(spacing: KSpace.x2) {
                        NodeGlyphView(kind: kind)
                        Text(kind.capitalized)
                    }
                }
                HStack(spacing: KSpace.x2) {
                    Capsule().fill(theme.line).frame(width: 10, height: 1.5)
                    Text("Mentions")
                }
                HStack(spacing: KSpace.x2) {
                    Capsule().fill(theme.indigo).frame(width: 10, height: 1.5)
                    Text("Typed link")
                }
            }
            .font(KFont.ui(12))
            .foregroundStyle(theme.muted)
            .padding(KSpace.x3)
            .mapChrome()
        }
    }
}
