import KnodeBrand
import SwiftUI

/// Settings > Agents and access: every agent of this machine, its key, and what it may do in each
/// space. Agents are connected with `knode connect`, from here or from the terminal.
struct AgentsSettingsView: View {
    @Environment(AgentsModel.self) private var agents
    @Environment(AppModel.self) private var model
    @Environment(\.kOverlays) private var overlays

    var body: some View {
        KSettingsPane {
            KSettingsSection("Agents and access") {
                KButton("Connect an agent", icon: .connect, kind: .primary, size: .small) { overlays?.connecting = .claudeCode }
                    .disabled(agents.isBusy)
            } content: {
                GrantsExplanation()
            }
            if agents.state == .serviceStopped {
                ServiceStoppedNotice()
            } else if agents.state == .loaded, agents.agents.isEmpty {
                KCard(padding: 0) {
                    KEmptyState(icon: .people, title: "No agents yet", message: "Connect one to let it read or write your spaces.")
                        .frame(height: 220)
                }
            } else if agents.state == .loading, agents.agents.isEmpty {
                HStack(spacing: KSpace.x2) { KSpinner(); KExplanation("Loading agents") }
            }
            ForEach(agents.agents) { agent in
                KSettingsSection(agent.name) {
                    if agent.kind != agent.name { KChip(agent.kind) }
                } content: {
                    AgentDetails(agent: agent, onConnect: { overlays?.connecting = $0 })
                    KHairline()
                    let spaces = AgentSpaces.rows(for: agent, sidebar: model.sidebarSpaces)
                    if spaces.isEmpty {
                        KExplanation("No spaces yet.")
                    }
                    ForEach(spaces) { space in
                        GrantPicker(agent: agent, title: space.title, icon: space.icon, url: space.url)
                    }
                    KHairline()
                    HStack {
                        Spacer()
                        KButton("Revoke", icon: .key, kind: .destructive, size: .small) { confirmRevoke(agent) }
                            .disabled(agents.isBusy || agent.validKey == nil)
                    }
                }
            }
            if let error = agents.errorMessage {
                KErrorText(error)
            }
        }
        .task { await agents.load() }
    }

    private func confirmRevoke(_ agent: AgentInfo) {
        overlays?.confirmation = KConfirmation(
            title: "Revoke \(agent.name)?",
            message: "Its keys stop working on its next call, and it loses access to every space. Its past changes keep its name. Connect it again to give it a new key.",
            confirmTitle: "Revoke",
            isDestructive: true
        ) { [agents] in Task { await agents.revoke(agent) } }
    }
}

/// Which agents may do what in one space: from the Space menu and the space's context menu.
struct SpaceAccessSheet: View {
    @Environment(AgentsModel.self) private var agents
    @Environment(AppModel.self) private var model
    @Environment(\.openSettings) private var openSettings
    @Environment(\.kOverlays) private var overlays
    let url: URL

    var body: some View {
        KSheetSurface(width: 560) {
            KSheetTitle(kicker: "Agent access", title: spaceName, close: close)
            GrantsExplanation()
            if agents.state == .serviceStopped {
                ServiceStoppedNotice()
            } else {
                KCard(padding: KSpace.x4) {
                    VStack(alignment: .leading, spacing: KSpace.x3) {
                        if agents.state == .loaded, agents.agents.isEmpty {
                            KExplanation("No agents yet.")
                        }
                        ForEach(agents.agents) { agent in
                            GrantPicker(agent: agent, title: agent.name, icon: nil, url: url)
                        }
                    }
                }
            }
            if let error = agents.errorMessage {
                KErrorText(error)
            }
            HStack(spacing: KSpace.x2) {
                KButton("All agents", icon: .settings, kind: .ghost) {
                    UserDefaults.standard.set(SettingsTab.agents.rawValue, forKey: SettingsTab.storageKey)
                    openSettings()
                }
                Spacer()
                KButton("Connect an agent", icon: .connect) { overlays?.connecting = .claudeCode }
                    .disabled(agents.isBusy)
                Button(action: close) { HStack(spacing: 8) { KIcon(.done, size: .sm); Text("Done") } }
                    .buttonStyle(KButtonStyle(kind: .primary))
                    .keyboardShortcut(.defaultAction)
            }
        }
        .frame(minHeight: 480, alignment: .top)
        .knodeTheme()
        .task { await agents.load() }
    }

    private func close() {
        if model.sheet != nil { model.sheet = nil }
    }

    private var spaceName: String {
        let item = model.sidebarSpaces.first { $0.url == url.standardizedFileURL }
        let name = item?.name ?? url.deletingPathExtension().lastPathComponent
        return item?.icon.map { "\($0) \(name)" } ?? name
    }
}

/// Runs `knode connect <agent>` and shows what it printed.
struct ConnectAgentView: View {
    @Environment(AgentsModel.self) private var agents
    @Environment(\.kTheme) private var theme
    @State private var agent: ConnectableAgent
    let close: () -> Void

    init(initial: ConnectableAgent, close: @escaping () -> Void) {
        _agent = State(initialValue: initial)
        self.close = close
    }

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x5) {
            KSheetTitle(kicker: "Connect an agent", title: "Give an agent a key", close: close)
            KSettingRow("Agent") {
                KDropdown(selection: $agent, options: ConnectableAgent.allCases.map { KOption($0, $0.title) })
                    .frame(width: 200)
            }
            KExplanation("Connecting gives \(agent.title) a key of its own, kept in your Keychain, and adds Knode to its settings with knode connect \(agent.rawValue). The first time, it can read and write only the space for agents. Connecting an agent again gives it a new key; its old one stops working.")

            if let output = agents.connectOutput, output.agent == agent {
                VStack(alignment: .leading, spacing: KSpace.x2) {
                    HStack(spacing: KSpace.x2) {
                        KIcon(output.succeeded ? (output.preview ? .show : .done) : .warning, size: .sm,
                              color: output.succeeded ? (theme.isDark ? theme.indigo : theme.fg) : theme.danger)
                        Text(output.preview ? "What it will do" : (output.succeeded ? "Connected" : "It did not work"))
                            .kLineStyle(11)
                            .foregroundStyle(output.succeeded ? theme.fg : theme.danger)
                    }
                    KScrollView {
                        Text(output.text.isEmpty ? "No output." : output.text)
                            .font(KFont.mono(12))
                            .foregroundStyle(theme.fg)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(KSpace.x3)
                    }
                    .frame(height: 160)
                    .background(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous).fill(theme.bg))
                    .overlay(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous).strokeBorder(theme.line))
                }
            }

            HStack(spacing: KSpace.x2) {
                KButton("Show what it will do", icon: .show, kind: .ghost) {
                    Task { await agents.connect(agent, preview: true) }
                }
                .disabled(agents.isBusy)
                Spacer()
                if agents.isBusy { KSpinner() }
                KButton("Close", action: close)
                Button { Task { await agents.connect(agent, preview: false) } } label: {
                    HStack(spacing: 8) { KIcon(.connect, size: .sm); Text("Connect") }
                }
                .buttonStyle(KButtonStyle(kind: .primary))
                .keyboardShortcut(.defaultAction)
                .disabled(agents.isBusy)
            }
        }
        .onAppear { agents.clearConnectOutput() }
    }
}

/// What grants do, and what they do not.
struct GrantsExplanation: View {
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            HStack(spacing: KSpace.x2) {
                KIcon(.private, size: .sm, color: theme.isDark ? theme.indigo : theme.fg)
                Text("Each agent sees only the spaces you give it").font(KFont.ui(13, .semibold))
            }
            KExplanation("No Access hides a space from the agent completely. Read lets it read and search. Read and Write also lets it write, and undo its own changes. Changes take effect on the agent's next call.")
            KExplanation("Grants are not a sandbox: they decide what an agent can do through Knode. A program running as you can still open the files of a space, or run the knode command, which acts for you.")
        }
    }
}

/// Agents are managed through Knode Service: offers to start it.
struct ServiceStoppedNotice: View {
    @Environment(AgentsModel.self) private var agents

    var body: some View {
        KBanner(.warning, title: "Knode Service is not running",
                message: "Agents and their access are managed through Knode Service. Start it to see and change them.", actions: {
            KButton("Start", icon: .play, kind: .primary, size: .small) {
                Task { await agents.startServiceAndLoad() }
            }
            .disabled(agents.isBusy)
        })
    }
}

/// When an agent was added and the state of its key.
struct AgentDetails: View {
    let agent: AgentInfo
    let onConnect: (ConnectableAgent) -> Void
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(alignment: .top, spacing: KSpace.x6) {
            VStack(alignment: .leading, spacing: 3) {
                Text("Added").kLineStyle(10).foregroundStyle(theme.muted)
                Text(agent.created, format: .dateTime.day().month(.abbreviated).year()).font(KFont.ui(13))
            }
            VStack(alignment: .leading, spacing: 3) {
                Text("Key").kLineStyle(10).foregroundStyle(theme.muted)
                if let key = agent.validKey {
                    Text("Active, \(key.fingerprint.prefix(15))…")
                        .font(KFont.mono(12))
                        .kTooltip("Fingerprint \(key.fingerprint). The key itself stays in your Keychain.")
                } else {
                    Text("Revoked").font(KFont.ui(13)).foregroundStyle(theme.muted)
                }
            }
            Spacer()
            if agent.validKey == nil, let connectable = ConnectableAgent(agent: agent.kind) {
                KButton("Connect again", icon: .connect, size: .small) { onConnect(connectable) }
            }
        }
    }
}

/// The level of one agent in one space.
struct GrantPicker: View {
    @Environment(AgentsModel.self) private var agents
    let agent: AgentInfo
    let title: String
    let icon: String?
    let url: URL

    var body: some View {
        let grant = agent.grant(for: url)
        if grant != nil, grant?.level == nil {
            KSettingRow(title, detail: "Space unavailable", icon: icon) { EmptyView() }
        } else {
            KSettingRow(title, detail: agent.validKey == nil ? "Revoked: connect it again first." : agent.level(for: url).explanation, icon: icon) {
                KDropdown(selection: Binding(
                    get: { agent.level(for: url) },
                    set: { level in Task { await agents.setLevel(level, for: agent, in: url) } }
                ), options: GrantLevel.allCases.map { KOption($0, $0.title, icon: $0.glyph) }, size: .small)
                .frame(width: 170)
                .disabled(agents.isBusy || agent.validKey == nil)
            }
        }
    }
}

extension GrantLevel {
    var glyph: KGlyph {
        switch self {
        case .none: .hide
        case .read: .show
        case .write: .edit
        }
    }
}

/// The spaces an agent's access is shown for: the spaces in the sidebar, then any other space it
/// was given a level in.
enum AgentSpaces {
    struct Row: Identifiable, Hashable {
        var url: URL
        var title: String
        var icon: String?
        var id: URL { url }
    }

    static func rows(for agent: AgentInfo, sidebar: [SpaceItem]) -> [Row] {
        var rows = sidebar.filter { !$0.isExample && !$0.isMissing }.map { Row(url: $0.url, title: $0.name, icon: $0.icon) }
        let known = Set(rows.map { AgentInfo.canonicalPath($0.url) })
        for grant in agent.spaces {
            let url = URL(filePath: grant.space, directoryHint: .isDirectory)
            guard !known.contains(AgentInfo.canonicalPath(url)) else { continue }
            rows.append(Row(url: url, title: url.deletingPathExtension().lastPathComponent, icon: nil))
        }
        return rows
    }
}
