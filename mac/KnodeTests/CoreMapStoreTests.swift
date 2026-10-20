import Foundation
import Testing
@testable import Knode

/// The map side of `CoreSpaceStore` against real `.knode` packages.
@Suite final class CoreMapStoreTests {
    let folder: URL

    init() throws {
        folder = FileManager.default.temporaryDirectory.appending(path: "KnodeMapTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    }

    deinit {
        try? FileManager.default.removeItem(at: folder)
    }

    var spaceURL: URL { folder.appending(path: "Map.knode", directoryHint: .isDirectory) }

    @Test func typedLinksReadWithTheirLabels() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: spaceURL, name: "Map").id
        let plan = try await store.createNode(in: space, title: "Plan", body: "")
        let step = try await store.createNode(in: space, title: "Step", body: "Part of [[Plan]].")
        #expect(step.kind == "note")
        try await store.link(from: step.id, to: plan.id, kind: "part-of", in: space)
        _ = try await store.setLinkKind(LinkKindInfo(kind: "blocks", label: "blocks", inverseLabel: "blocked by"), in: space)
        try await store.link(from: plan.id, to: step.id, kind: "blocks", in: space)

        let links = try await store.links(of: plan.id, in: space)
        #expect(links.map(\.label) == ["blocks", "mentioned in", "has part"])
        #expect(links.map(\.outgoing) == [true, false, false])
        #expect(links.last?.author.name == "Ada")
        #expect(try await store.linkKinds(in: space).count == 7)

        try await store.unlink(from: plan.id, to: step.id, kind: "blocks", in: space)
        #expect(try await store.links(of: plan.id, in: space).count == 2)
    }

    @Test func graphViewsAndPositionsAddNoChanges() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: spaceURL, name: "Map").id
        let a = try await store.createNode(in: space, title: "A", body: "[[B]]")
        let b = try await store.createNode(in: space, title: "B", body: "")
        let since = Date(timeIntervalSinceNow: -3600)
        let view = try await store.saveGraphView(id: nil, name: "Recent", filter: GraphFilter(since: since), in: space)
        #expect(view.kind == .graph)
        #expect(abs((view.filter.since ?? .distantPast).timeIntervalSince(since)) < 0.002)

        let before = try await store.changes(of: a.id, in: space, limit: 50)
        try await store.setPositions([ViewPosition(nodeID: a.id, x: 10, y: 20), ViewPosition(nodeID: b.id, x: -10, y: 5)], in: view.id, space: space)
        try await store.setViewport(Viewport(x: 1, y: 2, zoom: 0.5), of: view.id, in: space)
        #expect(try await store.changes(of: a.id, in: space, limit: 50) == before)

        let graph = try await store.graph(in: space, view: view.id, filter: view.filter)
        #expect(graph.nodes.count == 2)
        #expect(graph.edges == [GraphEdgeInfo(from: a.id, to: b.id, kind: "mentions")])
        #expect(Set(graph.positions.map(\.center)) == [CGPoint(x: 10, y: 20), CGPoint(x: -10, y: 5)])
        #expect(graph.nodes.first?.createdBy == "Ada")

        let views = try await store.mapViews(in: space)
        #expect(views.first?.viewport == Viewport(x: 1, y: 2, zoom: 0.5))
        try await store.removeGraphView(view.id, in: space)
        #expect(try await store.mapViews(in: space).isEmpty)
    }

    @Test func canvasesKeepPositionsSizesAndGroups() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: spaceURL, name: "Map").id
        let note = try await store.createNode(in: space, title: "Idea", body: "First line.")
        let canvas = try await store.createCanvas(title: "Board", in: space)
        let node = try #require(canvas.nodeID)
        #expect(try await store.node(node, in: space).kind == NodeKind.canvas)
        #expect(try await store.listNodes(in: space).contains { $0.id == node && $0.kind == NodeKind.canvas })

        let group = try await store.setGroup(GroupDraft(label: "Now", x: 0, y: 0, width: 400, height: 300), in: canvas.id, space: space)
        try await store.setPositions([ViewPosition(nodeID: note.id, x: 5, y: 6, size: CGSize(width: 300, height: 150), z: 3, collapsed: true, groupID: group.id)], in: canvas.id, space: space)

        let board = try #require(try await store.canvasBoard(of: node, in: space))
        #expect(board.view.name == "Board")
        #expect(board.items.count == 1)
        #expect(board.items[0].excerpt == "First line.")
        #expect(board.items[0].position == ViewPosition(nodeID: note.id, x: 5, y: 6, size: CGSize(width: 300, height: 150), z: 3, collapsed: true, groupID: group.id))
        #expect(board.groups == [group])

        try await store.removeGroup(group.id, in: canvas.id, space: space)
        try await store.removePositions([note.id], from: canvas.id, space: space)
        let empty = try #require(try await store.canvasBoard(of: node, in: space))
        #expect(empty.items.isEmpty && empty.groups.isEmpty)
        #expect(try await store.canvasBoard(of: note.id, in: space) == nil)
    }
}
