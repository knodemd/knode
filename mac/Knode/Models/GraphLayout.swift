import CoreGraphics
import Foundation

/// A force-directed layout of a graph: nodes push each other apart, links pull their nodes
/// together, and a weak pull towards the centre keeps loose nodes in sight.
///
/// It follows the velocity Verlet model of d3-force. Repulsion is computed with a Barnes-Hut
/// quadtree, so a step costs O(n log n): far groups of nodes act as one body at their centre of
/// mass. `alpha` cools from 1 towards 0 and the layout stops moving once it is below `alphaMin`.
///
/// A value type, so a copy can be stepped on another thread.
struct GraphLayout: Sendable {
    /// Positions and velocities, in points of the plane.
    private(set) var x: [Double]
    private(set) var y: [Double]
    private var vx: [Double]
    private var vy: [Double]
    /// Nodes held in place, such as the one being dragged.
    private var fixed: [Bool]
    /// Links as pairs of node indices.
    let sources: [Int32]
    let targets: [Int32]
    /// How many links each node has.
    let degree: [Int32]
    private let bias: [Double]
    private let strength: [Double]

    var alpha: Double = 1
    var alphaTarget: Double = 0
    var alphaMin: Double = 0.001
    /// How much alpha cools each step: about 300 steps from 1 to `alphaMin`.
    var alphaDecay: Double = 1 - pow(0.001, 1.0 / 300)
    /// The share of velocity lost each step.
    var velocityDecay: Double = 0.4
    /// Repulsion between two nodes; negative pushes apart.
    var charge: Double = -40
    /// Where repulsion stops counting, squared, so far components do not fly apart.
    var maxDistance2: Double = 1200 * 1200
    /// The Barnes-Hut criterion, squared: a cell this much narrower than its distance acts as one
    /// body.
    var theta2: Double = 0.81
    /// How long a link wants to be.
    var linkDistance: Double = 36
    /// The pull towards the origin.
    var gravity: Double = 0.03

    var count: Int { x.count }
    var isSettled: Bool { alpha < alphaMin && alphaTarget < alphaMin }

    /// - Parameters:
    ///   - count: how many nodes.
    ///   - links: pairs of node indices; links to themselves and out of range are ignored.
    ///   - positions: where nodes start, by index; the others are placed on a spiral.
    init(count: Int, links: [(Int, Int)], positions: [Int: CGPoint] = [:]) {
        var sources: [Int32] = []
        var targets: [Int32] = []
        var degree = [Int32](repeating: 0, count: count)
        sources.reserveCapacity(links.count)
        targets.reserveCapacity(links.count)
        for (s, t) in links where s != t && s >= 0 && t >= 0 && s < count && t < count {
            sources.append(Int32(s))
            targets.append(Int32(t))
            degree[s] += 1
            degree[t] += 1
        }
        self.sources = sources
        self.targets = targets
        self.degree = degree
        bias = zip(sources, targets).map { s, t in
            Double(degree[Int(s)]) / Double(degree[Int(s)] + degree[Int(t)])
        }
        strength = zip(sources, targets).map { s, t in
            1 / Double(min(degree[Int(s)], degree[Int(t)]))
        }
        x = []
        y = []
        x.reserveCapacity(count)
        y.reserveCapacity(count)
        for index in 0..<count {
            let point = positions[index] ?? Self.spiral(index)
            x.append(point.x)
            y.append(point.y)
        }
        vx = Array(repeating: 0, count: count)
        vy = Array(repeating: 0, count: count)
        fixed = Array(repeating: false, count: count)
    }

    /// Where node `index` starts without a saved position: a phyllotaxis spiral, so the first
    /// step has no two nodes on the same point.
    static func spiral(_ index: Int) -> CGPoint {
        let radius = 10 * (0.5 + Double(index)).squareRoot()
        let angle = Double(index) * Double.pi * (3 - 5.0.squareRoot())
        return CGPoint(x: radius * cos(angle), y: radius * sin(angle))
    }

    func position(_ index: Int) -> CGPoint {
        CGPoint(x: x[index], y: y[index])
    }

    /// Puts a node somewhere and holds it there while `fixed`, as when it is dragged.
    mutating func place(_ index: Int, at point: CGPoint, fixed hold: Bool) {
        x[index] = point.x
        y[index] = point.y
        vx[index] = 0
        vy[index] = 0
        fixed[index] = hold
    }

    /// Lets every held node move again.
    mutating func releaseAll() {
        for index in fixed.indices { fixed[index] = false }
    }

    /// Warms the layout up again, as after a drag or new nodes.
    mutating func reheat(to value: Double = 0.3) {
        alpha = max(alpha, value)
    }

    /// Runs steps until the layout settles or `limit` steps were made.
    mutating func run(steps limit: Int) {
        var steps = 0
        while !isSettled, steps < limit {
            step()
            steps += 1
        }
    }

    /// One step of the simulation.
    mutating func step() {
        alpha += (alphaTarget - alpha) * alphaDecay
        applyLinks()
        applyCharge()
        let n = count
        let pull = gravity * alpha
        let keep = 1 - velocityDecay
        x.withUnsafeMutableBufferPointer { x in
            y.withUnsafeMutableBufferPointer { y in
                vx.withUnsafeMutableBufferPointer { vx in
                    vy.withUnsafeMutableBufferPointer { vy in
                        fixed.withUnsafeBufferPointer { fixed in
                            for i in 0..<n {
                                if fixed[i] {
                                    vx[i] = 0
                                    vy[i] = 0
                                    continue
                                }
                                vx[i] = (vx[i] - x[i] * pull) * keep
                                vy[i] = (vy[i] - y[i] * pull) * keep
                                x[i] += vx[i]
                                y[i] += vy[i]
                            }
                        }
                    }
                }
            }
        }
    }

    private mutating func applyLinks() {
        let alpha = alpha
        let distance = linkDistance
        x.withUnsafeBufferPointer { x in
            y.withUnsafeBufferPointer { y in
                vx.withUnsafeMutableBufferPointer { vx in
                    vy.withUnsafeMutableBufferPointer { vy in
                        for k in 0..<sources.count {
                            let s = Int(sources[k]), t = Int(targets[k])
                            var dx = x[t] + vx[t] - x[s] - vx[s]
                            var dy = y[t] + vy[t] - y[s] - vy[s]
                            if dx == 0 { dx = jiggle(k) }
                            if dy == 0 { dy = jiggle(k + 1) }
                            let length = (dx * dx + dy * dy).squareRoot()
                            let pull = (length - distance) / length * alpha * strength[k]
                            dx *= pull
                            dy *= pull
                            let b = bias[k]
                            vx[t] -= dx * b
                            vy[t] -= dy * b
                            vx[s] += dx * (1 - b)
                            vy[s] += dy * (1 - b)
                        }
                    }
                }
            }
        }
    }

    private mutating func applyCharge() {
        let n = count
        guard n > 1 else { return }
        let tree = QuadTree(x: x, y: y)
        let factor = charge * alpha
        let theta2 = theta2
        let maxDistance2 = maxDistance2
        var forces = [Double](repeating: 0, count: 2 * n)
        var stack: [Int32] = []
        stack.reserveCapacity(256)
        forces.withUnsafeMutableBufferPointer { forces in
            for i in 0..<n {
                let (fx, fy) = tree.force(on: i, x: x[i], y: y[i], theta2: theta2, maxDistance2: maxDistance2, stack: &stack)
                forces[2 * i] = fx
                forces[2 * i + 1] = fy
            }
        }
        for i in 0..<n {
            vx[i] += forces[2 * i] * factor
            vy[i] += forces[2 * i + 1] * factor
        }
    }

    /// The bounding box of the nodes, or `.null` when there are none.
    var bounds: CGRect {
        guard let minX = x.min(), let maxX = x.max(), let minY = y.min(), let maxY = y.max() else { return .null }
        return CGRect(x: minX, y: minY, width: maxX - minX, height: maxY - minY)
    }

    /// The node nearest to a point of the plane within `radius`, if any.
    func node(near point: CGPoint, radius: Double) -> Int? {
        var best: Int?
        var bestDistance = radius * radius
        for i in 0..<count {
            let dx = x[i] - point.x, dy = y[i] - point.y
            let d = dx * dx + dy * dy
            if d <= bestDistance {
                bestDistance = d
                best = i
            }
        }
        return best
    }

    /// The link nearest to a point of the plane within `radius`, by index, if any.
    func link(near point: CGPoint, radius: Double) -> Int? {
        var best: Int?
        var bestDistance = radius * radius
        for k in 0..<sources.count {
            let a = position(Int(sources[k])), b = position(Int(targets[k]))
            let d = Self.distance2(from: point, toSegment: a, b)
            if d <= bestDistance {
                bestDistance = d
                best = k
            }
        }
        return best
    }

    /// The squared distance from a point to a segment.
    static func distance2(from p: CGPoint, toSegment a: CGPoint, _ b: CGPoint) -> Double {
        let dx = b.x - a.x, dy = b.y - a.y
        let length2 = dx * dx + dy * dy
        var t = length2 > 0 ? ((p.x - a.x) * dx + (p.y - a.y) * dy) / length2 : 0
        t = min(max(t, 0), 1)
        let cx = a.x + t * dx - p.x, cy = a.y + t * dy - p.y
        return cx * cx + cy * cy
    }
}

/// A tiny, deterministic nudge for two nodes on the same point.
private func jiggle(_ seed: Int) -> Double {
    (Double((seed &* 2_654_435_761) & 0xFFFF) / 65_536 - 0.5) * 1e-6
}

/// A Barnes-Hut quadtree over points, kept in flat arrays: each cell knows its square, its
/// children, the number of points under it and their centre of mass.
struct QuadTree {
    private(set) var cellX: [Double] = []
    private(set) var cellY: [Double] = []
    private(set) var cellSize: [Double] = []
    /// Four children per cell, -1 for none.
    private var children: [Int32] = []
    /// The first point of a leaf, -1 for none; more points on the same spot follow in `next`.
    private var body: [Int32] = []
    private var next: [Int32]
    /// How many points are under each cell, and their centre of mass.
    private(set) var mass: [Double] = []
    private(set) var massX: [Double] = []
    private(set) var massY: [Double] = []
    private let x: [Double]
    private let y: [Double]
    /// How deep cells split before points share a leaf.
    static let maxDepth = 24

    init(x: [Double], y: [Double]) {
        self.x = x
        self.y = y
        next = Array(repeating: -1, count: x.count)
        guard let minX = x.min(), let maxX = x.max(), let minY = y.min(), let maxY = y.max() else { return }
        let size = max(maxX - minX, maxY - minY, 1) * 1.0001
        reserve(x.count * 2)
        _ = addCell(x: minX, y: minY, size: size)
        for i in x.indices { insert(i) }
        accumulate()
    }

    var cellCount: Int { cellSize.count }

    private mutating func reserve(_ capacity: Int) {
        cellX.reserveCapacity(capacity)
        cellY.reserveCapacity(capacity)
        cellSize.reserveCapacity(capacity)
        children.reserveCapacity(capacity * 4)
        body.reserveCapacity(capacity)
    }

    private mutating func addCell(x: Double, y: Double, size: Double) -> Int32 {
        cellX.append(x)
        cellY.append(y)
        cellSize.append(size)
        children.append(contentsOf: [-1, -1, -1, -1])
        body.append(-1)
        return Int32(cellSize.count - 1)
    }

    private func quadrant(of point: Int, in cell: Int) -> Int {
        let half = cellSize[cell] / 2
        let right = x[point] >= cellX[cell] + half ? 1 : 0
        let below = y[point] >= cellY[cell] + half ? 2 : 0
        return right + below
    }

    private mutating func child(_ quadrant: Int, of cell: Int) -> Int {
        let slot = cell * 4 + quadrant
        if children[slot] >= 0 { return Int(children[slot]) }
        let half = cellSize[cell] / 2
        let made = addCell(
            x: cellX[cell] + (quadrant & 1 == 1 ? half : 0),
            y: cellY[cell] + (quadrant & 2 == 2 ? half : 0),
            size: half
        )
        children[slot] = made
        return Int(made)
    }

    private func isLeaf(_ cell: Int) -> Bool {
        children[cell * 4] < 0 && children[cell * 4 + 1] < 0 && children[cell * 4 + 2] < 0 && children[cell * 4 + 3] < 0
    }

    private mutating func insert(_ point: Int) {
        var cell = 0
        var depth = 0
        while true {
            if isLeaf(cell) {
                let occupant = Int(body[cell])
                if occupant < 0 {
                    body[cell] = Int32(point)
                    return
                }
                let same = x[occupant] == x[point] && y[occupant] == y[point]
                if same || depth >= Self.maxDepth {
                    next[point] = body[cell]
                    body[cell] = Int32(point)
                    return
                }
                // Split the leaf: its points (all on one spot) move down to a child.
                body[cell] = -1
                let moved = child(quadrant(of: occupant, in: cell), of: cell)
                body[moved] = Int32(occupant)
            }
            cell = child(quadrant(of: point, in: cell), of: cell)
            depth += 1
        }
    }

    /// Counts the points under each cell and their centre of mass. Children are always made after
    /// their parent, so one pass from the last cell back is enough.
    private mutating func accumulate() {
        let cells = cellSize.count
        mass = Array(repeating: 0, count: cells)
        massX = Array(repeating: 0, count: cells)
        massY = Array(repeating: 0, count: cells)
        for cell in stride(from: cells - 1, through: 0, by: -1) {
            var m = 0.0, sx = 0.0, sy = 0.0
            var point = body[cell]
            while point >= 0 {
                m += 1
                sx += x[Int(point)]
                sy += y[Int(point)]
                point = next[Int(point)]
            }
            for quadrant in 0..<4 {
                let c = Int(children[cell * 4 + quadrant])
                guard c >= 0, mass[c] > 0 else { continue }
                m += mass[c]
                sx += massX[c] * mass[c]
                sy += massY[c] * mass[c]
            }
            mass[cell] = m
            if m > 0 {
                massX[cell] = sx / m
                massY[cell] = sy / m
            }
        }
    }

    /// The repulsion on point `index` from every other point, before it is scaled by the charge:
    /// the sum of `(other - point) * mass / distance²`.
    func force(on index: Int, x px: Double, y py: Double, theta2: Double, maxDistance2: Double) -> (Double, Double) {
        var stack: [Int32] = []
        return force(on: index, x: px, y: py, theta2: theta2, maxDistance2: maxDistance2, stack: &stack)
    }

    /// The same, with a stack the caller keeps between calls, so a step allocates it once.
    func force(on index: Int, x px: Double, y py: Double, theta2: Double, maxDistance2: Double, stack: inout [Int32]) -> (Double, Double) {
        guard !cellSize.isEmpty else { return (0, 0) }
        var fx = 0.0, fy = 0.0
        stack.removeAll(keepingCapacity: true)
        stack.append(0)
        while let top = stack.popLast() {
            let cell = Int(top)
            let m = mass[cell]
            guard m > 0 else { continue }
            var dx = massX[cell] - px
            var dy = massY[cell] - py
            var d2 = dx * dx + dy * dy
            let size = cellSize[cell]
            let leaf = isLeaf(cell)
            if !leaf, size * size / theta2 < d2 {
                // Far enough: the whole cell acts as one body.
                if d2 < maxDistance2 {
                    fx += dx * m / d2
                    fy += dy * m / d2
                }
                continue
            }
            if leaf {
                var point = body[cell]
                while point >= 0 {
                    let j = Int(point)
                    point = next[j]
                    guard j != index else { continue }
                    dx = x[j] - px
                    dy = y[j] - py
                    if dx == 0 { dx = jiggle(index &+ j) }
                    if dy == 0 { dy = jiggle(index &* 31 &+ j) }
                    d2 = dx * dx + dy * dy
                    guard d2 < maxDistance2 else { continue }
                    // Very close points push as if they were 1 point apart.
                    if d2 < 1 { d2 = d2.squareRoot() }
                    fx += dx / d2
                    fy += dy / d2
                }
                continue
            }
            for quadrant in 0..<4 {
                let c = children[cell * 4 + quadrant]
                if c >= 0 { stack.append(c) }
            }
        }
        return (fx, fy)
    }
}
