import Foundation

// The map and the attachments of a space through Knode Service: link kinds, labelled links, graph
// views, canvases, positions, groups and attached files (`PROTOCOL.md`, "Link kinds, views,
// canvases and attachments"). The service announces each change to its other clients, so an
// agent sees a canvas as the person arranges it.

extension ServiceSpaceStore: MapStore {
    public func linkKinds(in space: Space.ID) async throws -> [LinkKindInfo] {
        let path = try path(space)
        return try await call("link_kinds.list", ["space": .string(path)], as: [WireLinkKind].self).map(LinkKindInfo.init)
    }

    public func setLinkKind(_ kind: LinkKindInfo, in space: Space.ID) async throws -> LinkKindInfo {
        let path = try path(space)
        let params: [String: JSONValue] = [
            "space": .string(path), "kind": .string(kind.kind), "label": .string(kind.label),
            "inverse_label": .string(kind.inverseLabel), "direction": .string(kind.direction.rawValue),
            "description": .string(kind.description), "by": author,
        ]
        return LinkKindInfo(try await call("link_kind.set", params, as: WireLinkKind.self))
    }

    public func links(of node: Node.ID, in space: Space.ID) async throws -> [NodeLinkInfo] {
        let path = try path(space)
        return try await call("links.of", ["space": .string(path), "id": .string(node)], as: [WireNodeLink].self, node: node).map(NodeLinkInfo.init)
    }

    public func link(from: Node.ID, to: Node.ID, kind: String, in space: Space.ID) async throws {
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "from": .string(from), "to": .string(to), "kind": .string(kind), "by": author]
        _ = try await call("link.create", params, as: WireLink.self, node: from)
    }

    public func unlink(from: Node.ID, to: Node.ID, kind: String, in space: Space.ID) async throws {
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "from": .string(from), "to": .string(to), "kind": .string(kind), "by": author]
        _ = try await call("link.delete", params, as: Bool.self, node: from)
    }

    public func mapViews(in space: Space.ID) async throws -> [MapView] {
        let path = try path(space)
        return try await call("views.list", ["space": .string(path)], as: [WireView].self).compactMap { MapView($0) }
    }

    public func saveGraphView(id: MapView.ID?, name: String, filter: GraphFilter, in space: Space.ID) async throws -> MapView {
        let path = try path(space)
        var params: [String: JSONValue] = ["space": .string(path), "name": .string(name), "by": author]
        if let id { params["id"] = .string(id) }
        if !filter.isEmpty { params["filter"] = filter.wire }
        guard let view = MapView(try await call("view.set", params, as: WireView.self)) else {
            throw SpaceStoreError.storage("the view could not be read back")
        }
        return view
    }

    public func removeGraphView(_ id: MapView.ID, in space: Space.ID) async throws {
        let path = try path(space)
        _ = try await call("view.remove", ["space": .string(path), "id": .string(id), "by": author], as: Bool.self)
    }

    public func setViewport(_ viewport: Viewport, of view: MapView.ID, in space: Space.ID) async throws {
        let path = try path(space)
        let params: [String: JSONValue] = [
            "space": .string(path), "id": .string(view), "x": .double(viewport.x), "y": .double(viewport.y),
            "zoom": .double(viewport.zoom), "by": author,
        ]
        _ = try await call("view.viewport.set", params, as: WireNothing?.self)
    }

    public func graph(in space: Space.ID, view: MapView.ID?, filter: GraphFilter) async throws -> GraphData {
        let path = try path(space)
        var params: [String: JSONValue] = ["space": .string(path)]
        if let view { params["view"] = .string(view) }
        // An empty filter is still sent: it shows the whole space, not the view's own filter.
        params["filter"] = filter.wire
        let graph = try await call("graph", params, as: WireGraph.self)
        return GraphData(
            nodes: graph.nodes.map(GraphNodeInfo.init),
            edges: graph.edges.map(GraphEdgeInfo.init),
            positions: graph.positions.map(ViewPosition.init),
            linkKinds: graph.linkKinds.map(LinkKindInfo.init)
        )
    }

    public func setPositions(_ positions: [ViewPosition], in view: MapView.ID, space: Space.ID) async throws {
        guard !positions.isEmpty else { return }
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "view": .string(view), "positions": .array(positions.map(\.wire)), "by": author]
        _ = try await call("positions.set", params, as: WireNothing?.self)
    }

    public func removePositions(_ nodes: [Node.ID], from view: MapView.ID, space: Space.ID) async throws {
        guard !nodes.isEmpty else { return }
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "view": .string(view), "nodes": .array(nodes.map(JSONValue.string)), "by": author]
        _ = try await call("positions.remove", params, as: Int64.self)
    }

    public func createCanvas(title: String, in space: Space.ID) async throws -> MapView {
        let path = try path(space)
        let view = try await call("canvas.create", ["space": .string(path), "title": .string(title), "by": author], as: WireView.self)
        guard var canvas = MapView(view) else { throw SpaceStoreError.storage("the canvas could not be read back") }
        if canvas.name.isEmpty { canvas.name = title }
        return canvas
    }

    public func canvasBoard(of node: Node.ID, in space: Space.ID) async throws -> CanvasBoard? {
        let path = try path(space)
        guard let board = try await call("canvas.board", ["space": .string(path), "id": .string(node)], as: WireBoard?.self, node: node),
              var view = MapView(board.view)
        else { return nil }
        view.name = board.title
        return CanvasBoard(
            view: view,
            items: board.items.map { CanvasItem(position: ViewPosition($0.position), kind: $0.kind, title: $0.title, excerpt: $0.excerpt) },
            groups: board.groups.map(ViewGroupInfo.init),
            edges: board.edges.map(GraphEdgeInfo.init),
            linkKinds: board.linkKinds.map(LinkKindInfo.init)
        )
    }

    public func setGroup(_ group: GroupDraft, in view: MapView.ID, space: Space.ID) async throws -> ViewGroupInfo {
        let path = try path(space)
        var params: [String: JSONValue] = [
            "space": .string(path), "view": .string(view), "label": .string(group.label),
            "x": .double(group.x), "y": .double(group.y), "width": .double(group.width), "height": .double(group.height),
            "display": .string(group.collapsed ? "collapsed" : "open"), "by": author,
        ]
        if let id = group.id { params["id"] = .string(id) }
        return ViewGroupInfo(try await call("group.set", params, as: WireGroup.self))
    }

    public func removeGroup(_ id: ViewGroupInfo.ID, in view: MapView.ID, space: Space.ID) async throws {
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "view": .string(view), "id": .string(id), "by": author]
        _ = try await call("group.remove", params, as: Bool.self)
    }
}

extension ServiceSpaceStore: AttachmentStore {
    /// The most bytes `attachment.add` takes; a larger pasted file goes through a file.
    static let maxInlineAttachment = 10 * 1024 * 1024

    public func attachments(of node: Node.ID, in space: Space.ID) async throws -> [AttachmentInfo] {
        let path = try path(space)
        return try await call("attachments.list", ["space": .string(path), "id": .string(node)], as: [WireAttachment].self, node: node).map(AttachmentInfo.init)
    }

    /// Attaches the file as the person: the service reads it from where it is.
    public func attachFile(at url: URL, to node: Node.ID, in space: Space.ID) async throws -> AttachmentInfo {
        let path = try path(space)
        let file = Self.path(of: url)
        let params: [String: JSONValue] = ["space": .string(path), "id": .string(node), "path": .string(file), "by": author]
        return AttachmentInfo(try await call("attachment.add_file", params, as: WireAttachment.self, node: node))
    }

    public func attach(_ data: Data, named name: String, to node: Node.ID, in space: Space.ID) async throws -> AttachmentInfo {
        guard data.count <= Self.maxInlineAttachment else {
            // Too large to send inline: written to a file of that name, attached from there.
            let folder = FileManager.default.temporaryDirectory.appending(path: "Knode-\(UUID().uuidString)", directoryHint: .isDirectory)
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            defer { try? FileManager.default.removeItem(at: folder) }
            let file = folder.appending(path: name.replacing("/", with: "-"))
            try data.write(to: file)
            return try await attachFile(at: file, to: node, in: space)
        }
        let path = try path(space)
        let params: [String: JSONValue] = [
            "space": .string(path), "id": .string(node), "name": .string(name),
            "content": .string(data.base64EncodedString()), "by": author,
        ]
        return AttachmentInfo(try await call("attachment.add", params, as: WireAttachment.self, node: node))
    }

    public func removeAttachment(named name: String, from node: Node.ID, in space: Space.ID) async throws {
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "id": .string(node), "name": .string(name), "by": author]
        _ = try await call("attachment.remove", params, as: Bool.self, node: node)
    }

    public func attachmentURL(named name: String, of node: Node.ID, in space: Space.ID) async throws -> URL? {
        let path = try path(space)
        let found = try await call("attachment.path", ["space": .string(path), "id": .string(node), "name": .string(name)], as: String?.self, node: node)
        return found.map { URL(filePath: $0) }
    }
}

// MARK: - Wire types

/// A result the app does not read, such as the `null` of `positions.set`.
struct WireNothing: Decodable, Sendable {
    init(from decoder: any Decoder) throws {}
}

struct WireLinkKind: Decodable, Sendable {
    var kind: String
    var label: String
    var inverseLabel: String?
    var direction: String
    var description: String?
    var builtIn: Bool?
}

struct WireLinkDetail: Decodable, Sendable {
    var from: String
    var to: String
    var kind: String
    var createdAt: Int64
    var createdBy: WireProvenance
}

struct WireNodeLink: Decodable, Sendable {
    var link: WireLinkDetail
    var outgoing: Bool
    var node: String
    var title: String
    var label: String
    var direction: String
}

struct WireViewport: Decodable, Sendable {
    var x: Double
    var y: Double
    var zoom: Double
}

/// A stored filter. `since` and `until` are RFC 3339 text or Unix milliseconds.
struct WireFilter: Decodable, Sendable {
    var nodeKinds: [String]?
    var tags: [String]?
    var actors: [String]?
    var since: Date?
    var until: Date?
    var linkKinds: [String]?

    enum CodingKeys: String, CodingKey { case nodeKinds, tags, actors, since, until, linkKinds }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        nodeKinds = try? container.decodeIfPresent([String].self, forKey: .nodeKinds)
        tags = try? container.decodeIfPresent([String].self, forKey: .tags)
        actors = try? container.decodeIfPresent([String].self, forKey: .actors)
        linkKinds = try? container.decodeIfPresent([String].self, forKey: .linkKinds)
        since = Self.time(container, .since)
        until = Self.time(container, .until)
    }

    private static func time(_ container: KeyedDecodingContainer<CodingKeys>, _ key: CodingKeys) -> Date? {
        if let millis = try? container.decodeIfPresent(Int64.self, forKey: key) { return Date(unixMilliseconds: millis) }
        guard let text = try? container.decodeIfPresent(String.self, forKey: key) else { return nil }
        return GraphFilter.date(rfc3339: text)
    }
}

struct WireViewSettings: Decodable, Sendable {
    var viewport: WireViewport?
    var filter: WireFilter?
}

struct WireView: Decodable, Sendable {
    var id: String
    var kind: String
    var name: String
    var nodeId: String?
    var settings: WireViewSettings?

    enum CodingKeys: String, CodingKey { case id, kind, name, nodeId, settings }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        kind = try container.decode(String.self, forKey: .kind)
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        nodeId = try container.decodeIfPresent(String.self, forKey: .nodeId)
        // Settings this version can't read show the view without them.
        settings = try? container.decodeIfPresent(WireViewSettings.self, forKey: .settings)
    }
}

struct WirePosition: Decodable, Sendable {
    var nodeId: String
    var x: Double
    var y: Double
    var width: Double?
    var height: Double?
    var z: Int64?
    var display: String?
    var groupId: String?
}

struct WireGroup: Decodable, Sendable {
    var id: String
    var label: String?
    var x: Double
    var y: Double
    var width: Double
    var height: Double
    var display: String?
}

struct WireGraphNode: Decodable, Sendable {
    var id: String
    var kind: String
    var title: String
    var tags: [String]?
    var createdAt: Int64
    var updatedAt: Int64
    var createdBy: WireProvenance
    var updatedBy: WireProvenance
}

struct WireEdge: Decodable, Sendable {
    var from: String
    var to: String
    var kind: String
}

struct WireGraph: Decodable, Sendable {
    var nodes: [WireGraphNode]
    var edges: [WireEdge]
    var positions: [WirePosition]
    var linkKinds: [WireLinkKind]
}

struct WireCanvasItem: Decodable, Sendable {
    var position: WirePosition
    var kind: String
    var title: String
    var excerpt: String
}

struct WireBoard: Decodable, Sendable {
    var view: WireView
    var title: String
    var items: [WireCanvasItem]
    var groups: [WireGroup]
    var edges: [WireEdge]
    var linkKinds: [WireLinkKind]
}

struct WireAttachment: Decodable, Sendable {
    var nodeId: String
    var name: String
    var hash: String
    var size: Int64
    var mediaType: String
    var createdAt: Int64
    var createdBy: WireProvenance
}

extension LinkKindInfo {
    init(_ kind: WireLinkKind) {
        self.init(
            kind: kind.kind, label: kind.label, inverseLabel: kind.inverseLabel ?? "",
            direction: LinkDirection(rawValue: kind.direction) ?? .directed,
            description: kind.description ?? "", builtIn: kind.builtIn ?? false
        )
    }
}

extension NodeLinkInfo {
    init(_ link: WireNodeLink) {
        self.init(
            from: link.link.from, to: link.link.to, kind: link.link.kind, outgoing: link.outgoing, nodeID: link.node,
            title: link.title, label: link.label, direction: LinkDirection(rawValue: link.direction) ?? .directed,
            createdAt: Date(unixMilliseconds: link.link.createdAt), author: Author(link.link.createdBy)
        )
    }
}

extension GraphFilter {
    init(_ filter: WireFilter) {
        self.init(nodeKinds: filter.nodeKinds, tags: filter.tags, actors: filter.actors, since: filter.since, until: filter.until, linkKinds: filter.linkKinds)
    }

    /// The filter as the service takes it, with times as RFC 3339.
    var wire: JSONValue {
        var object: [String: JSONValue] = [:]
        let lists = [("node_kinds", nodeKinds), ("tags", tags), ("actors", actors), ("link_kinds", linkKinds)]
        for (key, list) in lists {
            if let list { object[key] = .array(list.map(JSONValue.string)) }
        }
        if let since { object["since"] = .string(Self.rfc3339(since)) }
        if let until { object["until"] = .string(Self.rfc3339(until)) }
        return .object(object)
    }

    /// RFC 3339 with milliseconds, such as `2026-10-20T09:30:00.000Z`.
    static func rfc3339(_ date: Date) -> String {
        // Whole milliseconds, as the space stores them.
        let millis = (date.timeIntervalSince1970 * 1000).rounded()
        return Date(timeIntervalSince1970: millis / 1000).formatted(Date.ISO8601FormatStyle(includingFractionalSeconds: true))
    }

    static func date(rfc3339 text: String) -> Date? {
        let text = text.trimmingCharacters(in: .whitespaces)
        return (try? Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse(text))
            ?? (try? Date.ISO8601FormatStyle().parse(text))
    }
}

extension MapView {
    /// A view of a kind this version knows, or nil.
    init?(_ view: WireView) {
        guard let kind = Kind(rawValue: view.kind) else { return nil }
        self.init(
            id: view.id, kind: kind, name: view.name, nodeID: view.nodeId,
            viewport: view.settings?.viewport.map { Viewport(x: $0.x, y: $0.y, zoom: $0.zoom) },
            filter: view.settings?.filter.map(GraphFilter.init) ?? GraphFilter()
        )
    }
}

extension ViewPosition {
    init(_ position: WirePosition) {
        let size: CGSize? = if let width = position.width, let height = position.height { CGSize(width: width, height: height) } else { nil }
        self.init(
            nodeID: position.nodeId, x: position.x, y: position.y, size: size, z: position.z ?? 0,
            collapsed: position.display == "collapsed", groupID: position.groupId
        )
    }

    /// The position as `positions.set` takes it.
    var wire: JSONValue {
        var object: [String: JSONValue] = [
            "node_id": .string(nodeID), "x": .double(x), "y": .double(y), "z": .int(z),
            "display": .string(collapsed ? "collapsed" : "open"),
        ]
        if let size {
            object["width"] = .double(Double(size.width))
            object["height"] = .double(Double(size.height))
        }
        if let groupID { object["group_id"] = .string(groupID) }
        return .object(object)
    }
}

extension ViewGroupInfo {
    init(_ group: WireGroup) {
        self.init(id: group.id, label: group.label ?? "", x: group.x, y: group.y, width: group.width, height: group.height, collapsed: group.display == "collapsed")
    }
}

extension GraphNodeInfo {
    init(_ node: WireGraphNode) {
        self.init(
            id: node.id, kind: node.kind, title: node.title, tags: node.tags ?? [],
            createdAt: Date(unixMilliseconds: node.createdAt), updatedAt: Date(unixMilliseconds: node.updatedAt),
            createdBy: node.createdBy.actor, updatedBy: node.updatedBy.actor
        )
    }
}

extension GraphEdgeInfo {
    init(_ edge: WireEdge) {
        self.init(from: edge.from, to: edge.to, kind: edge.kind)
    }
}

extension AttachmentInfo {
    init(_ attachment: WireAttachment) {
        self.init(
            nodeID: attachment.nodeId, name: attachment.name, hash: attachment.hash, size: attachment.size,
            mediaType: attachment.mediaType, createdAt: Date(unixMilliseconds: attachment.createdAt), author: Author(attachment.createdBy)
        )
    }
}
