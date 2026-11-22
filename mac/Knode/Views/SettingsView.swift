import AppKit
import KnodeBrand
import SwiftUI

/// The sections of the settings window, in the order of its sidebar.
enum SettingsTab: String, CaseIterable, Hashable {
    case sources, spaces, agents, search, general

    /// The section the settings window shows, kept between openings; the main window and the
    /// menu bar panel set it to open the window on a section.
    static let storageKey = "KnodeSettingsTab"
    static let defaultTab = SettingsTab.sources

    /// The section stored in `defaults`, or the first one when none or an unknown one is stored.
    static func selected(in defaults: UserDefaults = .standard) -> SettingsTab {
        defaults.string(forKey: storageKey).flatMap(SettingsTab.init(rawValue:)) ?? defaultTab
    }

    /// Makes the settings window show `tab`, now if it is open or the next time it opens.
    static func select(_ tab: SettingsTab, in defaults: UserDefaults = .standard) {
        defaults.set(tab.rawValue, forKey: storageKey)
    }

    /// Opens the settings window, on `tab` when one is given.
    @MainActor static func open(_ tab: SettingsTab?, with openSettings: OpenSettingsAction) {
        if let tab { select(tab) }
        NSApp.activate()
        openSettings()
    }

    var title: String {
        switch self {
        case .agents: "Agents and access"
        case .sources: "Sources"
        case .spaces: "Spaces"
        case .search: "Search"
        case .general: "General"
        }
    }

    /// One line under the title of the section.
    var subtitle: String {
        switch self {
        case .agents: "Which agents reach your spaces, and what each one may do."
        case .sources: "Mail, chats, calendars and other apps that bring things into a space."
        case .spaces: "Where your spaces are, and what each one keeps."
        case .search: "Search by meaning, on this Mac."
        case .general: "Knode Service, the space for agents, and the knode command."
        }
    }

    var glyph: KGlyph {
        switch self {
        case .agents: .private
        case .sources: .import
        case .spaces: .folder
        case .search: .search
        case .general: .settings
        }
    }
}

/// The settings window: a sidebar of sections, as in System Settings, and the section beside it.
struct SettingsView: View {
    @AppStorage(SettingsTab.storageKey) private var tab = SettingsTab.defaultTab

    var body: some View {
        SettingsWindow(tab: $tab)
            .knodeTheme(background: true)
    }
}

private struct SettingsWindow: View {
    @Binding var tab: SettingsTab
    @State private var overlays = KOverlays()
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(spacing: 0) {
            SettingsSidebar(tab: $tab)
                .frame(width: 210)
            KDivider(.vertical)
            VStack(alignment: .leading, spacing: 0) {
                HStack(alignment: .bottom, spacing: KSpace.x3) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(tab.title).kBlockStyle(17).foregroundStyle(theme.fg)
                        Text(tab.subtitle).font(KFont.ui(12)).foregroundStyle(theme.muted).lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    if tab == .sources {
                        ConnectSourcesButton()
                    }
                }
                .padding(.horizontal, KSpace.x8)
                .frame(maxWidth: .infinity, minHeight: WindowChrome.barHeight + KSpace.x4, alignment: .bottomLeading)
                .padding(.bottom, KSpace.x3)
                .background {
                    theme.bg.gesture(WindowDragGesture())
                }
                KHairline()
                Group {
                    switch tab {
                    case .agents: AgentsSettingsView()
                    case .sources: SourcesSettingsView()
                    case .spaces: SpacesSettingsView()
                    case .search: SearchSettingsView()
                    case .general: GeneralSettingsView()
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                .scrollEdgeEffectHidden(true, for: .all)
            }
            .background(theme.bg)
        }
        .kOverlayHost(overlays)
        .toolbarBackgroundVisibility(.hidden, for: .windowToolbar)
        .toolbar(removing: .title)
        .background(WindowChrome())
        .ignoresSafeArea(.container, edges: .top)
        .frame(minWidth: 820, idealWidth: 880, maxWidth: .infinity, minHeight: 560, idealHeight: 660, maxHeight: .infinity)
    }
}

/// The sections of the settings window, below the traffic lights so nothing sits under them.
private struct SettingsSidebar: View {
    @Binding var tab: SettingsTab
    @Environment(\.kTheme) private var theme
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // The traffic lights sit in this band, as in the main window.
            Color.clear
                .frame(height: WindowChrome.barHeight)
                .contentShape(Rectangle())
                .gesture(WindowDragGesture())
            KSectionLabel("Settings")
                .padding(.horizontal, KSpace.x5)
                .padding(.bottom, KSpace.x2)
            VStack(spacing: 2) {
                ForEach(SettingsTab.allCases, id: \.self) { item in
                    KRow(isSelected: item == tab, isFocused: focused && item == tab, action: { tab = item }) {
                        KIcon(item.glyph, size: .sm, color: item == tab ? (theme.isDark ? theme.indigo : theme.fg) : theme.muted)
                            .frame(width: 18)
                        Text(item.title)
                            .font(KFont.ui(13, item == tab ? .semibold : .medium))
                            .foregroundStyle(theme.fg)
                            .lineLimit(1)
                    }
                    .id(item)
                    .accessibilityLabel(item.title)
                }
            }
            .padding(.horizontal, KSpace.x2)
            .kKeyboardList(ids: SettingsTab.allCases, selection: tab, focus: $focused, select: { tab = $0 }, activate: { tab = $0 })
            Spacer(minLength: 0)
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .background(theme.panel)
    }
}

/// Settings > General: Knode Service, the space for agents and capture, and the knode command.
struct GeneralSettingsView: View {
    @Environment(ServiceMonitor.self) private var monitor
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    @State private var installer = CommandLineToolInstaller()

    var body: some View {
        KSettingsPane {
            KSettingsSection("Knode Service") {
                KSettingRow("Knode Service", detail: serviceDetail) {
                    ServiceStatusChip(state: monitor.state)
                    if monitor.isRunning {
                        KButton("Stop", icon: .stop, size: .small) { Task { await monitor.stopService() } }
                            .disabled(monitor.isBusy)
                    } else {
                        KButton("Start", icon: .play, kind: .primary, size: .small) { Task { await monitor.startService() } }
                            .disabled(monitor.isBusy)
                    }
                }
                KHairline()
                KSettingToggle("Start at login", detail: "Knode Service starts when you log in, so agents can reach your spaces before you open the app.",
                               isOn: Binding(get: { monitor.runsAtLogin }, set: { on in Task { await monitor.setRunsAtLogin(on) } }))
                    .disabled(monitor.isBusy)
                if let error = monitor.errorMessage { KErrorText(error) }
            }

            KSettingsSection("Agents and capture") {
                KSettingRow("Space for agents", detail: "Agents and capture write here, unless a folder of another space sends a conversation there.") {
                    DefaultSpaceDropdown()
                        .frame(width: 210)
                }
            }

            KSettingsSection("Command line tool") {
                KSettingRow("The knode command", detail: "With it, Claude Code, Codex and your terminal can use Knode.") {
                    KButton("Uninstall", icon: .delete, kind: .ghost, size: .small) { installer.uninstall() }
                    KButton("Install", icon: .terminal, size: .small) { installer.install() }
                }
            }

            HStack(spacing: KSpace.x3) {
                KnodeMark().foregroundStyle(theme.isDark ? theme.indigo : theme.fg).frame(height: 18)
                Text("Knode").kBlockStyle(15)
                Spacer()
                Text(Self.version).kLineStyle(10).foregroundStyle(theme.muted)
            }
            .padding(.horizontal, KSpace.x1)
        }
    }

    private var serviceDetail: String {
        switch monitor.state {
        case .checking: "Checking whether it runs."
        case .stopped: "Agents reach your spaces through it. It is not running."
        case .running(let status): ServiceMenu.summary(of: status).prefix(1).uppercased() + ServiceMenu.summary(of: status).dropFirst() + "."
        }
    }

    static var version: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "0"
        let build = info?["CFBundleVersion"] as? String
        return build.map { "Version \(short) (\($0))" } ?? "Version \(short)"
    }
}

/// Which space agents and capture write to: `default_space` in `config.toml`.
struct DefaultSpaceDropdown: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let spaces = model.sidebarSpaces.filter { !$0.isExample && !$0.isMissing }
        let current = spaces.first { $0.isDefault }?.url
        KDropdown(
            selection: Binding(get: { current }, set: { model.setDefaultSpace($0) }),
            options: [KOption<URL?>(nil, "None")] + spaces.map { item in KOption(Optional(item.url), item.icon.map { "\($0) \(item.name)" } ?? item.name) },
            size: .small
        )
        .onAppear { model.reloadDefaultSpace() }
    }
}

/// Whether Knode Service runs, as a chip.
struct ServiceStatusChip: View {
    var state: ServiceMonitor.State

    var body: some View {
        switch state {
        case .checking: KChip("Checking")
        case .stopped: KChip("Stopped")
        case .running: KChip("Running", icon: .done, kind: .accent)
        }
    }
}
