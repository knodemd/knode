import AppKit
import KnodeBrand
import SwiftUI

/// How a kind of node is drawn on the map. The brand has three inks and nothing else, so kinds
/// differ by shape and by ink: the foreground ink (ink on paper, paper on ink) or indigo, filled
/// or outlined.
struct NodeGlyph: Hashable {
    enum Shape: Hashable { case circle, square, diamond }

    var shape: Shape
    var indigo: Bool
    var filled: Bool

    static func of(kind: String) -> NodeGlyph {
        switch kind {
        case NodeKind.note: NodeGlyph(shape: .circle, indigo: false, filled: true)
        case NodeKind.conversation: NodeGlyph(shape: .circle, indigo: true, filled: true)
        case NodeKind.decision: NodeGlyph(shape: .diamond, indigo: true, filled: true)
        case NodeKind.idea: NodeGlyph(shape: .circle, indigo: false, filled: false)
        case NodeKind.source: NodeGlyph(shape: .square, indigo: false, filled: true)
        case NodeKind.canvas: NodeGlyph(shape: .square, indigo: true, filled: false)
        default: NodeGlyph(shape: .circle, indigo: true, filled: false)
        }
    }

    /// Adds the glyph, centred on a point, to a path.
    func add(to path: inout Path, at point: CGPoint, radius r: CGFloat) {
        switch shape {
        case .circle:
            path.addEllipse(in: CGRect(x: point.x - r, y: point.y - r, width: 2 * r, height: 2 * r))
        case .square:
            let side = r * 1.7
            path.addRoundedRect(in: CGRect(x: point.x - side / 2, y: point.y - side / 2, width: side, height: side), cornerSize: CGSize(width: side / 4, height: side / 4))
        case .diamond:
            let d = r * 1.3
            path.move(to: CGPoint(x: point.x, y: point.y - d))
            path.addLine(to: CGPoint(x: point.x + d, y: point.y))
            path.addLine(to: CGPoint(x: point.x, y: point.y + d))
            path.addLine(to: CGPoint(x: point.x - d, y: point.y))
            path.closeSubpath()
        }
    }
}

/// The brand inks for the map, in light and dark mode.
struct MapInks {
    var foreground: Color
    var background: Color
    var indigo: Color = KnodeColor.indigo

    init(_ scheme: ColorScheme) {
        foreground = scheme == .dark ? KnodeColor.paper : KnodeColor.ink
        background = scheme == .dark ? KnodeColor.ink : KnodeColor.paper
    }
}

/// A swatch of a node kind, for legends and menus.
struct NodeGlyphView: View {
    let kind: String
    var size: CGFloat = 10
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let glyph = NodeGlyph.of(kind: kind)
        let inks = MapInks(scheme)
        Canvas { context, canvasSize in
            var path = Path()
            glyph.add(to: &path, at: CGPoint(x: canvasSize.width / 2, y: canvasSize.height / 2), radius: canvasSize.width * 0.36)
            let ink = glyph.indigo ? inks.indigo : inks.foreground
            if glyph.filled {
                context.fill(path, with: .color(ink))
            } else {
                context.stroke(path, with: .color(ink), lineWidth: 1.4)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

extension View {
    /// Calls `action` with each scroll of the mouse wheel or the trackpad while the pointer is
    /// over the view; the event is used up when it returns true.
    func onScrollWheel(_ action: @escaping @MainActor (NSEvent) -> Bool) -> some View {
        modifier(ScrollWheelModifier(action: action))
    }
}

private struct ScrollWheelModifier: ViewModifier {
    let action: @MainActor (NSEvent) -> Bool
    @State private var isHovering = false
    @State private var monitor: Any?

    func body(content: Content) -> some View {
        content
            .onHover { isHovering = $0 }
            .onAppear {
                monitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { event in
                    // Local monitors run on the main thread.
                    nonisolated(unsafe) let event = event
                    let used = MainActor.assumeIsolated { isHovering && action(event) }
                    return used ? nil : event
                }
            }
            .onDisappear {
                if let monitor { NSEvent.removeMonitor(monitor) }
                monitor = nil
            }
    }
}
