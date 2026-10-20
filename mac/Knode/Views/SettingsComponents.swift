import SwiftUI

// The building blocks of the settings window, the menu bar panel and the sheets of this part of
// the app, made of the design system's components.

/// A settings pane: scrolls on the background, sections stacked with room between them.
struct KSettingsPane<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        KScrollView {
            VStack(alignment: .leading, spacing: KSpace.x6) { content }
                .padding(.horizontal, KSpace.x8)
                .padding(.vertical, KSpace.x6)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A group of settings: a Knode Line label above a card.
struct KSettingsSection<Content: View, Accessory: View>: View {
    var title: String
    var highlighted = false
    @ViewBuilder var accessory: Accessory
    @ViewBuilder var content: Content

    init(_ title: String, highlighted: Bool = false, @ViewBuilder accessory: () -> Accessory = { EmptyView() },
         @ViewBuilder content: () -> Content) {
        self.title = title
        self.highlighted = highlighted
        self.accessory = accessory()
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSectionLabel(title) { accessory }
                .padding(.horizontal, KSpace.x1)
            KCard(highlighted: highlighted, padding: KSpace.x4) {
                VStack(alignment: .leading, spacing: KSpace.x3) { content }
            }
        }
    }
}

/// One setting: a title and a line of explanation, with its control at the trailing edge.
struct KSettingRow<Trailing: View>: View {
    var title: String
    var detail: String?
    var icon: String?
    @ViewBuilder var trailing: Trailing
    @Environment(\.kTheme) private var theme

    init(_ title: String, detail: String? = nil, icon: String? = nil, @ViewBuilder trailing: () -> Trailing) {
        self.title = title
        self.detail = detail
        self.icon = icon
        self.trailing = trailing()
    }

    var body: some View {
        HStack(alignment: .center, spacing: KSpace.x3) {
            KSettingText(title: title, detail: detail, icon: icon)
            Spacer(minLength: KSpace.x3)
            trailing
        }
    }
}

/// The title and explanation of a setting.
struct KSettingText: View {
    var title: String
    var detail: String?
    var icon: String?
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                if let icon { Text(icon) }
                Text(title).font(KFont.ui(13, .medium)).foregroundStyle(theme.fg)
            }
            if let detail {
                Text(detail).font(KFont.ui(12)).foregroundStyle(theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A switch with a title and a line of explanation, the whole row its label.
struct KSettingToggle: View {
    var title: String
    var detail: String?
    @Binding var isOn: Bool

    init(_ title: String, detail: String? = nil, isOn: Binding<Bool>) {
        self.title = title
        self.detail = detail
        self._isOn = isOn
    }

    var body: some View {
        Toggle(isOn: $isOn) { KSettingText(title: title, detail: detail) }
            .toggleStyle(KToggleStyle())
    }
}

/// A one-pixel line between the rows of a card.
struct KHairline: View {
    @Environment(\.kTheme) private var theme

    var body: some View { Rectangle().fill(theme.line).frame(height: 1) }
}

/// A paragraph of explanation in the muted colour.
struct KExplanation: View {
    var text: String
    var size: CGFloat = 12
    @Environment(\.kTheme) private var theme

    init(_ text: String, size: CGFloat = 12) {
        self.text = text
        self.size = size
    }

    var body: some View {
        Text(text).font(KFont.ui(size)).foregroundStyle(theme.muted)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// What went wrong, in the danger colour.
struct KErrorText: View {
    var message: String
    @Environment(\.kTheme) private var theme

    init(_ message: String) { self.message = message }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: KSpace.x2) {
            KIcon(.warning, size: .sm, color: theme.danger).alignmentGuide(.firstTextBaseline) { $0[.bottom] - 2 }
            Text(message).font(KFont.ui(12)).foregroundStyle(theme.danger)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The top of a sheet: a kicker, its title and message, and a close button.
struct KSheetTitle: View {
    var kicker: String?
    var title: String
    var message: String?
    var close: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: KSpace.x3) {
            KSheetHeader(kicker: kicker, title: title, message: message)
            KIconButton(.close, title: "Close", size: .small, action: close)
                .keyboardShortcut(.cancelAction)
        }
    }
}

/// A sheet's own surface, for sheets that the window may present as a system sheet: the panel
/// colour edge to edge, with the sheet's padding.
struct KSheetSurface<Content: View>: View {
    var width: CGFloat
    @ViewBuilder var content: Content
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x5) { content }
            .padding(KSpace.x6)
            .frame(width: width, alignment: .topLeading)
            .background(theme.panel)
    }
}

/// The confirmations and sheets of a window of this part of the app, shown over the whole window
/// rather than over the pane that asks for them.
@MainActor @Observable
final class KOverlays {
    var confirmation: KConfirmation?
    var connecting: ConnectableAgent?
}

extension EnvironmentValues {
    @Entry var kOverlays: KOverlays?
}

extension View {
    /// Shows the confirmations and the connect sheet asked for inside this view, over it.
    func kOverlayHost(_ overlays: KOverlays) -> some View {
        @Bindable var overlays = overlays
        return self
            .environment(\.kOverlays, overlays)
            .kSheet(item: $overlays.connecting, width: 520) { agent in
                ConnectAgentView(initial: agent) { overlays.connecting = nil }
            }
            .kConfirm($overlays.confirmation)
    }
}
