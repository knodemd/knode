import AppKit
import KnodeBrand
import SwiftUI

/// The panes of the settings window.
enum SettingsTab: String, CaseIterable, Hashable {
    case agents, spaces, search, general

    /// The pane the settings window shows, kept between openings; the menu bar panel sets it to
    /// open the window on a pane.
    static let storageKey = "KnodeSettingsTab"

    var title: String {
        switch self {
        case .agents: "Agents and access"
        case .spaces: "Spaces"
        case .search: "Search"
        case .general: "General"
        }
    }

    var glyph: KGlyph {
        switch self {
        case .agents: .private
        case .spaces: .folder
        case .search: .search
        case .general: .settings
        }
    }
}

/// The settings window: our own bar of tabs under the traffic lights, and the pane below it.
struct SettingsView: View {
    @AppStorage(SettingsTab.storageKey) private var tab = SettingsTab.agents

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
        VStack(spacing: 0) {
            HStack {
                Spacer()
                KTabs(selection: $tab, options: SettingsTab.allCases.map { KOption($0, $0.title, icon: $0.glyph) })
                Spacer()
            }
            .padding(.top, 8)
            .frame(height: SettingsWindowChrome.barHeight)
            .background(theme.bg)
            KHairline()
            Group {
                switch tab {
                case .agents: AgentsSettingsView()
                case .spaces: SpacesSettingsView()
                case .search: SearchSettingsView()
                case .general: GeneralSettingsView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .scrollEdgeEffectHidden(true, for: .all)
        }
        .kOverlayHost(overlays)
        .toolbarBackgroundVisibility(.hidden, for: .windowToolbar)
        .toolbar(removing: .title)
        .background(SettingsWindowChrome())
        .ignoresSafeArea(.container, edges: .top)
        .frame(width: 680, height: 620)
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

/// The settings window without the system title bar: the content runs under it, the window takes
/// the background colour, and the traffic lights sit centred in our 52 pt bar of tabs.
struct SettingsWindowChrome: NSViewRepresentable {
    static let barHeight: CGFloat = 52

    func makeNSView(context: Context) -> SettingsWindowChromeView { SettingsWindowChromeView() }
    func updateNSView(_ view: SettingsWindowChromeView, context: Context) { view.placeButtons() }
}

final class SettingsWindowChromeView: NSView {
    private var observers: [NSObjectProtocol] = []

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        observers = []
        guard let window else { return }
        configure(window)
        DispatchQueue.main.async { [weak self] in
            guard let self, let window = self.window else { return }
            self.configure(window)
            self.placeButtons()
        }
        for name in [NSWindow.didResizeNotification, NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.placeButtons() }
            })
        }
        placeButtons()
    }

    override func layout() {
        super.layout()
        placeButtons()
    }

    private func configure(_ window: NSWindow) {
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.styleMask.insert(.fullSizeContentView)
        window.titlebarSeparatorStyle = .none
        window.backgroundColor = KColor.ns(\.bg)
    }

    /// Makes the title bar area as tall as the bar of tabs and centres the traffic lights in it.
    func placeButtons() {
        guard let window, let close = window.standardWindowButton(.closeButton),
              let titlebar = close.superview, let container = titlebar.superview else { return }
        let height = SettingsWindowChrome.barHeight
        if container.frame.height != height {
            var frame = container.frame
            frame.origin.y += frame.height - height
            frame.size.height = height
            container.frame = frame
            titlebar.frame = container.bounds
        }
        var x: CGFloat = 20
        for type in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            guard let button = window.standardWindowButton(type) else { continue }
            button.setFrameOrigin(NSPoint(x: x, y: ((height - button.frame.height) / 2).rounded()))
            x += button.frame.width + 6
        }
    }
}
