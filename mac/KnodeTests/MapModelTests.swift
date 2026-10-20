import Foundation
import Testing
@testable import Knode

/// The graph, the canvas and the links of a note, against the in-memory store.
@MainActor
@Suite struct MapModelTests {
    let url = URL(filePath: "/tmp/Map.knode")

    /// A space with three notes: A mentions B, B mentions C, and D stands alone.
    func makeSpace() async throws -> (PreviewSpaceStore, Space.ID, [String: Node.ID]) {
        let clock = TestClock()
        let store = PreviewSpaceStore(seedExamples: false, person: "Ada", now: { clock.now() })
        let space = try await store.openSpace(at: url)
        var ids: [String: Node.ID] = [:]
        ids["A"] = try await store.createNode(in: space.id, title: "A", body: "See [[B]].").id
        ids["B"] = try await store.createNode(in: space.id, title: "B", body: "See [[C]].").id
        ids["C"] = try await store.createNode(in: space.id, title: "C", body: "").id
        ids["D"] = try await store.createNode(in: space.id, title: "D", body: "").id
        return (store, space.id, ids)
    }

    // MARK: Graph

    @Test func loadsTheGraphAndWhatCanBeFiltered() async throws {
        let (store, space, _) = try await makeSpace()
        _ = try await store.createCanvas(title: "Board", in: space)
        let graph = GraphModel(spaceID: space, store: store)
        await graph.load()
        #expect(graph.nodes.count == 5)
        #expect(graph.edges.count == 2)
        #expect(graph.layout.count == 5)
        #expect(graph.layout.sources.count == 2)
        #expect(graph.facets.kinds == ["canvas", "note"])
        #expect(graph.facets.actors == ["Ada"])
        #expect(graph.isSimulating)
        await graph.settle(maxSteps: 1000)
        #expect(!graph.isSimulating)
    }

    @Test func filtersByKindAndLinkKind() async throws {
        let (store, space, ids) = try await makeSpace()
        _ = try await store.createCanvas(title: "Board", in: space)
        try await store.link(from: ids["A"]!, to: ids["D"]!, kind: "related", in: space)
        let graph = GraphModel(spaceID: space, store: store)
        await graph.load()
        graph.toggle("canvas", in: \.nodeKinds)
        await graph.reload()
        #expect(graph.nodes.map(\.title) == ["Board"])
        graph.filter = GraphFilter(linkKinds: ["related"])
        await graph.reload()
        #expect(graph.nodes.count == 5)
        #expect(graph.edges == [GraphEdgeInfo(from: ids["A"]!, to: ids["D"]!, kind: "related")])
        graph.showChanged(inLast: 1, now: Date(timeIntervalSince1970: 0))
        #expect(graph.filter.since == Date(timeIntervalSince1970: -86_400))
        #expect(graph.filter.count == 2)
    }

    @Test func searchFocusesANode() async throws {
        let (store, space, ids) = try await makeSpace()
        let graph = GraphModel(spaceID: space, store: store)
        await graph.load()
        await graph.settle(maxSteps: 1000)
        graph.searchText = "c"
        #expect(graph.matches.map(\.title) == ["C"])
        graph.focusFirstMatch()
        let c = graph.index[ids["C"]!]!
        #expect(graph.focused == c)
        #expect(graph.viewport.x == Double(graph.layout.position(c).x))
        #expect(graph.viewport.zoom >= 1.2)
        #expect(graph.labelled().contains(c))
    }

    @Test func clickingANodeOpensIt() async throws {
        let (store, space, ids) = try await makeSpace()
        let graph = GraphModel(spaceID: space, store: store)
        var opened: Node.ID?
        graph.onOpen = { opened = $0 }
        await graph.load()
        graph.open(graph.index[ids["B"]!]!)
        #expect(opened == ids["B"])
    }

    @Test func savesANamedViewWithItsFilterViewportAndPositions() async throws {
        let (store, space, _) = try await makeSpace()
        let graph = GraphModel(spaceID: space, store: store, saveDelay: .zero)
        await graph.load()
        await graph.settle(maxSteps: 1000)
        graph.filter = GraphFilter(nodeKinds: ["note"])
        await graph.reload()
        graph.viewport = Viewport(x: 12, y: 34, zoom: 1.5)
        await graph.saveAsNewView(named: "Notes")
        #expect(graph.views.map(\.name) == ["Notes"])
        let view = try #require(graph.currentView)
        #expect(view.filter == GraphFilter(nodeKinds: ["note"]))
        let saved = try await store.graph(in: space, view: view.id, filter: view.filter)
        #expect(saved.positions.count == 4)
        let stored = try await store.mapViews(in: space)
        #expect(stored.first?.viewport == Viewport(x: 12, y: 34, zoom: 1.5))

        // Opened again, the nodes are where they were and nothing moves.
        let again = GraphModel(spaceID: space, store: store)
        await again.load()
        await again.select(again.views.first)
        #expect(!again.isSimulating)
        #expect(again.viewport == Viewport(x: 12, y: 34, zoom: 1.5))
        for (id, i) in again.index {
            let before = graph.layout.position(graph.index[id]!)
            #expect(abs(again.layout.position(i).x - before.x) < 1e-6)
        }

        await again.rename(view, to: "Just notes")
        #expect(again.views.map(\.name) == ["Just notes"])
        await again.delete(again.views[0])
        #expect(again.views.isEmpty)
        #expect(again.currentView == nil)
    }

    @Test func draggingANodeMovesItAndKeepsItThere() async throws {
        let (store, space, ids) = try await makeSpace()
        let graph = GraphModel(spaceID: space, store: store)
        graph.size = CGSize(width: 400, height: 400)
        await graph.load()
        await graph.settle(maxSteps: 1000)
        let a = graph.index[ids["A"]!]!
        graph.beginDrag(a)
        #expect(graph.isSimulating)
        graph.drag(to: CGPoint(x: 10, y: 10))
        let target = graph.viewport.toPlane(CGPoint(x: 10, y: 10), in: graph.size)
        #expect(graph.layout.position(a) == target)
        graph.endDrag()
        #expect(graph.isDragging == nil)
    }

    // MARK: Canvas

    func makeCanvas() async throws -> (CanvasModel, PreviewSpaceStore, Space.ID, [NodeSummary]) {
        let (store, space, _) = try await makeSpace()
        let view = try await store.createCanvas(title: "Board", in: space)
        let canvas = CanvasModel(spaceID: space, nodeID: try #require(view.nodeID), store: store)
        await canvas.load()
        let notes = try await store.listNodes(in: space).filter { $0.kind == NodeKind.note }.sorted { $0.title < $1.title }
        return (canvas, store, space, notes)
    }

    @Test func dropsNotesOnTheBoardAndSavesThem() async throws {
        let (canvas, store, space, notes) = try await makeCanvas()
        #expect(canvas.view?.name == "Board")
        #expect(canvas.items.isEmpty)
        await canvas.add(Array(notes.prefix(2)), at: CGPoint(x: 100, y: 50))
        #expect(canvas.items.map(\.title) == ["A", "B"])
        #expect(canvas.items[0].position.center == CGPoint(x: 100, y: 50))
        #expect(canvas.items[1].position.z > canvas.items[0].position.z)
        #expect(canvas.edges.count == 1, "A mentions B")
        #expect(canvas.label(of: canvas.edges[0]) == "mentions")
        let board = try #require(try await store.canvasBoard(of: canvas.nodeID, in: space))
        #expect(board.items.count == 2)
        // Positions are not changes: the history of the space only has the notes and the canvas.
        #expect(try await store.changes(of: notes[0].id, in: space, limit: 10).count == 1)
    }

    @Test func movesUndoAndRedoWithinTheSession() async throws {
        let (canvas, store, space, notes) = try await makeCanvas()
        await canvas.add([notes[0]], at: .zero)
        let id = notes[0].id
        canvas.beginDrag()
        canvas.move([id], by: CGSize(width: 40, height: 10))
        canvas.move([id], by: CGSize(width: 80, height: 20))
        await canvas.endMove([id])
        #expect(canvas.items[0].position.center == CGPoint(x: 80, y: 20))
        #expect(canvas.undoName == "Move Note")
        #expect(try await saved(store, canvas, space)[id]?.center == CGPoint(x: 80, y: 20))

        await canvas.undo()
        #expect(canvas.items[0].position.center == .zero)
        #expect(try await saved(store, canvas, space)[id]?.center == .zero)
        #expect(canvas.canRedo)
        await canvas.redo()
        #expect(canvas.items[0].position.center == CGPoint(x: 80, y: 20))
        #expect(try await saved(store, canvas, space)[id]?.center == CGPoint(x: 80, y: 20))

        // Undo the move, then the add: the note leaves the board, and comes back with a redo.
        await canvas.undo()
        await canvas.undo()
        #expect(canvas.items.isEmpty)
        #expect(try await saved(store, canvas, space).isEmpty)
        await canvas.redo()
        #expect(canvas.items.map(\.title) == ["A"])
        #expect(canvas.canUndo)
    }

    @Test func resizesAndCollapsesANote() async throws {
        let (canvas, _, _, notes) = try await makeCanvas()
        await canvas.add([notes[0]], at: CGPoint(x: 0, y: 0))
        let id = notes[0].id
        canvas.beginDrag()
        canvas.resize(id, by: CGSize(width: 60, height: -1000))
        await canvas.endResize()
        let size = try #require(canvas.items[0].position.size)
        #expect(size.width == CanvasModel.defaultSize.width + 60)
        #expect(size.height == CanvasModel.minimumSize.height)
        // The top-left corner stays put.
        #expect(canvas.frame(of: canvas.items[0]).minX == -CanvasModel.defaultSize.width / 2)
        await canvas.toggleCollapsed(id)
        #expect(Double(canvas.frame(of: canvas.items[0]).height) == CanvasModel.collapsedHeight)
    }

    @Test func groupsHoldNotesThatMoveWithThem() async throws {
        let (canvas, store, space, notes) = try await makeCanvas()
        await canvas.add([notes[0]], at: CGPoint(x: 0, y: 0))
        await canvas.add([notes[1]], at: CGPoint(x: 600, y: 0))
        canvas.selection = [notes[0].id]
        await canvas.addGroup(label: "Now")
        let group = try #require(canvas.groups.first)
        #expect(canvas.items.first { $0.id == notes[0].id }?.position.groupID == group.id)
        #expect(group.frame.contains(CGPoint.zero))

        // Dropping B inside the group puts it in.
        canvas.beginDrag()
        canvas.move([notes[1].id], by: CGSize(width: -590, height: 10))
        await canvas.endMove([notes[1].id])
        #expect(canvas.items.first { $0.id == notes[1].id }?.position.groupID == group.id)

        // Moving the group moves both.
        canvas.beginDrag()
        canvas.moveGroup(group.id, by: CGSize(width: 100, height: 100))
        await canvas.endGroupDrag()
        #expect(canvas.items.first { $0.id == notes[0].id }?.position.center == CGPoint(x: 100, y: 100))
        #expect(canvas.groups[0].x == group.x + 100)
        let board = try #require(try await store.canvasBoard(of: canvas.nodeID, in: space))
        #expect(board.groups.first?.x == group.x + 100)

        // A collapsed group hides its notes.
        await canvas.toggleGroupCollapsed(group.id)
        #expect(canvas.items.allSatisfy(canvas.isHidden))

        // Removing the group keeps the notes; undoing it brings the group back.
        await canvas.removeGroup(group.id)
        #expect(canvas.groups.isEmpty)
        #expect(canvas.items.allSatisfy { $0.position.groupID == nil })
        await canvas.undo()
        #expect(canvas.groups.count == 1)
        let back = canvas.groups[0].id
        #expect(canvas.items.allSatisfy { $0.position.groupID == back })
        let restored = try #require(try await store.canvasBoard(of: canvas.nodeID, in: space))
        #expect(restored.groups.map(\.id) == [back])
        #expect(restored.items.allSatisfy { $0.position.groupID == back })
    }

    @Test func removesNotesAndLinksThemOnTheBoard() async throws {
        let (canvas, _, _, notes) = try await makeCanvas()
        await canvas.add([notes[0], notes[3]], at: .zero)
        #expect(canvas.edges.isEmpty)
        await canvas.link(from: notes[0].id, to: notes[3].id, kind: "part-of")
        #expect(canvas.edges.map(\.kind) == ["part-of"])
        #expect(canvas.label(of: canvas.edges[0]) == "part of")
        await canvas.remove([notes[3].id])
        #expect(canvas.items.map(\.title) == ["A"])
        #expect(canvas.edges.isEmpty)
        await canvas.undo()
        #expect(canvas.items.map(\.title).sorted() == ["A", "D"])
    }

    @Test func aNoteThatIsNotACanvasHasNoBoard() async throws {
        let (store, space, ids) = try await makeSpace()
        let canvas = CanvasModel(spaceID: space, nodeID: ids["A"]!, store: store)
        await canvas.load()
        #expect(canvas.isMissing)
    }

    private func saved(_ store: PreviewSpaceStore, _ canvas: CanvasModel, _ space: Space.ID) async throws -> [Node.ID: ViewPosition] {
        let board = try #require(try await store.canvasBoard(of: canvas.nodeID, in: space))
        return Dictionary(uniqueKeysWithValues: board.items.map { ($0.id, $0.position) })
    }

    // MARK: Links of a note

    @Test func showsLinksAndBacklinksWithLabels() async throws {
        let (store, space, ids) = try await makeSpace()
        try await store.link(from: ids["B"]!, to: ids["D"]!, kind: "part-of", in: space)
        let links = NoteLinksModel(spaceID: space, nodeID: ids["B"]!, store: store)
        await links.load()
        #expect(links.outgoing.map(\.label) == ["mentions", "part of"])
        #expect(links.outgoing.map(\.title) == ["C", "D"])
        #expect(links.backlinks.map(\.label) == ["mentioned in"])
        #expect(links.backlinks.map(\.title) == ["A"])
        #expect(links.backlinksByLabel.map(\.label) == ["mentioned in"])
        #expect(!links.choosableKinds.contains { $0.kind == "mentions" })
        #expect(!links.canRemove(links.outgoing[0]))

        let d = NoteLinksModel(spaceID: space, nodeID: ids["D"]!, store: store)
        await d.load()
        #expect(d.backlinks.map(\.label) == ["has part"])
        await d.remove(d.backlinks[0])
        #expect(d.backlinks.isEmpty)
    }

    @Test func addsATypedLinkWithANewKind() async throws {
        let (store, space, ids) = try await makeSpace()
        let links = NoteLinksModel(spaceID: space, nodeID: ids["A"]!, store: store)
        var changed = 0
        links.didChange = { changed += 1 }
        await links.load()
        #expect(await links.addLink(to: ids["C"]!, kind: "related"))
        #expect(links.outgoing.contains { $0.label == "related to" && $0.title == "C" })

        var draft = LinkKindDraft()
        draft.label = "Depends on"
        draft.inverseLabel = "needed by"
        #expect(draft.effectiveName == "depends-on")
        #expect(draft.problem(existing: links.kinds) == nil)
        #expect(await links.addLink(to: ids["D"]!, newKind: draft.kind))
        #expect(links.outgoing.contains { $0.label == "Depends on" && $0.title == "D" })
        #expect(links.kinds.contains { $0.kind == "depends-on" && !$0.builtIn })
        #expect(changed == 2)

        let d = NoteLinksModel(spaceID: space, nodeID: ids["D"]!, store: store)
        await d.load()
        #expect(d.backlinks.map(\.label) == ["needed by"])
        #expect(draft.problem(existing: links.kinds) == "A kind named depends-on already exists.")
    }

    @Test func checksANewKind() {
        var draft = LinkKindDraft()
        #expect(draft.problem(existing: []) != nil)
        draft.label = "Built on"
        #expect(draft.problem(existing: []) == nil)
        draft.name = "Built On"
        #expect(draft.problem(existing: []) != nil)
        draft.name = ""
        draft.direction = .symmetric
        draft.inverseLabel = "ignored"
        #expect(draft.kind.inverseLabel == "")
        #expect(LinkKindInfo.name(fromLabel: "  2 Depends -- on!") == "depends-on")
        #expect(LinkKindInfo.isValidName("depends-on"))
        #expect(!LinkKindInfo.isValidName("1st"))
        #expect(!LinkKindInfo.isValidName(String(repeating: "a", count: 33)))
        let partOf = LinkKindInfo.builtIns[2]
        #expect(partOf.label(outgoing: true) == "part of")
        #expect(partOf.label(outgoing: false) == "has part")
    }

    // MARK: App

    @Test func theAppShowsTheGraphAndOpensCanvases() async throws {
        let clock = TestClock()
        let model = AppModel(store: PreviewSpaceStore(seedExamples: true, now: { clock.now() }), searchDelay: .zero, saveDelay: .seconds(60))
        await model.openSpace(at: url)
        #expect(model.hasMap)
        model.toggleGraph()
        #expect(model.mode == .graph)
        let graph = try #require(model.graph)
        await graph.load()
        #expect(graph.nodes.count == 4)

        await model.createCanvas()
        #expect(model.mode == .notes)
        #expect(model.canvas != nil)
        #expect(model.editor?.title == "Untitled Canvas")
        #expect(model.nodes.first?.kind == NodeKind.canvas)

        await model.openNode(model.nodes.last?.id)
        #expect(model.canvas == nil)
        model.toggleLinks()
        #expect(model.isShowingLinks)
        model.toggleHistory()
        #expect(model.isShowingHistory)
        #expect(!model.isShowingLinks)
    }
}
