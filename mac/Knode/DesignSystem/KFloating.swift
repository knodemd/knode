import AppKit
import SwiftUI

/// Where a floating panel opens relative to the view it belongs to.
enum KPopoverEdge: Sendable {
    case below, above, trailing, leading
}

/// A borderless window above the main one, for our popovers, menus and tooltips: the system
/// popover and menu chrome never shows.
final class KFloatingPanel: NSPanel {
    var acceptsKey = true

    init() {
        super.init(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: true)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        level = .popUpMenu
        isReleasedWhenClosed = false
        hidesOnDeactivate = true
        animationBehavior = .none
        collectionBehavior = [.transient, .ignoresCycle, .fullScreenAuxiliary]
    }

    override var canBecomeKey: Bool { acceptsKey }
    override var canBecomeMain: Bool { false }
}

/// Shows SwiftUI content in a `KFloatingPanel` next to an anchor view and closes it on a click
/// outside, Escape, or when the app goes to the background.
@MainActor
final class KFloatingPresenter {
    /// Room around the content for the soft shadow.
    static let margin: CGFloat = 16

    weak var anchor: NSView?
    var onDismiss: (() -> Void)?
    private var panel: KFloatingPanel?
    private var host: NSHostingView<AnyView>?
    private var monitors: [Any] = []

    var isShowing: Bool { panel != nil }

    /// Opens the panel at `edge` of the anchor, or with its top left corner at `point` (in screen
    /// coordinates) when one is given.
    func present(_ content: AnyView, edge: KPopoverEdge, at point: NSPoint? = nil, interactive: Bool = true) {
        guard let anchor, let window = anchor.window else { return }
        dismiss(notify: false)
        let panel = KFloatingPanel()
        panel.acceptsKey = interactive
        panel.ignoresMouseEvents = !interactive
        let host = NSHostingView(rootView: content)
        host.sizingOptions = [.intrinsicContentSize]
        panel.contentView = host
        let size = host.fittingSize
        let m = Self.margin
        let rect = window.convertToScreen(anchor.convert(anchor.bounds, to: nil))
        var origin: NSPoint
        if let point {
            origin = NSPoint(x: point.x - m, y: point.y - size.height + m)
        } else {
            switch edge {
            case .below: origin = NSPoint(x: rect.minX - m, y: rect.minY - 6 - size.height + m)
            case .above: origin = NSPoint(x: rect.minX - m, y: rect.maxY + 6 - m)
            case .trailing: origin = NSPoint(x: rect.maxX + 6 - m, y: rect.maxY - size.height + m)
            case .leading: origin = NSPoint(x: rect.minX - 6 - size.width + m, y: rect.maxY - size.height + m)
            }
        }
        if let screen = window.screen ?? NSScreen.main {
            let visible = screen.visibleFrame
            origin.x = min(max(origin.x, visible.minX), visible.maxX - size.width)
            if origin.y < visible.minY, edge == .below, point == nil {
                origin.y = rect.maxY + 6 - m
            }
            origin.y = min(max(origin.y, visible.minY), visible.maxY - size.height)
        }
        panel.setFrame(NSRect(origin: origin, size: size), display: true)
        window.addChildWindow(panel, ordered: .above)
        if interactive {
            panel.makeKeyAndOrderFront(nil)
            installMonitors(for: panel)
        } else {
            panel.orderFront(nil)
        }
        self.panel = panel
        self.host = host
    }

    /// Replaces the content of the open panel, keeping its place.
    func update(_ content: AnyView) {
        guard let host, let panel else { return }
        host.rootView = content
        let size = host.fittingSize
        var frame = panel.frame
        frame.origin.y += frame.height - size.height
        frame.size = size
        panel.setFrame(frame, display: true)
    }

    func dismiss(notify: Bool = true) {
        for monitor in monitors { NSEvent.removeMonitor(monitor) }
        monitors = []
        if let panel {
            panel.parent?.removeChildWindow(panel)
            panel.orderOut(nil)
        }
        let wasShowing = panel != nil
        panel = nil
        host = nil
        if notify, wasShowing { onDismiss?() }
    }

    private func installMonitors(for panel: KFloatingPanel) {
        if let local = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown, .keyDown], handler: { [weak self] event in
            guard let self else { return event }
            if event.type == .keyDown {
                guard event.keyCode == 53, event.window === panel else { return event }
                self.dismiss()
                return nil
            }
            if event.window !== panel { self.dismiss() }
            return event
        }) {
            monitors.append(local)
        }
        if let global = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown], handler: { [weak self] _ in
            MainActor.assumeIsolated { self?.dismiss() }
        }) {
            monitors.append(global)
        }
    }
}

/// The card floating panels draw: panel colour, hairline border, a very soft shadow.
struct KFloatingCard<Content: View>: View {
    var padding: CGFloat = KSpace.x2
    @ViewBuilder var content: Content
    @Environment(\.kTheme) private var theme

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: KRadius.control, style: .continuous)
        content
            .padding(padding)
            .background(shape.fill(theme.panel))
            .overlay(shape.strokeBorder(theme.line))
            .clipShape(shape)
            .shadow(color: .black.opacity(theme.isDark ? 0.35 : 0.08), radius: 12, y: 4)
            .padding(KFloatingPresenter.margin)
            .fixedSize()
    }
}

/// Keeps a presenter's anchor and opens, updates or closes its panel as `isPresented` changes.
private struct KFloatingHost<Panel: View>: NSViewRepresentable {
    @Binding var isPresented: Bool
    var edge: KPopoverEdge
    var interactive: Bool
    var point: NSPoint?
    var panel: Panel

    func makeCoordinator() -> KFloatingPresenter { KFloatingPresenter() }

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        context.coordinator.anchor = view
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        let presenter = context.coordinator
        presenter.anchor = view
        let binding = $isPresented
        presenter.onDismiss = { binding.wrappedValue = false }
        let content = AnyView(panel.environment(\.self, context.environment).knodeTheme())
        if isPresented {
            if presenter.isShowing {
                presenter.update(content)
            } else {
                let edge = edge, interactive = interactive, point = point
                DispatchQueue.main.async {
                    guard binding.wrappedValue, !presenter.isShowing else { return }
                    presenter.present(content, edge: edge, at: point, interactive: interactive)
                }
            }
        } else if presenter.isShowing {
            presenter.dismiss(notify: false)
        }
    }

    static func dismantleNSView(_ view: NSView, coordinator: KFloatingPresenter) {
        coordinator.dismiss(notify: false)
    }
}

extension View {
    /// Our popover: a card in a floating panel at `edge` of this view.
    func kPopover<Content: View>(isPresented: Binding<Bool>, edge: KPopoverEdge = .below,
                                 @ViewBuilder content: () -> Content) -> some View {
        background(KFloatingHost(isPresented: isPresented, edge: edge, interactive: true, point: nil,
                                 panel: KFloatingCard(padding: KSpace.x3) { content() }))
    }

    /// Our tooltip: the title in a small card below the view after the pointer rests on it.
    func kTooltip(_ title: String?) -> some View { modifier(KTooltipModifier(title: title)) }

    /// Our context menu: opens at the pointer on a secondary click (or Control-click).
    func kContextMenu(_ entries: @escaping () -> [KMenuEntry]) -> some View {
        modifier(KContextMenuModifier(entries: entries))
    }
}

private struct KTooltipModifier: ViewModifier {
    var title: String?
    @State private var showing = false
    @State private var pending: Task<Void, Never>?

    func body(content: Content) -> some View {
        if let title, !title.isEmpty {
            content
                .onHover { inside in
                    pending?.cancel()
                    if inside {
                        pending = Task {
                            try? await Task.sleep(for: .milliseconds(650))
                            if !Task.isCancelled { showing = true }
                        }
                    } else {
                        showing = false
                    }
                }
                .simultaneousGesture(TapGesture().onEnded { pending?.cancel(); showing = false })
                .background(KFloatingHost(isPresented: $showing, edge: .below, interactive: false, point: nil,
                                          panel: KTooltipCard(title: title)))
        } else {
            content
        }
    }
}

private struct KTooltipCard: View {
    var title: String
    @Environment(\.kTheme) private var theme

    var body: some View {
        Text(title)
            .font(KFont.ui(12, .medium))
            .foregroundStyle(theme.fg)
            .padding(.horizontal, 8)
            .frame(height: 24)
            .background(RoundedRectangle(cornerRadius: 7, style: .continuous).fill(theme.panel))
            .overlay(RoundedRectangle(cornerRadius: 7, style: .continuous).strokeBorder(theme.line))
            .shadow(color: .black.opacity(theme.isDark ? 0.3 : 0.06), radius: 8, y: 2)
            .padding(KFloatingPresenter.margin)
            .fixedSize()
    }
}

private struct KContextMenuModifier: ViewModifier {
    var entries: () -> [KMenuEntry]
    @State private var showing = false
    @State private var point: NSPoint?

    func body(content: Content) -> some View {
        content
            .background(KSecondaryClickReader { location in
                point = location
                showing = true
            })
            .background(KFloatingHost(isPresented: $showing, edge: .below, interactive: true, point: point,
                                      panel: KFloatingCard(padding: KSpace.x1) {
                                          KMenuList(entries: entries(), dismiss: { showing = false })
                                      }))
    }
}

/// Calls back with the screen location of a secondary click (or Control-click) inside its frame.
private struct KSecondaryClickReader: NSViewRepresentable {
    var onClick: (NSPoint) -> Void

    final class Coordinator {
        weak var view: NSView?
        var onClick: (NSPoint) -> Void = { _ in }
        var monitor: Any?
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        let coordinator = context.coordinator
        coordinator.view = view
        coordinator.monitor = NSEvent.addLocalMonitorForEvents(matching: [.rightMouseDown, .leftMouseDown]) { [weak coordinator] event in
            guard let coordinator, let view = coordinator.view, let window = view.window, event.window === window,
                  !view.isHiddenOrHasHiddenAncestor else { return event }
            let secondary = event.type == .rightMouseDown || event.modifierFlags.contains(.control)
            guard secondary else { return event }
            let local = view.convert(event.locationInWindow, from: nil)
            guard view.bounds.contains(local) else { return event }
            coordinator.onClick(window.convertPoint(toScreen: event.locationInWindow))
            return nil
        }
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        context.coordinator.view = view
        context.coordinator.onClick = onClick
    }

    static func dismantleNSView(_ view: NSView, coordinator: Coordinator) {
        if let monitor = coordinator.monitor { NSEvent.removeMonitor(monitor) }
        coordinator.monitor = nil
    }
}
