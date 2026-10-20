import SwiftUI

enum KButtonKind: Sendable {
    /// Filled with the accent: the one main action of a view.
    case primary
    /// Outlined: every other action.
    case secondary
    /// No outline until the pointer is over it: actions in bars and lists.
    case ghost
    /// Deleting or discarding.
    case destructive
}

enum KControlSize: Sendable {
    case small, regular, large

    var height: CGFloat {
        switch self {
        case .small: 26
        case .regular: 32
        case .large: 40
        }
    }

    var fontSize: CGFloat {
        switch self {
        case .small: 12
        case .regular: 13
        case .large: 14
        }
    }

    var padding: CGFloat {
        switch self {
        case .small: 10
        case .regular: 14
        case .large: 20
        }
    }

    var radius: CGFloat { self == .large ? KRadius.control : KRadius.row }
}

/// The button of the design system, as a style so buttons keep their keyboard and accessibility
/// behaviour: `Button { } label: { }.buttonStyle(KButtonStyle(kind: .primary))`.
struct KButtonStyle: ButtonStyle {
    var kind: KButtonKind = .secondary
    var size: KControlSize = .regular

    func makeBody(configuration: Configuration) -> some View {
        KButtonBody(configuration: configuration, kind: kind, size: size)
    }
}

private struct KButtonBody: View {
    let configuration: ButtonStyleConfiguration
    let kind: KButtonKind
    let size: KControlSize
    @Environment(\.kTheme) private var theme
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovering = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: size.radius, style: .continuous)
        configuration.label
            .font(KFont.ui(size.fontSize, .semibold))
            .lineLimit(1)
            .padding(.horizontal, size.padding)
            .frame(minHeight: size.height)
            .foregroundStyle(foreground)
            .background(shape.fill(fill))
            .overlay(shape.strokeBorder(border, lineWidth: 1))
            .contentShape(shape)
            .brightness(kind == .primary && hovering && isEnabled ? (theme.isDark ? 0.06 : 0.12) : 0)
            .offset(y: configuration.isPressed ? 1 : 0)
            .opacity(isEnabled ? 1 : 0.4)
            .onHover { hovering = $0 }
            .animation(KMotion.animation(KMotion.fast, reduceMotion: reduceMotion), value: hovering)
    }

    private var active: Bool { hovering && isEnabled }

    private var foreground: Color {
        switch kind {
        case .primary: theme.onAccent
        case .secondary: theme.fg
        case .ghost: active ? theme.hover : theme.muted
        case .destructive: theme.danger
        }
    }

    private var fill: Color {
        switch kind {
        case .primary: theme.accent
        case .secondary: configuration.isPressed ? theme.hoverFill : .clear
        case .ghost: active || configuration.isPressed ? theme.hoverFill : .clear
        case .destructive: active ? theme.dangerFill : .clear
        }
    }

    private var border: Color {
        switch kind {
        case .primary: theme.accent
        case .secondary: active ? theme.muted : theme.line
        case .ghost: .clear
        case .destructive: theme.danger.opacity(active ? 0.8 : 0.45)
        }
    }
}

/// A button with an optional Knode icon before its title.
struct KButton: View {
    var title: String
    var icon: KGlyph?
    var kind: KButtonKind = .secondary
    var size: KControlSize = .regular
    var action: () -> Void

    init(_ title: String, icon: KGlyph? = nil, kind: KButtonKind = .secondary, size: KControlSize = .regular,
         action: @escaping () -> Void) {
        self.title = title
        self.icon = icon
        self.kind = kind
        self.size = size
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            HStack(spacing: size == .small ? 6 : 8) {
                if let icon { KIcon(icon, size: size == .large ? .md : .sm).accessibilityHidden(true) }
                Text(title)
            }
        }
        .buttonStyle(KButtonStyle(kind: kind, size: size))
    }
}

/// A square button with only an icon. Its title is the tooltip and the accessibility label.
struct KIconButton: View {
    var icon: KGlyph
    var title: String
    var isActive = false
    var size: KControlSize = .regular
    var action: () -> Void

    init(_ icon: KGlyph, title: String, isActive: Bool = false, size: KControlSize = .regular,
         action: @escaping () -> Void) {
        self.icon = icon
        self.title = title
        self.isActive = isActive
        self.size = size
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            KIcon(icon, size: size == .large ? .md : .sm)
        }
        .buttonStyle(KIconButtonStyle(isActive: isActive, size: size))
        .accessibilityLabel(title)
        .kTooltip(title)
    }
}

struct KIconButtonStyle: ButtonStyle {
    var isActive = false
    var size: KControlSize = .regular

    func makeBody(configuration: Configuration) -> some View {
        KIconButtonBody(configuration: configuration, isActive: isActive, size: size)
    }
}

private struct KIconButtonBody: View {
    let configuration: ButtonStyleConfiguration
    let isActive: Bool
    let size: KControlSize
    @Environment(\.kTheme) private var theme
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    var body: some View {
        let side = size.height - (size == .regular ? 4 : 0)
        let shape = RoundedRectangle(cornerRadius: KRadius.small, style: .continuous)
        configuration.label
            .foregroundStyle(isActive ? (theme.isDark ? theme.indigo : theme.fg) : (hovering && isEnabled ? theme.hover : theme.muted))
            .frame(width: side, height: side)
            .background(shape.fill(isActive ? theme.highlight : (hovering || configuration.isPressed) && isEnabled ? theme.hoverFill : .clear))
            .contentShape(shape)
            .opacity(isEnabled ? 1 : 0.35)
            .onHover { hovering = $0 }
    }
}

/// A button without chrome: a hit area over its label, dimmed while pressed. For rows, links and
/// custom controls.
struct KPlainButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .contentShape(Rectangle())
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// A text link: foreground, indigo under the pointer (ink with an indigo underline on paper).
struct KLinkButtonStyle: ButtonStyle {
    var muted = false

    func makeBody(configuration: Configuration) -> some View {
        KLinkBody(configuration: configuration, muted: muted)
    }
}

private struct KLinkBody: View {
    let configuration: ButtonStyleConfiguration
    let muted: Bool
    @Environment(\.kTheme) private var theme
    @State private var hovering = false

    var body: some View {
        configuration.label
            .foregroundStyle(hovering ? theme.hover : muted ? theme.muted : theme.fg)
            .underline(hovering && theme.hoverUnderline, color: theme.indigo)
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
    }
}
