import SwiftUI

/// One line of a `KMenuList`: an action, a divider or a small section label.
enum KMenuEntry {
    case item(KMenuItem)
    case divider
    case section(String)

    static func action(_ title: String, icon: KGlyph? = nil, shortcut: [KKey] = [], isChecked: Bool = false,
                       isDestructive: Bool = false, isEnabled: Bool = true,
                       perform: @escaping @MainActor () -> Void) -> KMenuEntry {
        .item(KMenuItem(title: title, icon: icon, shortcut: shortcut, isChecked: isChecked,
                        isDestructive: isDestructive, isEnabled: isEnabled, perform: perform))
    }
}

struct KMenuItem {
    var title: String
    var icon: KGlyph?
    var shortcut: [KKey] = []
    var isChecked = false
    var isDestructive = false
    var isEnabled = true
    var perform: @MainActor () -> Void
}

/// The list inside our menus: rows that highlight under the pointer, Knode icons, key hints.
struct KMenuList: View {
    var entries: [KMenuEntry]
    var dismiss: () -> Void
    var minWidth: CGFloat = 200
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            ForEach(Array(entries.enumerated()), id: \.offset) { _, entry in
                switch entry {
                case .item(let item):
                    KMenuRow(item: item) {
                        dismiss()
                        item.perform()
                    }
                case .divider:
                    Rectangle().fill(theme.line).frame(height: 1).padding(.vertical, 4).padding(.horizontal, 6)
                case .section(let title):
                    Text(title).kLineStyle(10).foregroundStyle(theme.muted)
                        .padding(.horizontal, 10).padding(.top, 6).padding(.bottom, 2)
                }
            }
        }
        .frame(minWidth: minWidth, alignment: .leading)
    }
}

private struct KMenuRow: View {
    var item: KMenuItem
    var action: () -> Void
    @Environment(\.kTheme) private var theme
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: KSpace.x2) {
                Group {
                    if item.isChecked {
                        KIcon(.done, size: .xs, color: theme.isDark ? theme.indigo : theme.fg)
                    } else if let icon = item.icon {
                        KIcon(icon, size: .sm)
                    } else {
                        Color.clear
                    }
                }
                .frame(width: 14)
                Text(item.title).font(KFont.ui(13)).lineLimit(1)
                Spacer(minLength: KSpace.x4)
                if !item.shortcut.isEmpty { KKeyHint(item.shortcut) }
            }
            .foregroundStyle(item.isDestructive ? theme.danger : hovering ? theme.hover : theme.fg)
            .padding(.horizontal, 8)
            .frame(height: 28)
            .background(RoundedRectangle(cornerRadius: 7, style: .continuous).fill(hovering ? theme.highlight : .clear))
            .contentShape(Rectangle())
        }
        .buttonStyle(KPlainButtonStyle())
        .disabled(!item.isEnabled)
        .opacity(item.isEnabled ? 1 : 0.4)
        .onHover { hovering = $0 && item.isEnabled }
    }
}

/// A button that opens our menu below it.
struct KMenuButton<Label: View>: View {
    var entries: () -> [KMenuEntry]
    @ViewBuilder var label: Label
    var style: KButtonKind = .ghost
    @State private var open = false

    init(style: KButtonKind = .ghost, entries: @escaping () -> [KMenuEntry], @ViewBuilder label: () -> Label) {
        self.style = style
        self.entries = entries
        self.label = label()
    }

    var body: some View {
        Button { open.toggle() } label: { label }
            .buttonStyle(KButtonStyle(kind: style, size: .small))
            .kPopover(isPresented: $open) {
                KMenuList(entries: entries(), dismiss: { open = false })
                    .padding(-KSpace.x2)
            }
    }
}

/// A pop-up choice: the current value with a down mark, opening our menu of the options.
struct KDropdown<Value: Hashable>: View {
    @Binding var selection: Value
    var options: [KOption<Value>]
    var size: KControlSize = .regular
    @Environment(\.kTheme) private var theme
    @State private var open = false
    @State private var hovering = false

    var body: some View {
        let current = options.first { $0.value == selection }
        let shape = RoundedRectangle(cornerRadius: KRadius.row, style: .continuous)
        Button { open.toggle() } label: {
            HStack(spacing: KSpace.x2) {
                if let icon = current?.icon { KIcon(icon, size: .sm) }
                Text(current?.title ?? "").font(KFont.ui(size.fontSize)).lineLimit(1)
                Spacer(minLength: KSpace.x2)
                KIcon(.down, size: .xs, color: theme.muted)
            }
            .foregroundStyle(theme.fg)
            .padding(.horizontal, 10)
            .frame(height: size.height)
            .background(shape.fill(theme.field))
            .overlay(shape.strokeBorder(open ? theme.indigo : hovering ? theme.muted.opacity(0.6) : theme.line, lineWidth: open ? 1.5 : 1))
            .contentShape(shape)
        }
        .buttonStyle(KPlainButtonStyle())
        .onHover { hovering = $0 }
        .accessibilityValue(current?.title ?? "")
        .kPopover(isPresented: $open) {
            KMenuList(entries: options.map { option in
                .action(option.title, icon: option.icon, isChecked: option.value == selection) { selection = option.value }
            }, dismiss: { open = false })
            .padding(-KSpace.x2)
        }
    }
}

// MARK: Sheets and confirmations

/// Our sheet: a card over a dimmed window, closed with Escape.
private struct KSheetModifier<Sheet: View>: ViewModifier {
    @Binding var isPresented: Bool
    var dismissOnScrim: Bool
    var width: CGFloat?
    @ViewBuilder var sheet: () -> Sheet
    @Environment(\.kTheme) private var theme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content
            .overlay {
                ZStack {
                    if isPresented {
                        Rectangle()
                            .fill(Color.black.opacity(theme.isDark ? 0.55 : 0.28))
                            .ignoresSafeArea()
                            .onTapGesture { if dismissOnScrim { isPresented = false } }
                            .transition(.opacity)
                        KSheetCard(width: width) { sheet() }
                            .transition(reduceMotion ? .opacity : .opacity.combined(with: .scale(scale: 0.97)))
                            .background {
                                Button("") { isPresented = false }
                                    .keyboardShortcut(.cancelAction)
                                    .opacity(0)
                                    .accessibilityHidden(true)
                            }
                    }
                }
                .animation(KMotion.animation(reduceMotion: reduceMotion), value: isPresented)
            }
    }
}

private struct KSheetCard<Content: View>: View {
    var width: CGFloat?
    @ViewBuilder var content: Content
    @Environment(\.kTheme) private var theme

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: KRadius.card, style: .continuous)
        content
            .padding(KSpace.x6)
            .frame(width: width)
            .background(shape.fill(theme.panel))
            .overlay(shape.strokeBorder(theme.line))
            .shadow(color: .black.opacity(theme.isDark ? 0.45 : 0.12), radius: 30, y: 12)
            .accessibilityAddTraits(.isModal)
    }
}

/// The title of a sheet or confirmation: Geist semibold, with a Knode Line kicker above.
struct KSheetHeader: View {
    var kicker: String?
    var title: String
    var message: String?
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            if let kicker { Text(kicker).kLineStyle(11).foregroundStyle(theme.isDark ? theme.indigo : theme.muted) }
            Text(title).font(KFont.ui(17, .semibold)).foregroundStyle(theme.fg).fixedSize(horizontal: false, vertical: true)
            if let message {
                Text(message).font(KFont.ui(13)).foregroundStyle(theme.muted).fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A question with a cancel button and one action.
struct KConfirmation {
    var title: String
    var message: String?
    var confirmTitle: String
    var cancelTitle = "Cancel"
    var isDestructive = false
    var confirm: @MainActor () -> Void
}

private struct KConfirmCard: View {
    var confirmation: KConfirmation
    var close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x5) {
            KSheetHeader(title: confirmation.title, message: confirmation.message)
            HStack(spacing: KSpace.x2) {
                Spacer()
                KButton(confirmation.cancelTitle, kind: .secondary, action: close)
                Button {
                    close()
                    confirmation.confirm()
                } label: {
                    Text(confirmation.confirmTitle)
                }
                .buttonStyle(KButtonStyle(kind: confirmation.isDestructive ? .destructive : .primary))
                .keyboardShortcut(.defaultAction)
            }
        }
    }
}

extension View {
    /// Our sheet, centred over this view (put it on the window's root view).
    func kSheet<Sheet: View>(isPresented: Binding<Bool>, width: CGFloat? = 440, dismissOnScrim: Bool = false,
                             @ViewBuilder content: @escaping () -> Sheet) -> some View {
        modifier(KSheetModifier(isPresented: isPresented, dismissOnScrim: dismissOnScrim, width: width, sheet: content))
    }

    /// Our sheet for an optional item: shown while it is not nil.
    func kSheet<Item, Sheet: View>(item: Binding<Item?>, width: CGFloat? = 440,
                                   @ViewBuilder content: @escaping (Item) -> Sheet) -> some View {
        let shown = Binding(get: { item.wrappedValue != nil }, set: { if !$0 { item.wrappedValue = nil } })
        return kSheet(isPresented: shown, width: width) {
            if let value = item.wrappedValue { content(value) }
        }
    }

    /// Our confirmation, shown while `confirmation` is not nil.
    func kConfirm(_ confirmation: Binding<KConfirmation?>) -> some View {
        kSheet(item: confirmation, width: 400) { value in
            KConfirmCard(confirmation: value) { confirmation.wrappedValue = nil }
        }
    }
}
