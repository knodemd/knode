import KnodeBrand
import SwiftUI

/// A canvas: notes arranged by hand on a board. Drag notes from the list onto it, move and resize
/// them, group them, link them, and zoom and pan around. Edit > Undo undoes moves.
struct CanvasScreen: View {
    @Bindable var canvas: CanvasModel
    @Bindable var editor: NoteEditorModel
    @Environment(AppModel.self) private var model
    @Environment(\.undoManager) private var undoManager
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            CanvasBar(canvas: canvas, editor: editor)
            Divider()
            if canvas.isMissing {
                ContentUnavailableView("No Board", systemImage: "square.dashed", description: Text("This node is a canvas without a board."))
            } else {
                CanvasBoardView(canvas: canvas)
            }
        }
        .background(MapInks(scheme).background)
        .onAppear { canvas.undoManager = undoManager }
        .onChange(of: undoManager) { _, manager in canvas.undoManager = manager }
        .onDisappear {
            canvas.forgetUndo()
            Task {
                await canvas.flush()
                await editor.save()
            }
        }
        .alert(
            "Something went wrong",
            isPresented: Binding(get: { canvas.errorMessage != nil }, set: { if !$0 { canvas.errorMessage = nil } }),
            actions: { Button("OK") { canvas.errorMessage = nil } },
            message: { Text(canvas.errorMessage ?? "") }
        )
    }
}

/// The bar above a canvas: its title, adding notes and groups, undo, zoom.
struct CanvasBar: View {
    @Bindable var canvas: CanvasModel
    @Bindable var editor: NoteEditorModel
    @Environment(AppModel.self) private var model
    @Environment(\.undoManager) private var undoManager
    @State private var isAdding = false

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "rectangle.on.rectangle.angled")
                .foregroundStyle(KnodeColor.indigo)
                .accessibilityHidden(true)
            TextField("Canvas name", text: $editor.title)
                .textFieldStyle(.plain)
                .font(.title3.weight(.semibold))
                .frame(minWidth: 120)
            Spacer(minLength: 8)
            Button("Add Notes", systemImage: "plus.rectangle.on.rectangle") { isAdding = true }
                .popover(isPresented: $isAdding, arrowEdge: .bottom) {
                    AddToCanvasList(canvas: canvas, notes: model.nodes.filter { $0.id != canvas.nodeID })
                }
                .help("Put notes on the canvas; you can also drag them from the list")
            Button("New Group", systemImage: "square.dashed") { Task { await canvas.addGroup() } }
                .help(canvas.selection.isEmpty ? "Add a group" : "Group the selected notes")
            ControlGroup {
                Button("Undo", systemImage: "arrow.uturn.backward") { undoManager?.undo() }
                    .disabled(!canvas.canUndo)
                Button("Redo", systemImage: "arrow.uturn.forward") { undoManager?.redo() }
                    .disabled(!canvas.canRedo)
            }
            .labelStyle(.iconOnly)
            .controlSize(.small)
            .fixedSize()
            .help("Undo and redo moves (Command-Z, Shift-Command-Z)")
            ControlGroup {
                Button("Zoom Out", systemImage: "minus.magnifyingglass") { canvas.zoom(by: 1 / 1.25) }
                Button("Zoom In", systemImage: "plus.magnifyingglass") { canvas.zoom(by: 1.25) }
                Button("Fit", systemImage: "arrow.up.left.and.down.right.magnifyingglass") { canvas.fit() }
            }
            .labelStyle(.iconOnly)
            .controlSize(.small)
            .fixedSize()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }
}

/// Notes to put on the canvas, with a search.
struct AddToCanvasList: View {
    let canvas: CanvasModel
    let notes: [NodeSummary]
    @State private var query = ""
    @State private var chosen: Set<Node.ID> = []

    private var shown: [NodeSummary] {
        let query = query.trimmingCharacters(in: .whitespaces)
        let onBoard = Set(canvas.items.map(\.id))
        let list = notes.filter { !onBoard.contains($0.id) }
        return query.isEmpty ? list : list.filter { $0.title.localizedCaseInsensitiveContains(query) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Search notes", text: $query)
                .textFieldStyle(.roundedBorder)
            List(shown, selection: $chosen) { note in
                Label {
                    Text(note.title).lineLimit(1)
                } icon: {
                    NodeGlyphView(kind: note.kind)
                }
                .tag(note.id)
            }
            .frame(height: 260)
            HStack {
                Spacer()
                Button("Add to Canvas", systemImage: "plus") {
                    let picked = notes.filter { chosen.contains($0.id) }
                    let centre = canvas.viewport.toPlane(CGPoint(x: canvas.size.width / 2, y: canvas.size.height / 2), in: canvas.size)
                    Task { await canvas.add(picked, at: centre) }
                    chosen = []
                }
                .disabled(chosen.isEmpty)
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(12)
        .frame(width: 320)
    }
}

/// The board: groups, links and note cards on a plane that zooms and pans.
struct CanvasBoardView: View {
    @Bindable var canvas: CanvasModel
    @Environment(AppModel.self) private var model
    @Environment(\.colorScheme) private var scheme
    @State private var panStart: Viewport?
    @State private var magnifyStart: Viewport?
    @State private var lastHover: CGPoint?

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .topLeading) {
                background
                ForEach(canvas.groups) { group in
                    GroupBox(canvas: canvas, group: group)
                }
                links
                ForEach(canvas.items.filter { !canvas.isHidden($0) }) { item in
                    CanvasCard(canvas: canvas, item: item)
                }
            }
            .frame(width: proxy.size.width, height: proxy.size.height)
            .clipped()
            .onAppear {
                canvas.size = proxy.size
                if canvas.view?.viewport == nil, !canvas.items.isEmpty { canvas.fit() }
            }
            .onChange(of: proxy.size) { _, size in canvas.size = size }
        }
        .onContinuousHover { phase in
            if case .active(let point) = phase { lastHover = point } else { lastHover = nil }
        }
        .onScrollWheel { event in
            if event.modifierFlags.contains(.command) || !event.hasPreciseScrollingDeltas {
                let factor = pow(1.0025, event.scrollingDeltaY * (event.hasPreciseScrollingDeltas ? 1 : 8))
                canvas.zoom(by: factor, at: lastHover)
            } else {
                canvas.pan(by: CGSize(width: -event.scrollingDeltaX, height: -event.scrollingDeltaY))
            }
            return true
        }
        .simultaneousGesture(
            MagnifyGesture()
                .onChanged { value in
                    if magnifyStart == nil { magnifyStart = canvas.viewport }
                    if let start = magnifyStart { canvas.viewport = start.zoomed(by: value.magnification, at: value.startLocation, in: canvas.size) }
                }
                .onEnded { _ in magnifyStart = nil }
        )
        .dropDestination(for: String.self) { ids, location in
            let notes = ids.compactMap { id in model.nodes.first { $0.id == id } }.filter { $0.id != canvas.nodeID }
            guard !notes.isEmpty else { return false }
            Task { await canvas.add(notes, at: canvas.viewport.toPlane(location, in: canvas.size)) }
            return true
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Canvas with \(canvas.items.count) notes")
    }

    /// A dot grid that moves with the board; dragging it pans, clicking it clears the selection.
    private var background: some View {
        Canvas { context, size in
            let viewport = canvas.viewport
            let step = 24 * viewport.zoom
            guard step >= 6 else { return }
            let origin = viewport.toScreen(.zero, in: size)
            let startX = origin.x.truncatingRemainder(dividingBy: step) - step
            let startY = origin.y.truncatingRemainder(dividingBy: step) - step
            var dots = Path()
            var x = startX
            while x < size.width + step {
                var y = startY
                while y < size.height + step {
                    dots.addEllipse(in: CGRect(x: x - 0.75, y: y - 0.75, width: 1.5, height: 1.5))
                    y += step
                }
                x += step
            }
            context.fill(dots, with: .color(MapInks(scheme).foreground.opacity(0.12)))
        }
        .contentShape(Rectangle())
        .gesture(
            DragGesture(minimumDistance: 2)
                .onChanged { value in
                    if panStart == nil { panStart = canvas.viewport }
                    if let panStart { canvas.viewport = panStart.panned(by: value.translation) }
                }
                .onEnded { _ in panStart = nil }
        )
        .onTapGesture { canvas.selection = [] }
    }

    /// Links between notes of the board, with their labels.
    private var links: some View {
        Canvas { context, size in
            let inks = MapInks(scheme)
            let centres = Dictionary(canvas.items.filter { !canvas.isHidden($0) }.map { ($0.id, canvas.viewport.toScreen($0.position.center, in: size)) }, uniquingKeysWith: { first, _ in first })
            for edge in canvas.edges {
                guard let a = centres[edge.from], let b = centres[edge.to], edge.from != edge.to else { continue }
                var path = Path()
                path.move(to: a)
                path.addLine(to: b)
                let typed = edge.kind != LinkKindInfo.mentions
                context.stroke(path, with: .color(typed ? inks.indigo : inks.foreground.opacity(0.35)), style: StrokeStyle(lineWidth: 1.5, dash: typed ? [] : [4, 3]))
                guard canvas.viewport.zoom > 0.45 else { continue }
                let mid = CGPoint(x: (a.x + b.x) / 2, y: (a.y + b.y) / 2)
                let text = context.resolve(Text(canvas.label(of: edge)).font(.knodeLine(10)).foregroundStyle(typed ? KnodeColor.ink : inks.foreground))
                let measured = text.measure(in: CGSize(width: 240, height: 40))
                let box = CGRect(x: mid.x - measured.width / 2 - 5, y: mid.y - measured.height / 2 - 2, width: measured.width + 10, height: measured.height + 4)
                context.fill(Path(roundedRect: box, cornerRadius: box.height / 2), with: .color(typed ? inks.indigo : inks.background))
                context.draw(text, at: mid)
            }
        }
        .allowsHitTesting(false)
    }
}

/// One note on the board.
struct CanvasCard: View {
    let canvas: CanvasModel
    let item: CanvasItem
    @Environment(\.colorScheme) private var scheme
    @State private var isDragging = false

    var body: some View {
        let inks = MapInks(scheme)
        let zoom = canvas.viewport.zoom
        let size = CanvasModel.boxSize(of: item.position)
        let centre = canvas.viewport.toScreen(item.position.center, in: canvas.size)
        let selected = canvas.selection.contains(item.id)
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                NodeGlyphView(kind: item.kind, size: 9)
                Text(item.title.isEmpty ? "Untitled" : item.title)
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(item.position.collapsed ? 1 : 2)
                Spacer(minLength: 0)
            }
            if !item.position.collapsed, !item.excerpt.isEmpty {
                Text(item.excerpt)
                    .font(.system(size: 12))
                    .foregroundStyle(inks.foreground.opacity(0.7))
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .frame(width: size.width, height: size.height, alignment: .topLeading)
        .background(inks.background, in: RoundedRectangle(cornerRadius: 8))
        .overlay {
            RoundedRectangle(cornerRadius: 8)
                .strokeBorder(selected ? inks.indigo : inks.foreground.opacity(0.25), lineWidth: selected ? 2 : 1)
        }
        .overlay(alignment: .bottomTrailing) {
            ResizeHandle()
                .gesture(
                    DragGesture(minimumDistance: 1, coordinateSpace: .global)
                        .onChanged { value in
                            canvas.beginDrag()
                            canvas.resize(item.id, by: CGSize(width: value.translation.width / zoom, height: value.translation.height / zoom))
                        }
                        .onEnded { _ in Task { await canvas.endResize() } }
                )
        }
        .contentShape(RoundedRectangle(cornerRadius: 8))
        .scaleEffect(zoom)
        .frame(width: size.width * zoom, height: size.height * zoom)
        .position(centre)
        .gesture(
            DragGesture(minimumDistance: 2, coordinateSpace: .global)
                .onChanged { value in
                    if !isDragging {
                        isDragging = true
                        if !canvas.selection.contains(item.id) { canvas.selection = [item.id] }
                        canvas.beginDrag()
                    }
                    canvas.move(canvas.selection, by: CGSize(width: value.translation.width / zoom, height: value.translation.height / zoom))
                }
                .onEnded { _ in
                    isDragging = false
                    let moved = canvas.selection
                    Task { await canvas.endMove(moved) }
                }
        )
        .onTapGesture(count: 2) { canvas.onOpen?(item.id) }
        .onTapGesture {
            if NSEvent.modifierFlags.contains(.shift) || NSEvent.modifierFlags.contains(.command) {
                if canvas.selection.contains(item.id) { canvas.selection.remove(item.id) } else { canvas.selection.insert(item.id) }
            } else {
                canvas.selection = [item.id]
            }
        }
        .contextMenu { menu }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityAction(named: "Open") { canvas.onOpen?(item.id) }
    }

    @ViewBuilder private var menu: some View {
        Button("Open", systemImage: "doc.text") { canvas.onOpen?(item.id) }
        Button(item.position.collapsed ? "Show Content" : "Show Title Only", systemImage: item.position.collapsed ? "rectangle.expand.vertical" : "rectangle.compress.vertical") {
            Task { await canvas.toggleCollapsed(item.id) }
        }
        let others = canvas.items.filter { $0.id != item.id }
        if !others.isEmpty {
            Menu("Link To", systemImage: "link") {
                ForEach(others) { other in
                    Menu(other.title.isEmpty ? "Untitled" : other.title) {
                        ForEach(canvas.linkKinds.filter { $0.kind != LinkKindInfo.mentions }) { kind in
                            Button(kind.label) { Task { await canvas.link(from: item.id, to: other.id, kind: kind.kind) } }
                        }
                    }
                }
            }
        }
        Divider()
        Button("Remove from Canvas", systemImage: "minus.circle") {
            let ids = canvas.selection.contains(item.id) ? canvas.selection : [item.id]
            Task { await canvas.remove(ids) }
        }
    }
}

/// A group on the board: a labelled box; its notes move with it.
struct GroupBox: View {
    let canvas: CanvasModel
    let group: ViewGroupInfo
    @Environment(\.colorScheme) private var scheme
    @State private var isRenaming = false
    @State private var label = ""

    var body: some View {
        let inks = MapInks(scheme)
        let zoom = canvas.viewport.zoom
        let size = group.collapsed ? CGSize(width: group.width, height: 36) : CGSize(width: group.width, height: group.height)
        let topLeft = canvas.viewport.toScreen(CGPoint(x: group.x - group.width / 2, y: group.y - group.height / 2), in: canvas.size)
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                Button(group.collapsed ? "Expand" : "Collapse", systemImage: group.collapsed ? "chevron.right" : "chevron.down") {
                    Task { await canvas.toggleGroupCollapsed(group.id) }
                }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                Text(group.label.isEmpty ? "Group" : group.label)
                    .font(.knodeLine(12))
                    .foregroundStyle(inks.indigo)
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10)
            .frame(height: 32)
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 2, coordinateSpace: .global)
                    .onChanged { value in
                        canvas.beginDrag()
                        canvas.moveGroup(group.id, by: CGSize(width: value.translation.width / zoom, height: value.translation.height / zoom))
                    }
                    .onEnded { _ in Task { await canvas.endGroupDrag() } }
            )
            .onTapGesture(count: 2) {
                label = group.label
                isRenaming = true
            }
            Spacer(minLength: 0)
        }
        .frame(width: size.width, height: size.height)
        .background(inks.indigo.opacity(0.06), in: RoundedRectangle(cornerRadius: 12))
        .overlay {
            RoundedRectangle(cornerRadius: 12)
                .strokeBorder(inks.indigo, style: StrokeStyle(lineWidth: 1.5, dash: [6, 4]))
        }
        .overlay(alignment: .bottomTrailing) {
            if !group.collapsed {
                ResizeHandle()
                    .gesture(
                        DragGesture(minimumDistance: 1, coordinateSpace: .global)
                            .onChanged { value in
                                canvas.beginDrag()
                                canvas.resizeGroup(group.id, by: CGSize(width: value.translation.width / zoom, height: value.translation.height / zoom))
                            }
                            .onEnded { _ in Task { await canvas.endGroupDrag("Resize Group") } }
                    )
            }
        }
        .scaleEffect(zoom, anchor: .topLeading)
        .frame(width: size.width * zoom, height: size.height * zoom, alignment: .topLeading)
        .offset(x: topLeft.x, y: topLeft.y)
        .contextMenu {
            Button("Rename…", systemImage: "pencil") {
                label = group.label
                isRenaming = true
            }
            Button("Remove Group", systemImage: "minus.circle") { Task { await canvas.removeGroup(group.id) } }
        }
        .alert("Rename Group", isPresented: $isRenaming) {
            TextField("Label", text: $label)
            Button("Rename") { Task { await canvas.renameGroup(group.id, to: label) } }
            Button("Cancel", role: .cancel) {}
        }
    }
}

/// The corner a box is resized by.
struct ResizeHandle: View {
    var body: some View {
        Image(systemName: "arrow.down.right")
            .font(.system(size: 9, weight: .bold))
            .foregroundStyle(.secondary)
            .frame(width: 16, height: 16)
            .contentShape(Rectangle())
            .onHover { inside in
                if inside { NSCursor.crosshair.set() } else { NSCursor.arrow.set() }
            }
            .accessibilityLabel("Resize")
    }
}
