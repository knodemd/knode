import AppKit
import KnodeBrand
import SwiftUI

/// The panel of the Knode menu bar item: whether Knode Service runs, what it has open, the space
/// agents and capture write to, search by meaning, and the actions to open the app, start or stop
/// the service and start it at login.
struct ServiceMenu: View {
    let monitor: ServiceMonitor
    let model: AppModel
    let searchIndex: SearchIndexModel

    var body: some View {
        ServiceMenuPanel(monitor: monitor, model: model, searchIndex: searchIndex)
            .knodeTheme(background: true)
    }

    static func summary(of status: ServiceMonitor.Status) -> String {
        let spaces = status.spaces.count == 1 ? "1 space" : "\(status.spaces.count) spaces"
        let clients = status.clients == 1 ? "1 client" : "\(status.clients) clients"
        return "\(spaces) open, \(clients) connected"
    }

    static func name(ofSpaceAt path: String) -> String {
        URL(filePath: path).deletingPathExtension().lastPathComponent
    }
}

private struct ServiceMenuPanel: View {
    let monitor: ServiceMonitor
    let model: AppModel
    let searchIndex: SearchIndexModel
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
                .padding(KSpace.x4)
            KHairline()
            VStack(alignment: .leading, spacing: KSpace.x4) {
                if let error = monitor.errorMessage { KErrorText(error) }
                openSpaces
                spaceForAgents
                if case .running(let status) = monitor.state, let index = status.index {
                    SearchIndexMenu(status: index, index: searchIndex)
                }
            }
            .padding(KSpace.x4)
            KHairline()
            actions
                .padding(KSpace.x2)
        }
        .frame(width: 320)
    }

    private var header: some View {
        HStack(spacing: KSpace.x3) {
            KnodeMark()
                .foregroundStyle(theme.isDark ? theme.indigo : theme.fg)
                .frame(height: 22)
            VStack(alignment: .leading, spacing: 3) {
                Text("Knode Service").font(KFont.ui(13, .semibold))
                Text(statusLine).font(KFont.ui(12)).foregroundStyle(theme.muted).lineLimit(1)
            }
            Spacer(minLength: KSpace.x2)
            ServiceStatusChip(state: monitor.state)
        }
    }

    private var statusLine: String {
        switch monitor.state {
        case .checking: "Checking"
        case .stopped: "Agents cannot reach your spaces"
        case .running(let status): ServiceMenu.summary(of: status)
        }
    }

    @ViewBuilder private var openSpaces: some View {
        if case .running(let status) = monitor.state, !status.spaces.isEmpty {
            VStack(alignment: .leading, spacing: KSpace.x2) {
                KSectionLabel("Open spaces")
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(status.spaces, id: \.self) { path in
                        HStack(spacing: KSpace.x2) {
                            KIcon(.folder, size: .xs, color: theme.muted)
                            Text(ServiceMenu.name(ofSpaceAt: path)).font(KFont.ui(12)).lineLimit(1)
                        }
                        .kTooltip(path)
                    }
                }
                .padding(.horizontal, KSpace.x1)
            }
        }
    }

    /// Which space agents and capture write to: `default_space` in `config.toml`, read again by
    /// the service and the `knode` command, so no restart is needed.
    private var spaceForAgents: some View {
        let spaces = model.sidebarSpaces.filter { !$0.isExample && !$0.isMissing }
        return VStack(alignment: .leading, spacing: KSpace.x1) {
            KSectionLabel("Space for agents")
            if spaces.isEmpty {
                KExplanation("No spaces yet").padding(.horizontal, KSpace.x1)
            }
            VStack(spacing: 1) {
                ForEach(spaces) { item in
                    KRow(isSelected: item.isDefault, action: { model.setDefaultSpace(item.isDefault ? nil : item.url) }) {
                        Text(item.icon ?? "").frame(width: item.icon == nil ? 0 : 16)
                        Text(item.name).font(KFont.ui(13)).lineLimit(1)
                        Spacer(minLength: KSpace.x2)
                        if item.isDefault { KIcon(.done, size: .xs, color: theme.isDark ? theme.indigo : theme.fg) }
                    }
                    .accessibilityAddTraits(item.isDefault ? .isSelected : [])
                }
            }
            .padding(.horizontal, -KSpace.x1)
        }
        .onAppear { model.reloadDefaultSpace() }
    }

    private var actions: some View {
        VStack(spacing: 1) {
            ServiceMenuAction("Open Knode", icon: .home) {
                openWindow(id: "main")
                NSApp.activate()
            }
            ServiceMenuAction("Agents and access", icon: .private) {
                UserDefaults.standard.set(SettingsTab.agents.rawValue, forKey: SettingsTab.storageKey)
                NSApp.activate()
                openSettings()
            }
            if monitor.isRunning {
                ServiceMenuAction("Stop Knode Service", icon: .stop) { Task { await monitor.stopService() } }
                    .disabled(monitor.isBusy)
            } else {
                ServiceMenuAction("Start Knode Service", icon: .play) { Task { await monitor.startService() } }
                    .disabled(monitor.isBusy)
            }
            KToggle("Start at login", isOn: Binding(
                get: { monitor.runsAtLogin },
                set: { on in Task { await monitor.setRunsAtLogin(on) } }
            ))
            .font(KFont.ui(13))
            .padding(.horizontal, KSpace.x2)
            .frame(height: 30)
            .disabled(monitor.isBusy)
            KHairline().padding(.vertical, KSpace.x1).padding(.horizontal, KSpace.x2)
            ServiceMenuAction("Quit Knode", icon: .close, shortcut: [.command, .char("Q")]) { NSApp.terminate(nil) }
                .keyboardShortcut("q")
        }
    }
}

/// One action of the menu bar panel: a Knode icon, a title, and its key hint.
private struct ServiceMenuAction: View {
    var title: String
    var icon: KGlyph
    var shortcut: [KKey] = []
    var action: () -> Void
    @Environment(\.kTheme) private var theme
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    init(_ title: String, icon: KGlyph, shortcut: [KKey] = [], action: @escaping () -> Void) {
        self.title = title
        self.icon = icon
        self.shortcut = shortcut
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            HStack(spacing: KSpace.x2) {
                KIcon(icon, size: .sm)
                Text(title).font(KFont.ui(13))
                Spacer(minLength: KSpace.x2)
                if !shortcut.isEmpty { KKeyHint(shortcut) }
            }
            .foregroundStyle(hovering && isEnabled ? theme.hover : theme.fg)
            .padding(.horizontal, KSpace.x2)
            .frame(height: 30)
            .background(RoundedRectangle(cornerRadius: KRadius.small, style: .continuous).fill(hovering && isEnabled ? theme.highlight : .clear))
            .contentShape(Rectangle())
        }
        .buttonStyle(KPlainButtonStyle())
        .opacity(isEnabled ? 1 : 0.4)
        .onHover { hovering = $0 }
    }
}
