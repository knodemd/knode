import Foundation
import Testing
@testable import Knode

/// The force-directed layout, its Barnes-Hut quadtree and the viewport math.
@Suite struct GraphLayoutTests {
    /// Points from a fixed generator, so every run sees the same.
    func points(_ count: Int, seed: UInt64 = 7) -> (x: [Double], y: [Double]) {
        var state = seed
        func next() -> Double {
            state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return Double(state >> 11) / Double(1 << 53) * 1000 - 500
        }
        var x: [Double] = [], y: [Double] = []
        for _ in 0..<count {
            x.append(next())
            y.append(next())
        }
        return (x, y)
    }

    @Test func theTreeHoldsEveryPointAtItsCentreOfMass() {
        let (x, y) = points(500)
        let tree = QuadTree(x: x, y: y)
        #expect(tree.mass[0] == 500)
        #expect(abs(tree.massX[0] - x.reduce(0, +) / 500) < 1e-9)
        #expect(abs(tree.massY[0] - y.reduce(0, +) / 500) < 1e-9)
        #expect(tree.cellCount > 500)
    }

    @Test func pointsOnTheSameSpotShareALeaf() {
        let tree = QuadTree(x: [1, 1, 1, 5], y: [2, 2, 2, 5])
        #expect(tree.mass[0] == 4)
        let (fx, fy) = tree.force(on: 3, x: 5, y: 5, theta2: 0, maxDistance2: .infinity)
        // Three points at (1, 2) pull point 3 towards them, before the charge flips the sign.
        #expect(fx < 0 && fy < 0)
    }

    @Test func barnesHutIsCloseToTheExactSum() {
        let (x, y) = points(400)
        let tree = QuadTree(x: x, y: y)
        var worst = 0.0
        for i in stride(from: 0, to: 400, by: 37) {
            var ex = 0.0, ey = 0.0
            for j in 0..<400 where j != i {
                let dx = x[j] - x[i], dy = y[j] - y[i]
                let d2 = dx * dx + dy * dy
                ex += dx / d2
                ey += dy / d2
            }
            let exact = tree.force(on: i, x: x[i], y: y[i], theta2: 0, maxDistance2: .infinity)
            #expect(abs(exact.0 - ex) < 1e-9 && abs(exact.1 - ey) < 1e-9)
            let (ax, ay) = tree.force(on: i, x: x[i], y: y[i], theta2: 0.81, maxDistance2: .infinity)
            let error = hypot(ax - ex, ay - ey) / max(hypot(ex, ey), 1e-9)
            worst = max(worst, error)
        }
        #expect(worst < 0.2)
    }

    @Test func linkedNodesComeTogetherAndOthersDrift() {
        var layout = GraphLayout(count: 4, links: [(0, 1)], positions: [
            0: CGPoint(x: -300, y: 0), 1: CGPoint(x: 300, y: 0),
            2: CGPoint(x: 0, y: 2), 3: CGPoint(x: 0, y: -2),
        ])
        layout.run(steps: 600)
        #expect(layout.isSettled)
        let linked = hypot(layout.x[0] - layout.x[1], layout.y[0] - layout.y[1])
        let loose = hypot(layout.x[2] - layout.x[3], layout.y[2] - layout.y[3])
        #expect(linked < 120)
        #expect(loose > 20)
    }

    @Test func aHeldNodeStaysWhereItWasPut() {
        var layout = GraphLayout(count: 3, links: [(0, 1), (1, 2)])
        layout.place(1, at: CGPoint(x: 400, y: 400), fixed: true)
        layout.reheat(to: 1)
        for _ in 0..<50 { layout.step() }
        #expect(layout.position(1) == CGPoint(x: 400, y: 400))
        layout.releaseAll()
        layout.step()
        #expect(layout.position(1) != CGPoint(x: 400, y: 400))
    }

    @Test func ignoresLinksToItselfAndOutOfRange() {
        let layout = GraphLayout(count: 2, links: [(0, 0), (0, 5), (0, 1)])
        #expect(layout.sources == [0])
        #expect(layout.degree == [1, 1])
    }

    @Test func theSpiralNeverPutsTwoNodesTogether() {
        let points = (0..<2000).map(GraphLayout.spiral)
        #expect(Set(points.map { "\($0.x),\($0.y)" }).count == 2000)
    }

    @Test func findsTheNodeAndTheLinkUnderAPoint() {
        let layout = GraphLayout(count: 2, links: [(0, 1)], positions: [0: .zero, 1: CGPoint(x: 100, y: 0)])
        #expect(layout.node(near: CGPoint(x: 3, y: 4), radius: 6) == 0)
        #expect(layout.node(near: CGPoint(x: 50, y: 30), radius: 6) == nil)
        #expect(layout.link(near: CGPoint(x: 50, y: 2), radius: 5) == 0)
        #expect(GraphLayout.distance2(from: CGPoint(x: -3, y: 4), toSegment: .zero, CGPoint(x: 10, y: 0)) == 25)
    }

    @Test func viewportMapsBetweenScreenAndPlane() {
        let size = CGSize(width: 800, height: 600)
        let viewport = Viewport(x: 100, y: -50, zoom: 2)
        let point = CGPoint(x: 130, y: -20)
        let screen = viewport.toScreen(point, in: size)
        #expect(screen == CGPoint(x: 460, y: 360))
        #expect(viewport.toPlane(screen, in: size) == point)
        // Zooming keeps the point under the pointer where it is.
        let anchor = CGPoint(x: 200, y: 150)
        let before = viewport.toPlane(anchor, in: size)
        let zoomed = viewport.zoomed(by: 1.5, at: anchor, in: size)
        let after = zoomed.toPlane(anchor, in: size)
        #expect(abs(before.x - after.x) < 1e-9 && abs(before.y - after.y) < 1e-9)
        #expect(zoomed.zoom == 3)
        #expect(viewport.zoomed(by: 100, at: anchor, in: size).zoom == Viewport.maxZoom)
        let panned = viewport.panned(by: CGSize(width: 20, height: -10))
        #expect(panned.x == 90 && panned.y == -45)
        let fit = Viewport.fitting(CGRect(x: 0, y: 0, width: 360, height: 100), in: size, margin: 20)
        #expect(fit.x == 180 && fit.y == 50 && fit.zoom == 2)
    }

    /// The cost of one step for a space of 10,000 notes. Reported, not checked: tests run in a
    /// debug build.
    @Test func timesAStepOfTenThousandNodes() {
        let count = 10_000
        let links = (1..<count).map { ($0, $0 / 2) } + stride(from: 0, to: count, by: 3).map { ($0, ($0 * 7 + 13) % count) }
        var layout = GraphLayout(count: count, links: links)
        let clock = ContinuousClock()
        let start = clock.now
        for _ in 0..<5 { layout.step() }
        let perStep = (clock.now - start) / 5
        print("GraphLayout: one step of \(count) nodes and \(layout.sources.count) links takes \(perStep)")
        #expect(layout.count == count)
    }
}
