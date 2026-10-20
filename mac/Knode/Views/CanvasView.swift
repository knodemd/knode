import SwiftUI

/// A canvas: notes arranged by hand on a board. Drag notes from the list onto it, move and resize
/// them, group them, link them, and zoom and pan around. Edit > Undo undoes moves.
struct CanvasScreen: View {
    @Bindable var canvas: CanvasModel
    @Bindable var editor: NoteEditorModel
    @Environment(AppModel.self) private var model
    @Environment(\.undoManager) private var undoManager
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(spacing: 0) {
            CanvasBar(canvas: canvas, editor: editor)
            Rectangle().fill(theme.line).frame(height: 1)
            if canvas.isMissing {
                KEmptyState(icon: .canvas, title: "No board", message: "This node is a canvas without a board.")
            } else {
                CanvasBoardView(canvas: canvas)
                    .overlay(alignment: .bottomTrailing) {
                        MapZoomControls(
                            zoomOut: { canvas.zoom(by: 1 / 1.25) },
                            zoomIn: { canvas.zoom(by: 1.25) },
                            fit: { canvas.fit() }
                        )
                        .padding(KSpace.x3)
                    }
            }
        }
        .overlay(alignment: .top) {
            if let error = canvas.errorMessage {
                KBanner(.error, title: "Something went wrong", message: error, onClose: { canvas.errorMessage = nil })
                    .frame(maxWidth: 520)
                    .padding(.top, 56)
                    .padding(.horizontal, KSpace.x3)
            }
        }
        .background(theme.bg)
        .onAppear { canvas.undoManager = undoManager }
        .onChange(of: undoManager) { _, manager in canvas.undoManager = manager }
        .onDisappear {
            canvas.forgetUndo()
            Task {
                await canvas.flush()
                await editor.save()
            }
        }
        .knodeTheme()
    }
}

/// The bar above a canvas: its title, adding notes and groups, undo.
struct CanvasBar: View {
    @Bindable var canvas: CanvasModel
    @Bindable var editor: NoteEditorModel
    @Environment(AppModel.self) private var model
    @Environment(\.undoManager) private var undoManager
    @Environment(\.kTheme) private var theme
    @State private var isAdding = false

    var body: some View {
        HStack(spacing: KSpace.x2) {
            KIcon(.canvas, size: .sm, color: theme.indigo)
                .accessibilityHidden(true)
            TextField("", text: $editor.title, prompt: Text("Canvas name").foregroundStyle(theme.muted))
                .textFieldStyle(.plain)
                .font(KFont.ui(15, .semibold))
                .foregroundStyle(theme.fg)
                .frame(minWidth: 120)
                .accessibilityLabel("Canvas name")
            Spacer(minLength: KSpace.x2)
            KButton("Add notes", icon: .add, size: .small) { isAdding = true }
                .kTooltip("Put notes on the canvas; you can also drag them from the list")
                .kPopover(isPresented: $isAdding) {
                    AddToCanvasList(canvas: canvas, notes: model.nodes.filter { $0.id != canvas.nodeID }) { isAdding = false }
                }
            KButton("New group", icon: .group, size: .small) { Task { await canvas.addGroup() } }
                .kTooltip(canvas.selection.isEmpty ? "Add a group" : "Group the selected notes")
            Rectangle().fill(theme.line).frame(width: 1, height: 18).padding(.horizontal, 2)
            KIconButton(.undo, title: "Undo (Command-Z)", size: .small) { undoManager?.undo() }
                .disabled(!canvas.canUndo)
            KIconButton(.redo, title: "Redo (Shift-Command-Z)", size: .small) { undoManager?.redo() }
                .disabled(!canvas.canRedo)
        }
        .padding(.horizontal, KSpace.x3)
        .frame(height: 44)
        .background(theme.bg)
    }
}

/// Notes to put on the canvas, with a search.
struct AddToCanvasList: View {
    let canvas: CanvasModel
    let notes: [NodeSummary]
    var close: () -> Void = {}
    @Environment(\.kTheme) private var theme
    @State private var query = ""
    @State private var chosen: Set<Node.ID> = []

    private var shown: [NodeSummary] {
        let query = query.trimmingCharacters(in: .whitespaces)
        let onBoard = Set(canvas.items.map(\.id))
        let list = notes.filter { !onBoard.contains($0.id) }
        return query.isEmpty ? list : list.filter { $0.title.localizedCaseInsensitiveContains(query) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSectionLabel("Add notes")
            KSearchField(placeholder: "Search notes", text: $query, size: .small)
            KScrollView {
                LazyVStack(alignment: .leading, spacing: 1) {
                    ForEach(shown) { note in
                        let on = chosen.contains(note.id)
                        KRow(isSelected: on, action: {
                            if on { chosen.remove(note.id) } else { chosen.insert(note.id) }
                        }) {
                            KCheckmark(isOn: on, side: 14)
                            NodeGlyphView(kind: note.kind)
                            Text(note.title.isEmpty ? "Untitled" : note.title).font(KFont.ui(13)).foregroundStyle(theme.fg).lineLimit(1)
                        }
                    }
                    if shown.isEmpty {
                        Text(query.isEmpty ? "Every note is on the canvas." : "No note matches.")
                            .font(KFont.ui(12)).foregroundStyle(theme.muted).padding(KSpace.x3)
                    }
                }
            }
            .frame(height: 260)
            HStack {
                Text(chosen.isEmpty ? "" : "\(chosen.count) chosen").kLineStyle(10).foregroundStyle(theme.muted)
                Spacer()
                Button {
                    let picked = notes.filter { chosen.contains($0.id) }
                    let centre = canvas.viewport.toPlane(CGPoint(x: canvas.size.width / 2, y: canvas.size.height / 2), in: canvas.size)
                    Task { await canvas.add(picked, at: centre) }
                    chosen = []
                    close()
                } label: {
                    HStack(spacing: 6) {
                        KIcon(.add, size: .sm)
                        Text("Add to canvas")
                    }
                }
                .buttonStyle(KButtonStyle(kind: .primary, size: .small))
                .disabled(chosen.isEmpty)
                .keyboardShortcut(.defaultAction)
            }
        }
        .frame(width: 300)
    }
}

/// The board: groups, links and note cards on a plane that zooms and pans.
struct CanvasBoardView: View {
    @Bindable var canvas: CanvasModel
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    @State private var panStart: Viewport?
    @State private var magnifyStart: Viewport?
    @State private var lastHover: CGPoint?
    @State private var isDropTargeted = false

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
        // Notes dragged from the list go on the board; files from the Finder become notes with
        // the file attached.
        .onDrop(of: [.fileURL, .utf8PlainText, .plainText], isTargeted: $isDropTargeted) { providers, location in
            let point = canvas.viewport.toPlane(location, in: canvas.size)
            if DroppedItems.hasFiles(providers) {
                guard model.canAttachFiles(to: canvas) else { return false }
                Task { await model.addFiles(await DroppedItems.fileURLs(providers), to: canvas, at: point) }
                return true
            }
            Task {
                let ids = await DroppedItems.strings(providers)
                let notes = ids.compactMap { id in model.nodes.first { $0.id == id } }.filter { $0.id != canvas.nodeID }
                await canvas.add(notes, at: point)
            }
            return true
        }
        .overlay {
            if isDropTargeted { DropHighlight() }
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
            context.fill(dots, with: .color(theme.line))
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
            let inks = MapInks(theme)
            let centres = Dictionary(canvas.items.filter { !canvas.isHidden($0) }.map { ($0.id, canvas.viewport.toScreen($0.position.center, in: size)) }, uniquingKeysWith: { first, _ in first })
            for edge in canvas.edges {
                guard let a = centres[edge.from], let b = centres[edge.to], edge.from != edge.to else { continue }
                var path = Path()
                path.move(to: a)
                path.addLine(to: b)
                let typed = edge.kind != LinkKindInfo.mentions
                context.stroke(path, with: .color(typed ? inks.indigo : inks.muted.opacity(0.6)), style: StrokeStyle(lineWidth: 1.5, dash: typed ? [] : [4, 3]))
                guard canvas.viewport.zoom > 0.45 else { continue }
                let mid = CGPoint(x: (a.x + b.x) / 2, y: (a.y + b.y) / 2)
                MapLinkLabel.draw(canvas.label(of: edge), at: mid, typed: typed, inks: inks, in: &context)
            }
        }
        .allowsHitTesting(false)
    }
}

/// One note on the board: a card like the cards of knode.md.
struct CanvasCard: View {
    let canvas: CanvasModel
    let item: CanvasItem
    @Environment(\.kTheme) private var theme
    @State private var isDragging = false
    @State private var isLinking = false
    @State private var hovering = false

    var body: some View {
        let zoom = canvas.viewport.zoom
        let size = CanvasModel.boxSize(of: item.position)
        let centre = canvas.viewport.toScreen(item.position.center, in: canvas.size)
        face(size: size, zoom: zoom)
            .scaleEffect(zoom)
            .frame(width: size.width * zoom, height: size.height * zoom)
        .kContextMenu(menu)
        .kPopover(isPresented: $isLinking, edge: .trailing) {
            CanvasLinkPicker(canvas: canvas, item: item) { isLinking = false }
        }
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
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityAddTraits(canvas.selection.contains(item.id) ? .isSelected : [])
        .accessibilityAction(named: "Open") { canvas.onOpen?(item.id) }
    }

    /// The card at its size on the plane, before the zoom.
    private func face(size: CGSize, zoom: CGFloat) -> some View {
        let selected = canvas.selection.contains(item.id)
        let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        let border: Color = selected ? theme.indigo : hovering ? theme.muted.opacity(0.6) : theme.line
        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                NodeGlyphView(kind: item.kind, size: 9)
                Text(item.title.isEmpty ? "Untitled" : item.title)
                    .font(KFont.ui(13, .semibold))
                    .foregroundStyle(theme.fg)
                    .lineLimit(item.position.collapsed ? 1 : 2)
                Spacer(minLength: 0)
            }
            if !item.position.collapsed, !item.excerpt.isEmpty {
                Text(item.excerpt)
                    .font(KFont.ui(12))
                    .lineSpacing(2)
                    .foregroundStyle(theme.muted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(width: size.width, height: size.height, alignment: .topLeading)
        .background(shape.fill(theme.panel))
        .background(shape.fill(theme.indigo.opacity(selected ? 0.10 : 0)))
        .overlay(shape.strokeBorder(border, lineWidth: selected ? 1.5 : 1))
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
        .contentShape(shape)
        .onHover { hovering = $0 }
    }

    private func menu() -> [KMenuEntry] {
        var entries: [KMenuEntry] = [
            .action("Open", icon: .note) { canvas.onOpen?(item.id) },
            .action(item.position.collapsed ? "Show content" : "Show title only", icon: item.position.collapsed ? .expand : .text) {
                Task { await canvas.toggleCollapsed(item.id) }
            },
        ]
        if canvas.items.contains(where: { $0.id != item.id }) {
            entries.append(.action("Link to", icon: .link) { isLinking = true })
        }
        entries.append(.divider)
        entries.append(.action("Remove from canvas", icon: .delete, isDestructive: true) {
            let ids = canvas.selection.contains(item.id) ? canvas.selection : [item.id]
            Task { await canvas.remove(ids) }
        })
        return entries
    }
}

/// Links a card to another card of the board: choose the note, then how they relate.
struct CanvasLinkPicker: View {
    let canvas: CanvasModel
    let item: CanvasItem
    var close: () -> Void
    @Environment(\.kTheme) private var theme
    @State private var target: Node.ID?

    var body: some View {
        let others = canvas.items.filter { $0.id != item.id }
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSectionLabel("Link to")
            KScrollView {
                VStack(alignment: .leading, spacing: 1) {
                    ForEach(others) { other in
                        KRow(isSelected: target == other.id, action: { target = other.id }) {
                            NodeGlyphView(kind: other.kind)
                            Text(other.title.isEmpty ? "Untitled" : other.title).font(KFont.ui(13)).foregroundStyle(theme.fg).lineLimit(1)
                        }
                    }
                }
            }
            .frame(height: min(220, CGFloat(others.count) * 34 + 4))
            KSectionLabel("How they relate")
            FlowRow(spacing: 6) {
                ForEach(canvas.linkKinds.filter { $0.kind != LinkKindInfo.mentions }) { kind in
                    Button {
                        guard let target else { return }
                        close()
                        Task { await canvas.link(from: item.id, to: target, kind: kind.kind) }
                    } label: {
                        KChip(kind.label, kind: target == nil ? .neutral : .accent)
                    }
                    .buttonStyle(KPlainButtonStyle())
                    .disabled(target == nil)
                }
            }
            .frame(width: 260)
            if target == nil {
                Text("Choose a note first.").font(KFont.ui(12)).foregroundStyle(theme.muted)
            }
        }
        .frame(width: 260)
    }
}

/// A group on the board: a labelled box; its notes move with it.
struct GroupBox: View {
    let canvas: CanvasModel
    let group: ViewGroupInfo
    @Environment(\.kTheme) private var theme
    @State private var isRenaming = false
    @State private var label = ""

    var body: some View {
        let zoom = canvas.viewport.zoom
        let size = group.collapsed ? CGSize(width: group.width, height: 36) : CGSize(width: group.width, height: group.height)
        let topLeft = canvas.viewport.toScreen(CGPoint(x: group.x - group.width / 2, y: group.y - group.height / 2), in: canvas.size)
        let shape = RoundedRectangle(cornerRadius: 18, style: .continuous)
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                Button {
                    Task { await canvas.toggleGroupCollapsed(group.id) }
                } label: {
                    KIcon(group.collapsed ? .forward : .down, size: .xs)
                }
                .buttonStyle(KIconButtonStyle(size: .small))
                .frame(width: 20, height: 20)
                .accessibilityLabel(group.collapsed ? "Expand" : "Collapse")
                Text(group.label.isEmpty ? "Group" : group.label)
                    .kLineStyle(12)
                    .foregroundStyle(theme.isDark ? theme.indigo : theme.fg)
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 8)
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
        .background(shape.fill(theme.indigo.opacity(theme.isDark ? 0.05 : 0.06)))
        .overlay {
            shape.strokeBorder(theme.indigo.opacity(0.7), style: StrokeStyle(lineWidth: 1, dash: [5, 4]))
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
        .overlay(alignment: .top) {
            // The header, outside the zoom, so its menu and rename card find it on screen.
            Color.clear
                .frame(height: 32 * zoom)
                .allowsHitTesting(false)
                .kContextMenu {
                    [
                        .action("Rename", icon: .edit) {
                            label = group.label
                            isRenaming = true
                        },
                        .action("Remove group", icon: .delete, isDestructive: true) { Task { await canvas.removeGroup(group.id) } },
                    ]
                }
                .kPopover(isPresented: $isRenaming) {
                    GroupRenameCard(label: $label) {
                        isRenaming = false
                    } rename: {
                        isRenaming = false
                        Task { await canvas.renameGroup(group.id, to: label) }
                    }
                }
        }
        .position(x: topLeft.x + size.width * zoom / 2, y: topLeft.y + size.height * zoom / 2)
    }
}

/// Renames a group of the board.
private struct GroupRenameCard: View {
    @Binding var label: String
    var cancel: () -> Void
    var rename: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x3) {
            KSectionLabel("Rename group")
            KTextField("Label", text: $label, size: .small, onSubmit: rename)
            HStack(spacing: KSpace.x2) {
                Spacer()
                KButton("Cancel", kind: .ghost, size: .small, action: cancel)
                Button(action: rename) { Text("Rename") }
                    .buttonStyle(KButtonStyle(kind: .primary, size: .small))
                    .keyboardShortcut(.defaultAction)
            }
        }
        .frame(width: 240)
    }
}

/// The corner a box is resized by: a grip drawn on the Knode grid.
struct ResizeHandle: View {
    @Environment(\.kTheme) private var theme
    private static let grip = KGlyph(rows: ["......#", ".......", "....#.#", ".......", "..#.#.#", ".......", "#.#.#.#"], title: "Resize")

    var body: some View {
        KIcon(Self.grip, points: 8.75, color: theme.muted.opacity(0.8))
            .frame(width: 18, height: 18)
            .contentShape(Rectangle())
            .onHover { inside in
                if inside { NSCursor.crosshair.set() } else { NSCursor.arrow.set() }
            }
            .accessibilityLabel("Resize")
    }
}
