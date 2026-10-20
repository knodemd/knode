import SwiftUI

/// A row of a list. Selected: an indigo marker at the leading edge over a soft highlight.
struct KRow<Content: View>: View {
    var isSelected = false
    var action: (() -> Void)?
    @ViewBuilder var content: Content
    @Environment(\.kTheme) private var theme
    @State private var hovering = false

    init(isSelected: Bool = false, action: (() -> Void)? = nil, @ViewBuilder content: () -> Content) {
        self.isSelected = isSelected
        self.action = action
        self.content = content()
    }

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: KRadius.row, style: .continuous)
        let row = HStack(spacing: KSpace.x2) { content }
            .padding(.horizontal, KSpace.x3)
            .padding(.vertical, KSpace.x2)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(shape.fill(isSelected ? theme.highlight : hovering ? theme.hoverFill : .clear))
            .overlay(alignment: .leading) {
                if isSelected {
                    Capsule().fill(theme.indigo).frame(width: 3).padding(.vertical, 8)
                }
            }
            .contentShape(shape)
            .onHover { hovering = $0 }
            .accessibilityAddTraits(isSelected ? .isSelected : [])
        if let action {
            Button(action: action) { row }.buttonStyle(KPlainButtonStyle())
        } else {
            row
        }
    }
}

/// A small uppercase label in Knode Line above a group.
struct KSectionLabel<Accessory: View>: View {
    var title: String
    @ViewBuilder var accessory: Accessory
    @Environment(\.kTheme) private var theme

    init(_ title: String, @ViewBuilder accessory: () -> Accessory = { EmptyView() }) {
        self.title = title
        self.accessory = accessory()
    }

    var body: some View {
        HStack(spacing: KSpace.x2) {
            Text(title).kLineStyle(11).foregroundStyle(theme.muted)
            Spacer(minLength: 0)
            accessory
        }
        .accessibilityAddTraits(.isHeader)
    }
}

/// A card: panel colour, a hairline border, 18 pt corners.
struct KCard<Content: View>: View {
    var highlighted = false
    var padding: CGFloat = KSpace.x5
    @ViewBuilder var content: Content
    @Environment(\.kTheme) private var theme

    init(highlighted: Bool = false, padding: CGFloat = KSpace.x5, @ViewBuilder content: () -> Content) {
        self.highlighted = highlighted
        self.padding = padding
        self.content = content()
    }

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: KRadius.card, style: .continuous)
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(shape.fill(theme.panel))
            .overlay(shape.strokeBorder(highlighted ? theme.indigo : theme.line, lineWidth: 1))
    }
}

enum KChipKind: Sendable { case neutral, accent, solid }

/// A chip or badge: Knode Line in a capsule.
struct KChip: View {
    var title: String
    var icon: KGlyph?
    var kind: KChipKind = .neutral
    @Environment(\.kTheme) private var theme

    init(_ title: String, icon: KGlyph? = nil, kind: KChipKind = .neutral) {
        self.title = title
        self.icon = icon
        self.kind = kind
    }

    var body: some View {
        HStack(spacing: 5) {
            if let icon { KIcon(icon, points: 8.75) }
            Text(title).kLineStyle(10)
        }
        .foregroundStyle(foreground)
        .padding(.horizontal, 8)
        .frame(height: 20)
        .background(Capsule().fill(kind == .solid ? (theme.isDark ? theme.indigo : theme.accent) : .clear))
        .overlay(Capsule().strokeBorder(kind == .accent ? theme.indigo : kind == .solid ? .clear : theme.line))
    }

    private var foreground: Color {
        switch kind {
        case .neutral: theme.muted
        case .accent: theme.isDark ? theme.indigo : theme.fg
        case .solid: theme.onAccent
        }
    }
}

/// A thin progress bar; indeterminate when `value` is nil.
struct KProgress: View {
    var value: Double?
    @Environment(\.kTheme) private var theme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase = false

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(theme.line)
                if let value {
                    Capsule().fill(theme.indigo).frame(width: geo.size.width * min(max(value, 0), 1))
                } else {
                    Capsule().fill(theme.indigo)
                        .frame(width: geo.size.width * 0.3)
                        .offset(x: phase ? geo.size.width * 0.7 : 0)
                }
            }
        }
        .frame(height: 4)
        .clipShape(Capsule())
        .onAppear {
            guard value == nil, !reduceMotion else { return }
            withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true)) { phase = true }
        }
        .accessibilityElement()
        .accessibilityLabel("Progress")
        .accessibilityValue(value.map { "\(Int($0 * 100)) percent" } ?? "In progress")
    }
}

/// A small spinner: an indigo arc turning on the line colour.
struct KSpinner: View {
    var size: CGFloat = 14
    @Environment(\.kTheme) private var theme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var turning = false

    var body: some View {
        ZStack {
            Circle().stroke(theme.line, lineWidth: 2)
            Circle().trim(from: 0, to: 0.28).stroke(theme.indigo, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                .rotationEffect(.degrees(turning ? 360 : 0))
        }
        .frame(width: size, height: size)
        .onAppear {
            guard !reduceMotion else { return }
            withAnimation(.linear(duration: 0.8).repeatForever(autoreverses: false)) { turning = true }
        }
        .accessibilityLabel("Loading")
    }
}

/// A scroll view with a thin indicator of our own that shows while scrolling or under the pointer.
struct KScrollView<Content: View>: View {
    var axes: Axis.Set = .vertical
    @ViewBuilder var content: Content
    @Environment(\.kTheme) private var theme
    @State private var geometry = ScrollMetrics()
    @State private var hovering = false
    @State private var scrolling = false

    init(_ axes: Axis.Set = .vertical, @ViewBuilder content: () -> Content) {
        self.axes = axes
        self.content = content()
    }

    struct ScrollMetrics: Equatable {
        var offset: CGFloat = 0
        var content: CGFloat = 0
        var container: CGFloat = 0
    }

    var body: some View {
        ScrollView(axes) { content }
            .scrollIndicators(.never)
            .onScrollGeometryChange(for: ScrollMetrics.self) { geo in
                axes.contains(.vertical)
                    ? ScrollMetrics(offset: geo.contentOffset.y + geo.contentInsets.top, content: geo.contentSize.height, container: geo.containerSize.height)
                    : ScrollMetrics(offset: geo.contentOffset.x + geo.contentInsets.leading, content: geo.contentSize.width, container: geo.containerSize.width)
            } action: { _, new in
                geometry = new
            }
            .onScrollPhaseChange { _, phase in
                withAnimation(.easeOut(duration: KMotion.standard)) { scrolling = phase.isScrolling }
            }
            .overlay(alignment: axes.contains(.vertical) ? .topTrailing : .bottomLeading) { indicator }
            .onHover { hovering = $0 }
    }

    @ViewBuilder private var indicator: some View {
        let m = geometry
        if m.content > m.container + 1, m.container > 0 {
            let ratio = m.container / m.content
            let length = max(m.container * ratio, 28)
            let progress = min(max(m.offset / (m.content - m.container), 0), 1)
            let position = (m.container - length) * progress
            Capsule()
                .fill(theme.muted.opacity(0.5))
                .frame(width: axes.contains(.vertical) ? 3 : length, height: axes.contains(.vertical) ? length : 3)
                .offset(x: axes.contains(.vertical) ? -3 : position, y: axes.contains(.vertical) ? position : -3)
                .opacity(scrolling || hovering ? 1 : 0)
                .allowsHitTesting(false)
        }
    }
}

/// An empty state: an icon, a title in Knode Block, a line of text and actions.
struct KEmptyState<Actions: View>: View {
    var icon: KGlyph
    var title: String
    var message: String?
    @ViewBuilder var actions: Actions
    @Environment(\.kTheme) private var theme

    init(icon: KGlyph, title: String, message: String? = nil, @ViewBuilder actions: () -> Actions = { EmptyView() }) {
        self.icon = icon
        self.title = title
        self.message = message
        self.actions = actions()
    }

    var body: some View {
        VStack(spacing: KSpace.x4) {
            KIcon(icon, size: .xl, color: theme.isDark ? theme.indigo : theme.fg)
            Text(title).kBlockStyle(20).multilineTextAlignment(.center).foregroundStyle(theme.fg)
            if let message {
                Text(message).font(KFont.ui(13)).foregroundStyle(theme.muted).multilineTextAlignment(.center)
                    .frame(maxWidth: 340)
            }
            HStack(spacing: KSpace.x2) { actions }.padding(.top, KSpace.x1)
        }
        .padding(KSpace.x8)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

enum KBannerKind: Sendable { case info, warning, error }

/// A banner across the top of a view: something to know, with actions.
struct KBanner<Actions: View>: View {
    var kind: KBannerKind = .info
    var title: String
    var message: String?
    var onClose: (() -> Void)?
    /// The actions go under the text instead of beside it, for narrow places such as the sidebar.
    var actionsBelow = false
    @ViewBuilder var actions: Actions
    @Environment(\.kTheme) private var theme

    init(_ kind: KBannerKind = .info, title: String, message: String? = nil, onClose: (() -> Void)? = nil,
         actionsBelow: Bool = false, @ViewBuilder actions: () -> Actions = { EmptyView() }) {
        self.kind = kind
        self.title = title
        self.message = message
        self.onClose = onClose
        self.actionsBelow = actionsBelow
        self.actions = actions()
    }

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: KRadius.control, style: .continuous)
        HStack(alignment: actionsBelow ? .top : .center, spacing: KSpace.x3) {
            KIcon(kind == .info ? .info : .warning, size: .sm, color: tint)
            VStack(alignment: .leading, spacing: actionsBelow ? KSpace.x2 : 2) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(KFont.ui(13, .semibold)).foregroundStyle(theme.fg)
                        .fixedSize(horizontal: false, vertical: true)
                    if let message { Text(message).font(KFont.ui(12)).foregroundStyle(theme.muted) }
                }
                if actionsBelow { actions }
            }
            Spacer(minLength: KSpace.x2)
            if !actionsBelow { actions }
            if let onClose { KIconButton(.close, title: "Close", size: .small, action: onClose) }
        }
        .padding(.horizontal, KSpace.x4)
        .padding(.vertical, KSpace.x3)
        .background(shape.fill(kind == .error ? theme.dangerFill : theme.panel))
        .overlay(shape.strokeBorder(kind == .error ? theme.danger.opacity(0.5) : kind == .warning ? theme.indigo : theme.line))
    }

    private var tint: Color {
        switch kind {
        case .info: theme.muted
        case .warning: theme.isDark ? theme.indigo : theme.fg
        case .error: theme.danger
        }
    }
}

/// A short message that shows at the bottom of the window for a few seconds.
struct KToastItem: Identifiable, Equatable {
    let id = UUID()
    var message: String
    var icon: KGlyph?
}

/// The toasts of a window. Put `.kToasts(center)` on the window's root view.
@MainActor @Observable
final class KToastCenter {
    private(set) var toasts: [KToastItem] = []

    func show(_ message: String, icon: KGlyph? = .done, seconds: Double = 3) {
        let item = KToastItem(message: message, icon: icon)
        toasts.append(item)
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(seconds))
            self?.toasts.removeAll { $0.id == item.id }
        }
    }

    func dismiss(_ id: KToastItem.ID) { toasts.removeAll { $0.id == id } }
}

private struct KToastOverlay: ViewModifier {
    var center: KToastCenter
    @Environment(\.kTheme) private var theme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content.overlay(alignment: .bottom) {
            VStack(spacing: KSpace.x2) {
                ForEach(center.toasts) { toast in
                    HStack(spacing: KSpace.x2) {
                        if let icon = toast.icon { KIcon(icon, size: .sm, color: theme.isDark ? theme.indigo : theme.fg) }
                        Text(toast.message).font(KFont.ui(13, .medium)).foregroundStyle(theme.fg)
                    }
                    .padding(.horizontal, KSpace.x4)
                    .frame(height: 38)
                    .background(Capsule().fill(theme.panel))
                    .overlay(Capsule().strokeBorder(theme.line))
                    .shadow(color: .black.opacity(theme.isDark ? 0.4 : 0.08), radius: 16, y: 6)
                    .onTapGesture { center.dismiss(toast.id) }
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
                    .accessibilityAddTraits(.isStaticText)
                }
            }
            .padding(.bottom, KSpace.x6)
            .animation(KMotion.animation(reduceMotion: reduceMotion), value: center.toasts)
        }
    }
}

extension View {
    func kToasts(_ center: KToastCenter) -> some View { modifier(KToastOverlay(center: center)) }
}
