import SwiftUI

/// One choice of a `KSegmented` or `KTabs`.
struct KOption<Value: Hashable>: Identifiable {
    var value: Value
    var title: String
    var icon: KGlyph?
    var id: Value { value }

    init(_ value: Value, _ title: String, icon: KGlyph? = nil) {
        self.value = value
        self.title = title
        self.icon = icon
    }
}

/// A segmented control: the chosen segment raised on a quiet track.
struct KSegmented<Value: Hashable>: View {
    @Binding var selection: Value
    var options: [KOption<Value>]
    var showsTitles = true
    @Environment(\.kTheme) private var theme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Namespace private var thumb

    var body: some View {
        HStack(spacing: 2) {
            ForEach(options) { option in
                let selected = option.value == selection
                Button {
                    withAnimation(KMotion.animation(reduceMotion: reduceMotion)) { selection = option.value }
                } label: {
                    HStack(spacing: 6) {
                        if let icon = option.icon {
                            KIcon(icon, size: .xs, color: selected ? (theme.isDark ? theme.indigo : theme.fg) : nil)
                        }
                        if showsTitles || option.icon == nil { Text(option.title) }
                    }
                    .font(KFont.ui(12, .medium))
                    .foregroundStyle(selected ? theme.fg : theme.muted)
                    .padding(.horizontal, 10)
                    .frame(height: 24)
                    .background {
                        if selected {
                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                .fill(theme.isDark ? theme.fg.opacity(0.08) : theme.panel)
                                .overlay(RoundedRectangle(cornerRadius: 7, style: .continuous).strokeBorder(theme.line))
                                .matchedGeometryEffect(id: "thumb", in: thumb)
                        }
                    }
                }
                .buttonStyle(KPlainButtonStyle())
                .accessibilityLabel(option.title)
                .accessibilityAddTraits(selected ? .isSelected : [])
                .kTooltip(showsTitles ? nil : option.title)
            }
        }
        .padding(3)
        .background(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous).fill(theme.isDark ? theme.bg : theme.fg.opacity(0.04)))
        .overlay(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous).strokeBorder(theme.line))
    }
}

/// Underlined tabs with Knode Line titles.
struct KTabs<Value: Hashable>: View {
    @Binding var selection: Value
    var options: [KOption<Value>]
    @Environment(\.kTheme) private var theme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Namespace private var underline

    var body: some View {
        HStack(spacing: KSpace.x5) {
            ForEach(options) { option in
                let selected = option.value == selection
                Button {
                    withAnimation(KMotion.animation(reduceMotion: reduceMotion)) { selection = option.value }
                } label: {
                    VStack(spacing: 7) {
                        HStack(spacing: 6) {
                            if let icon = option.icon { KIcon(icon, size: .xs) }
                            Text(option.title).kLineStyle(11)
                        }
                        .foregroundStyle(selected ? theme.fg : theme.muted)
                        ZStack {
                            Rectangle().fill(.clear).frame(height: 2)
                            if selected {
                                Capsule().fill(theme.indigo).frame(height: 2)
                                    .matchedGeometryEffect(id: "underline", in: underline)
                            }
                        }
                    }
                    .fixedSize()
                }
                .buttonStyle(KPlainButtonStyle())
                .accessibilityAddTraits(selected ? .isSelected : [])
            }
        }
    }
}

/// A one-line text field in a rounded box that turns indigo when focused.
struct KTextField: View {
    var placeholder: String
    @Binding var text: String
    var icon: KGlyph?
    var mono = false
    var size: KControlSize = .regular
    var onSubmit: () -> Void = {}
    @Environment(\.kTheme) private var theme
    @FocusState private var focused: Bool

    init(_ placeholder: String, text: Binding<String>, icon: KGlyph? = nil, mono: Bool = false,
         size: KControlSize = .regular, onSubmit: @escaping () -> Void = {}) {
        self.placeholder = placeholder
        self._text = text
        self.icon = icon
        self.mono = mono
        self.size = size
        self.onSubmit = onSubmit
    }

    var body: some View {
        KFieldBox(focused: focused, size: size) {
            if let icon { KIcon(icon, size: .sm, color: theme.muted) }
            TextField("", text: $text, prompt: Text(placeholder).foregroundStyle(theme.muted))
                .textFieldStyle(.plain)
                .font(mono ? KFont.mono(size.fontSize) : KFont.ui(size.fontSize))
                .foregroundStyle(theme.fg)
                .focused($focused)
                .onSubmit(onSubmit)
                .accessibilityLabel(placeholder)
        }
        .onTapGesture { focused = true }
    }
}

/// A search field: the search icon, a clear button once there is text, and a key hint before.
struct KSearchField: View {
    var placeholder = "Search"
    @Binding var text: String
    var hint: [KKey] = []
    var size: KControlSize = .regular
    var focus: FocusState<Bool>.Binding?
    var onSubmit: () -> Void = {}
    @Environment(\.kTheme) private var theme
    @FocusState private var ownFocus: Bool

    var body: some View {
        let isFocused = focus?.wrappedValue ?? ownFocus
        KFieldBox(focused: isFocused, size: size) {
            KIcon(.search, size: .sm, color: isFocused ? (theme.isDark ? theme.indigo : theme.fg) : theme.muted)
            field
            if !text.isEmpty {
                Button { text = "" } label: { KIcon(.close, size: .xs) }
                    .buttonStyle(KIconButtonStyle(size: .small))
                    .frame(width: 18, height: 18)
                    .accessibilityLabel("Clear")
            } else if !hint.isEmpty, !isFocused {
                KKeyHint(hint)
            }
        }
    }

    @ViewBuilder private var field: some View {
        let base = TextField("", text: $text, prompt: Text(placeholder).foregroundStyle(theme.muted))
            .textFieldStyle(.plain)
            .font(KFont.ui(size.fontSize))
            .foregroundStyle(theme.fg)
            .onSubmit(onSubmit)
            .accessibilityLabel(placeholder)
        if let focus { base.focused(focus) } else { base.focused($ownFocus) }
    }
}

/// The rounded box of text fields.
struct KFieldBox<Content: View>: View {
    var focused: Bool
    var size: KControlSize = .regular
    @ViewBuilder var content: Content
    @Environment(\.kTheme) private var theme
    @State private var hovering = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: KRadius.row, style: .continuous)
        HStack(spacing: KSpace.x2) { content }
            .padding(.horizontal, 10)
            .frame(height: size.height)
            .background(shape.fill(theme.field))
            .overlay(shape.strokeBorder(focused ? theme.indigo : hovering ? theme.muted.opacity(0.6) : theme.line,
                                        lineWidth: focused ? 1.5 : 1))
            .contentShape(shape)
            .onHover { hovering = $0 }
    }
}

/// A switch: the track fills with the accent when on.
struct KToggleStyle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View { KToggleBody(configuration: configuration) }
}

private struct KToggleBody: View {
    let configuration: ToggleStyleConfiguration
    @Environment(\.kTheme) private var theme
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let on = configuration.isOn
        Button {
            withAnimation(KMotion.animation(KMotion.fast, reduceMotion: reduceMotion)) { configuration.isOn.toggle() }
        } label: {
            HStack(spacing: KSpace.x3) {
                configuration.label.foregroundStyle(theme.fg)
                Spacer(minLength: 0)
                Capsule()
                    .fill(on ? (theme.isDark ? theme.indigo : theme.accent) : theme.line)
                    .frame(width: 32, height: 18)
                    .overlay(alignment: on ? .trailing : .leading) {
                        Circle().fill(on ? theme.onAccent : (theme.isDark ? theme.muted : theme.panel))
                            .frame(width: 14, height: 14)
                            .padding(2)
                    }
            }
        }
        .buttonStyle(KPlainButtonStyle())
        .opacity(isEnabled ? 1 : 0.4)
        .accessibilityRepresentation { Toggle(isOn: configuration.$isOn) { configuration.label } }
    }
}

/// A checkbox: a rounded square that fills with the accent and shows the done mark.
struct KCheckboxStyle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View { KCheckboxBody(configuration: configuration) }
}

private struct KCheckboxBody: View {
    let configuration: ToggleStyleConfiguration
    @Environment(\.kTheme) private var theme
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    var body: some View {
        Button { configuration.isOn.toggle() } label: {
            HStack(spacing: KSpace.x2) {
                KCheckmark(isOn: configuration.isOn, hovering: hovering)
                configuration.label.foregroundStyle(theme.fg)
            }
        }
        .buttonStyle(KPlainButtonStyle())
        .onHover { hovering = $0 }
        .opacity(isEnabled ? 1 : 0.4)
        .accessibilityRepresentation { Toggle(isOn: configuration.$isOn) { configuration.label } }
    }
}

/// The box of a checkbox, also used for the task items of the preview.
struct KCheckmark: View {
    var isOn: Bool
    var hovering = false
    var side: CGFloat = 16
    @Environment(\.kTheme) private var theme

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: side * 0.3, style: .continuous)
        let fill = theme.isDark ? theme.indigo : theme.accent
        shape.fill(isOn ? fill : theme.field)
            .overlay(shape.strokeBorder(isOn ? fill : hovering ? theme.muted : theme.line, lineWidth: 1.5))
            .overlay { if isOn { KIcon(.done, points: side * 0.62, color: theme.onAccent) } }
            .frame(width: side, height: side)
    }
}

/// A switch with a label.
struct KToggle: View {
    var title: String
    @Binding var isOn: Bool

    init(_ title: String, isOn: Binding<Bool>) {
        self.title = title
        self._isOn = isOn
    }

    var body: some View { Toggle(title, isOn: $isOn).toggleStyle(KToggleStyle()) }
}

/// A checkbox with a label.
struct KCheckbox: View {
    var title: String
    @Binding var isOn: Bool

    init(_ title: String, isOn: Binding<Bool>) {
        self.title = title
        self._isOn = isOn
    }

    var body: some View { Toggle(title, isOn: $isOn).toggleStyle(KCheckboxStyle()) }
}

/// A key of a keyboard shortcut hint.
enum KKey: Hashable, Sendable {
    case command, shift, option, control, `return`, escape
    case char(String)

    var glyph: KGlyph? {
        switch self {
        case .command: .keyCommand
        case .shift: .keyShift
        case .option: .keyOption
        case .control: .up
        case .return: .keyReturn
        case .escape, .char: nil
        }
    }

    var title: String {
        switch self {
        case .command: "Command"
        case .shift: "Shift"
        case .option: "Option"
        case .control: "Control"
        case .return: "Return"
        case .escape: "Esc"
        case .char(let c): c.uppercased()
        }
    }
}

/// A keyboard shortcut, key by key, in small key caps.
struct KKeyHint: View {
    var keys: [KKey]
    @Environment(\.kTheme) private var theme

    init(_ keys: [KKey]) { self.keys = keys }

    var body: some View {
        HStack(spacing: 2) {
            ForEach(Array(keys.enumerated()), id: \.offset) { _, key in
                Group {
                    if let glyph = key.glyph {
                        KIcon(glyph, points: 7)
                    } else {
                        Text(key.title).kLineStyle(9.5)
                    }
                }
                .foregroundStyle(theme.muted)
                .padding(.horizontal, 4)
                .frame(minWidth: 16, minHeight: 16)
                .overlay(RoundedRectangle(cornerRadius: 4, style: .continuous).strokeBorder(theme.line))
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(keys.map(\.title).joined(separator: " "))
    }
}
