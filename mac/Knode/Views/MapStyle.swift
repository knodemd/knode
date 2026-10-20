import AppKit
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

/// The colours of the map, from the Knode tokens of one appearance: the background of the
/// board, the panel colour of cards, the hairline colour of plain links and the inks of nodes.
struct MapInks {
    var foreground: Color
    var background: Color
    var panel: Color
    var line: Color
    var muted: Color
    var indigo: Color

    init(_ theme: KTheme) {
        foreground = theme.fg
        background = theme.bg
        panel = theme.panel
        line = theme.line
        muted = theme.muted
        indigo = theme.indigo
    }

    init(_ scheme: ColorScheme) { self.init(KTheme.resolved(for: scheme)) }

    /// Fills and outlines a glyph's path: filled kinds in their ink ringed with the background so
    /// they stay apart where they touch, outlined kinds as a ring of their ink on the panel colour.
    func draw(_ glyph: NodeGlyph, _ path: Path, in context: inout GraphicsContext, lineWidth: CGFloat = 1.4) {
        let ink = glyph.indigo ? indigo : foreground
        if glyph.filled {
            context.stroke(path, with: .color(background), lineWidth: lineWidth * 1.6)
            context.fill(path, with: .color(ink))
        } else {
            context.fill(path, with: .color(panel))
            context.stroke(path, with: .color(ink), lineWidth: lineWidth)
        }
    }
}

/// A swatch of a node kind, for legends and menus.
struct NodeGlyphView: View {
    let kind: String
    var size: CGFloat = 10
    @Environment(\.kTheme) private var theme

    var body: some View {
        let glyph = NodeGlyph.of(kind: kind)
        let inks = MapInks(theme)
        Canvas { context, canvasSize in
            var path = Path()
            glyph.add(to: &path, at: CGPoint(x: canvasSize.width / 2, y: canvasSize.height / 2), radius: canvasSize.width * 0.34)
            inks.draw(glyph, path, in: &context, lineWidth: 1.3)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// A floating bar or card over a map: panel colour, hairline border, rounded corners.
struct MapChrome: ViewModifier {
    var radius: CGFloat = KRadius.control
    @Environment(\.kTheme) private var theme

    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: radius, style: .continuous)
        content
            .background(shape.fill(theme.panel))
            .overlay(shape.strokeBorder(theme.line))
    }
}

extension View {
    func mapChrome(radius: CGFloat = KRadius.control) -> some View { modifier(MapChrome(radius: radius)) }
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
