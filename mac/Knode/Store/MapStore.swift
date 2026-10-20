import Foundation

/// What the map of a space needs: typed links, graph views and canvases (section 18 of the
/// format).
///
/// `CoreSpaceStore` implements it with the Rust core, `RoutingSpaceStore` with the core in the app
/// even while Knode Service holds the space (the service has no view methods yet), and
/// `PreviewSpaceStore` in memory. Moving the viewport, positions and groups adds no change to the
/// history of the space.
public protocol MapStore: Sendable {
    /// Every kind of link: the built-in kinds, then the ones the space defined.
    func linkKinds(in space: Space.ID) async throws -> [LinkKindInfo]

    /// Defines a kind of link, or changes its labels and description.
    func setLinkKind(_ kind: LinkKindInfo, in space: Space.ID) async throws -> LinkKindInfo

    /// The links of a node, going out then coming in, with the label each reads with from it.
    func links(of node: Node.ID, in space: Space.ID) async throws -> [NodeLinkInfo]

    /// Links two nodes with a kind. Linking twice changes nothing.
    func link(from: Node.ID, to: Node.ID, kind: String, in space: Space.ID) async throws

    /// Removes a link made by hand.
    func unlink(from: Node.ID, to: Node.ID, kind: String, in space: Space.ID) async throws

    /// The graph views and canvases of the space, oldest first.
    func mapViews(in space: Space.ID) async throws -> [MapView]

    /// Creates a graph view (`id` nil), or renames it or changes its filter.
    func saveGraphView(id: MapView.ID?, name: String, filter: GraphFilter, in space: Space.ID) async throws -> MapView

    /// Removes a graph view and its positions.
    func removeGraphView(_ id: MapView.ID, in space: Space.ID) async throws

    /// Saves the part of a view on screen. Adds no change.
    func setViewport(_ viewport: Viewport, of view: MapView.ID, in space: Space.ID) async throws

    /// The nodes a filter keeps and the links between them, with the positions saved in `view`.
    func graph(in space: Space.ID, view: MapView.ID?, filter: GraphFilter) async throws -> GraphData

    /// Places nodes in a view in one write. Adds no change.
    func setPositions(_ positions: [ViewPosition], in view: MapView.ID, space: Space.ID) async throws

    /// Takes nodes out of a view; the nodes stay in the space.
    func removePositions(_ nodes: [Node.ID], from view: MapView.ID, space: Space.ID) async throws

    /// Creates a canvas: a node of kind `canvas` with this title, and its board.
    func createCanvas(title: String, in space: Space.ID) async throws -> MapView

    /// The board of a canvas node, or nil when the node is not a canvas.
    func canvasBoard(of node: Node.ID, in space: Space.ID) async throws -> CanvasBoard?

    /// Adds a group to a canvas (`group.id` nil) or changes one. Adds no change.
    func setGroup(_ group: GroupDraft, in view: MapView.ID, space: Space.ID) async throws -> ViewGroupInfo

    /// Removes a group; its nodes stay on the canvas.
    func removeGroup(_ id: ViewGroupInfo.ID, in space: Space.ID) async throws
}

// MARK: - Models

/// Whether a kind of link reads the same from both nodes.
public enum LinkDirection: String, Sendable, Hashable, CaseIterable {
    case directed, symmetric
}

/// A kind of link: built in, or defined by the space.
public struct LinkKindInfo: Identifiable, Hashable, Sendable {
    public var kind: String
    /// How it reads from its source, such as "part of".
    public var label: String
    /// How it reads from its target, such as "has part". Empty for none.
    public var inverseLabel: String
    public var direction: LinkDirection
    public var description: String
    public var builtIn: Bool
    public var id: String { kind }

    public init(kind: String, label: String, inverseLabel: String = "", direction: LinkDirection = .directed, description: String = "", builtIn: Bool = false) {
        self.kind = kind
        self.label = label
        self.inverseLabel = inverseLabel
        self.direction = direction
        self.description = description
        self.builtIn = builtIn
    }

    /// How a link of this kind reads from one of its nodes.
    public func label(outgoing: Bool) -> String {
        if outgoing || direction == .symmetric { return label }
        return inverseLabel.isEmpty ? kind : inverseLabel
    }

    /// The kind every link is made with by hand unless another is chosen.
    public static let related = "related"
    /// Links written from `[[links]]` in a body; never made by hand.
    public static let mentions = "mentions"

    /// The built-in kinds, as the format defines them.
    public static let builtIns: [LinkKindInfo] = [
        LinkKindInfo(kind: "mentions", label: "mentions", inverseLabel: "mentioned in", description: "The source's body has a [[link]] to the target.", builtIn: true),
        LinkKindInfo(kind: "related", label: "related to", inverseLabel: "related to", direction: .symmetric, description: "The nodes are related.", builtIn: true),
        LinkKindInfo(kind: "part-of", label: "part of", inverseLabel: "has part", description: "The source is part of the target.", builtIn: true),
        LinkKindInfo(kind: "derived-from", label: "derived from", inverseLabel: "source of", description: "The source was made from the target.", builtIn: true),
        LinkKindInfo(kind: "answers", label: "answers", inverseLabel: "answered by", description: "The source answers a question the target asks.", builtIn: true),
        LinkKindInfo(kind: "contradicts", label: "contradicts", inverseLabel: "contradicted by", description: "The source says the opposite of the target.", builtIn: true),
    ]

    /// Whether a name follows the rule for kinds: lower-case letters, digits and `-`, starting
    /// with a letter, at most 32 characters.
    public static func isValidName(_ name: String) -> Bool {
        guard let first = name.unicodeScalars.first, name.unicodeScalars.count <= 32,
              ("a"..."z").contains(first) else { return false }
        return name.unicodeScalars.allSatisfy { ("a"..."z").contains($0) || ("0"..."9").contains($0) || $0 == "-" }
    }

    /// A name for a kind made from its label: "depends on" becomes `depends-on`.
    public static func name(fromLabel label: String) -> String {
        let lowered = label.lowercased().unicodeScalars.map { scalar -> Character in
            ("a"..."z").contains(scalar) || ("0"..."9").contains(scalar) ? Character(scalar) : "-"
        }
        let parts = String(lowered).split(separator: "-", omittingEmptySubsequences: true)
        var name = parts.joined(separator: "-")
        while let first = name.first, !first.isLetter { name.removeFirst() }
        return String(name.prefix(32)).trimmingCharacters(in: CharacterSet(charactersIn: "-"))
    }
}

/// A link as one of its nodes shows it.
public struct NodeLinkInfo: Identifiable, Hashable, Sendable {
    public var from: Node.ID
    public var to: Node.ID
    public var kind: String
    /// True for a link going out (or to the node itself), false for a backlink.
    public var outgoing: Bool
    /// The node at the other end.
    public var nodeID: Node.ID
    public var title: String
    /// How the link reads from the node.
    public var label: String
    public var direction: LinkDirection
    public var createdAt: Date
    public var author: Author
    public var id: String { "\(from) \(kind) \(to)" }

    public init(from: Node.ID, to: Node.ID, kind: String, outgoing: Bool, nodeID: Node.ID, title: String, label: String, direction: LinkDirection, createdAt: Date, author: Author) {
        self.from = from
        self.to = to
        self.kind = kind
        self.outgoing = outgoing
        self.nodeID = nodeID
        self.title = title
        self.label = label
        self.direction = direction
        self.createdAt = createdAt
        self.author = author
    }
}

/// The part of a view's plane on screen: the point at the centre and the zoom.
public struct Viewport: Hashable, Sendable {
    public var x: Double
    public var y: Double
    public var zoom: Double

    public init(x: Double = 0, y: Double = 0, zoom: Double = 1) {
        self.x = x
        self.y = y
        self.zoom = zoom
    }

    public static let minZoom = 0.05
    public static let maxZoom = 4.0

    /// Where a point of the plane is drawn in a view of this size.
    public func toScreen(_ point: CGPoint, in size: CGSize) -> CGPoint {
        CGPoint(x: (point.x - x) * zoom + size.width / 2, y: (point.y - y) * zoom + size.height / 2)
    }

    /// The point of the plane under a point of the screen.
    public func toPlane(_ point: CGPoint, in size: CGSize) -> CGPoint {
        CGPoint(x: (point.x - size.width / 2) / zoom + x, y: (point.y - size.height / 2) / zoom + y)
    }

    /// Moves the plane with the pointer, by a distance on screen.
    public func panned(by translation: CGSize) -> Viewport {
        Viewport(x: x - translation.width / zoom, y: y - translation.height / zoom, zoom: zoom)
    }

    /// Zooms by a factor, keeping the point of the plane under `anchor` (on screen) in place.
    public func zoomed(by factor: Double, at anchor: CGPoint, in size: CGSize) -> Viewport {
        let newZoom = min(max(zoom * factor, Self.minZoom), Self.maxZoom)
        let fixed = toPlane(anchor, in: size)
        return Viewport(
            x: fixed.x - (anchor.x - size.width / 2) / newZoom,
            y: fixed.y - (anchor.y - size.height / 2) / newZoom,
            zoom: newZoom
        )
    }

    /// The viewport that shows a rectangle of the plane whole, with a margin on screen.
    public static func fitting(_ rect: CGRect, in size: CGSize, margin: Double = 40) -> Viewport {
        guard !rect.isNull, size.width > 0, size.height > 0 else { return Viewport() }
        let width = max(rect.width, 1), height = max(rect.height, 1)
        let zoom = min((size.width - 2 * margin) / width, (size.height - 2 * margin) / height)
        return Viewport(x: rect.midX, y: rect.midY, zoom: min(max(zoom, minZoom), 2))
    }
}

/// Which nodes and links a graph view shows; nil leaves a field out.
public struct GraphFilter: Hashable, Sendable {
    public var nodeKinds: [String]?
    public var tags: [String]?
    public var actors: [String]?
    /// Changed at or after.
    public var since: Date?
    /// Changed before.
    public var until: Date?
    public var linkKinds: [String]?

    public init(nodeKinds: [String]? = nil, tags: [String]? = nil, actors: [String]? = nil, since: Date? = nil, until: Date? = nil, linkKinds: [String]? = nil) {
        self.nodeKinds = nodeKinds
        self.tags = tags
        self.actors = actors
        self.since = since
        self.until = until
        self.linkKinds = linkKinds
    }

    public var isEmpty: Bool { self == GraphFilter() }

    /// How many fields are set, for the filter button.
    public var count: Int {
        [nodeKinds != nil, tags != nil, actors != nil, since != nil || until != nil, linkKinds != nil].filter { $0 }.count
    }
}

/// A view of a space: a named graph view or a canvas.
public struct MapView: Identifiable, Hashable, Sendable {
    public enum Kind: String, Sendable { case graph, canvas }

    public typealias ID = String
    public var id: ID
    public var kind: Kind
    /// The name of a graph view; for a canvas, its node's title.
    public var name: String
    /// The node of a canvas.
    public var nodeID: Node.ID?
    public var viewport: Viewport?
    public var filter: GraphFilter

    public init(id: ID, kind: Kind, name: String, nodeID: Node.ID? = nil, viewport: Viewport? = nil, filter: GraphFilter = GraphFilter()) {
        self.id = id
        self.kind = kind
        self.name = name
        self.nodeID = nodeID
        self.viewport = viewport
        self.filter = filter
    }
}

/// Where a node is in a view.
public struct ViewPosition: Hashable, Sendable {
    public var nodeID: Node.ID
    /// The centre, in points of the view's plane; y grows down.
    public var x: Double
    public var y: Double
    /// The size of the node's box, or nil for the default size.
    public var size: CGSize?
    /// Stacking order on a canvas: higher is in front.
    public var z: Int64
    /// Only the title is shown.
    public var collapsed: Bool
    public var groupID: ViewGroupInfo.ID?

    public init(nodeID: Node.ID, x: Double, y: Double, size: CGSize? = nil, z: Int64 = 0, collapsed: Bool = false, groupID: ViewGroupInfo.ID? = nil) {
        self.nodeID = nodeID
        self.x = x
        self.y = y
        self.size = size
        self.z = z
        self.collapsed = collapsed
        self.groupID = groupID
    }

    public var center: CGPoint {
        get { CGPoint(x: x, y: y) }
        set { x = newValue.x; y = newValue.y }
    }
}

/// A labelled box on a canvas that holds nodes.
public struct ViewGroupInfo: Identifiable, Hashable, Sendable {
    public typealias ID = String
    public var id: ID
    public var label: String
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double
    public var collapsed: Bool

    public init(id: ID, label: String, x: Double, y: Double, width: Double, height: Double, collapsed: Bool = false) {
        self.id = id
        self.label = label
        self.x = x
        self.y = y
        self.width = width
        self.height = height
        self.collapsed = collapsed
    }

    /// The box, in points of the plane.
    public var frame: CGRect {
        CGRect(x: x - width / 2, y: y - height / 2, width: width, height: height)
    }

    public var draft: GroupDraft {
        GroupDraft(id: id, label: label, x: x, y: y, width: width, height: height, collapsed: collapsed)
    }
}

/// A group to add (`id` nil) or change.
public struct GroupDraft: Hashable, Sendable {
    public var id: ViewGroupInfo.ID?
    public var label: String
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double
    public var collapsed: Bool

    public init(id: ViewGroupInfo.ID? = nil, label: String, x: Double, y: Double, width: Double, height: Double, collapsed: Bool = false) {
        self.id = id
        self.label = label
        self.x = x
        self.y = y
        self.width = width
        self.height = height
        self.collapsed = collapsed
    }
}

/// A node of the graph.
public struct GraphNodeInfo: Identifiable, Hashable, Sendable {
    public var id: Node.ID
    public var kind: String
    public var title: String
    public var tags: [String]
    public var createdAt: Date
    public var updatedAt: Date
    /// Who created it and who changed it last.
    public var createdBy: String
    public var updatedBy: String

    public init(id: Node.ID, kind: String, title: String, tags: [String] = [], createdAt: Date, updatedAt: Date, createdBy: String, updatedBy: String) {
        self.id = id
        self.kind = kind
        self.title = title
        self.tags = tags
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.createdBy = createdBy
        self.updatedBy = updatedBy
    }
}

/// A link of the graph.
public struct GraphEdgeInfo: Hashable, Sendable {
    public var from: Node.ID
    public var to: Node.ID
    public var kind: String

    public init(from: Node.ID, to: Node.ID, kind: String) {
        self.from = from
        self.to = to
        self.kind = kind
    }
}

/// What a graph view shows.
public struct GraphData: Sendable {
    public var nodes: [GraphNodeInfo]
    public var edges: [GraphEdgeInfo]
    public var positions: [ViewPosition]
    public var linkKinds: [LinkKindInfo]

    public init(nodes: [GraphNodeInfo], edges: [GraphEdgeInfo], positions: [ViewPosition] = [], linkKinds: [LinkKindInfo] = LinkKindInfo.builtIns) {
        self.nodes = nodes
        self.edges = edges
        self.positions = positions
        self.linkKinds = linkKinds
    }
}

/// A node on a canvas, with what its card shows.
public struct CanvasItem: Identifiable, Hashable, Sendable {
    public var position: ViewPosition
    public var kind: String
    public var title: String
    public var excerpt: String
    public var id: Node.ID { position.nodeID }

    public init(position: ViewPosition, kind: String, title: String, excerpt: String) {
        self.position = position
        self.kind = kind
        self.title = title
        self.excerpt = excerpt
    }
}

/// A canvas as its board shows it.
public struct CanvasBoard: Sendable {
    public var view: MapView
    public var items: [CanvasItem]
    public var groups: [ViewGroupInfo]
    public var edges: [GraphEdgeInfo]
    public var linkKinds: [LinkKindInfo]

    public init(view: MapView, items: [CanvasItem], groups: [ViewGroupInfo], edges: [GraphEdgeInfo], linkKinds: [LinkKindInfo]) {
        self.view = view
        self.items = items
        self.groups = groups
        self.edges = edges
        self.linkKinds = linkKinds
    }
}

/// Kinds of node the format names.
public enum NodeKind {
    public static let note = "note"
    public static let conversation = "conversation"
    public static let decision = "decision"
    public static let idea = "idea"
    public static let source = "source"
    public static let canvas = "canvas"
}
