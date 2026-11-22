import KnodeBrand
import SwiftUI

/// Settings > Agents and access: every agent of this machine, its key, and what it may do in each
/// space. Agents are connected with `knode connect`, from here or from the terminal.
struct AgentsSettingsView: View {
    @Environment(AgentsModel.self) private var agents
    @Environment(AppModel.self) private var model
    @Environment(\.kOverlays) private var overlays
    @State private var checks: AgentChecks

    init() {
        _checks = State(initialValue: AgentChecks { arguments in
            try await KnodeCommand.withShellPath().run(arguments)
        })
    }

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
                    if let connectable = ConnectableAgent.checkable(agent) {
                        KHairline()
                        AgentCheckSection(agent: connectable, checks: checks)
                    }
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
        .task(id: agents.agents.compactMap(ConnectableAgent.checkable).map(\.rawValue)) {
            for agent in agents.agents.compactMap(ConnectableAgent.checkable) where checks.results[agent.rawValue] == nil {
                await checks.check(agent.rawValue)
            }
        }
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
                    SettingsTab.select(.agents)
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

/// Connect your agents: the agents of this machine, Claude Code first, each with a Connect button,
/// and what to do next once one is connected. On the first run and in Get started.
struct ConnectAgentsPanel: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    @State private var connect: ConnectAgentsModel
    @State private var checks: AgentChecks
    @State private var space: URL?

    init() {
        let refusal = ConnectAgentsModel.moveFirst(
            executable: CommandLineTool.forThisApp.executable().resolvingSymlinksInPath(),
            bundle: Bundle.main.bundleURL.resolvingSymlinksInPath(),
            home: FileManager.default.homeDirectoryForCurrentUser
        )
        _connect = State(initialValue: ConnectAgentsModel(refusal: refusal) { arguments in
            try await KnodeCommand.withShellPath().run(arguments)
        })
        _checks = State(initialValue: AgentChecks { arguments in
            try await KnodeCommand.withShellPath().run(arguments)
        })
    }

    /// What the checks depend on: the chosen space and the agents connected.
    private var checkKey: [String] {
        [space?.path(percentEncoded: false) ?? ""] + connect.agents.filter(\.connected).map(\.agent)
    }

    private var spaces: [SpaceItem] { model.sidebarSpaces.filter { !$0.isExample && !$0.isMissing } }

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x3) {
            KExplanation("Connecting gives an agent a key of its own, kept in your Keychain, and adds Knode to its settings. The first time, it can read and write only this space.")
            if let refusal = connect.refusal {
                KBanner(.warning, title: "Knode is not in Applications", message: refusal, actions: {
                    KButton("Move to Applications", icon: .folder, kind: .primary, size: .small) {
                        _ = ApplicationMover().offerToMove(evenIfDeclined: true)
                    }
                })
            }
            if spaces.isEmpty {
                KExplanation("You have no space yet: your agent will make one when it sets up your memory.")
            } else {
                KSettingRow("Space") {
                    KDropdown(selection: $space, options: spaces.map { KOption(Optional($0.url), $0.isDefault ? "\($0.name) (space for agents)" : $0.name) }, size: .small)
                        .frame(width: 260)
                }
            }
            switch connect.state {
            case .loading:
                HStack(spacing: KSpace.x2) { KSpinner(); KExplanation("Looking for your agents") }
            case .failed(let message):
                KErrorText(message).textSelection(.enabled)
                KButton("Check again", icon: .search, size: .small) { Task { await connect.load() } }
            case .loaded where connect.agents.isEmpty:
                KExplanation("No agents found on this Mac. Install Claude Code or Codex, then check again.")
                KButton("Check again", icon: .search, size: .small) { Task { await connect.load() } }
            case .loaded:
                ForEach(connect.agents) { agent in row(agent) }
                HStack {
                    Spacer()
                    KButton("Connect all detected", icon: .connect, kind: .primary, size: .small) {
                        Task { await connect.connectAllDetected(space: space, defaultSpace: model.defaultSpaceURL) }
                    }
                    .disabled(connect.isBusy || connect.refusal != nil || connect.detectedNotConnected.isEmpty)
                }
            }
        }
        .task { await connect.load() }
        .task(id: checkKey) {
            for agent in connect.agents where agent.connected {
                await checks.check(agent.agent, space: space, defaultSpace: model.defaultSpaceURL)
            }
        }
        .onAppear { if space == nil { space = model.defaultSpaceURL ?? spaces.first?.url } }
    }

    private func row(_ agent: AgentOnThisMachine) -> some View {
        VStack(alignment: .leading, spacing: KSpace.x1) {
            HStack(spacing: KSpace.x2) {
                Text(agent.title).font(KFont.ui(13, .semibold))
                if !agent.detected { Text("Not found on this Mac").font(KFont.ui(11)).foregroundStyle(theme.muted) }
                Spacer()
                if agent.connected {
                    HStack(spacing: KSpace.x1) { KIcon(.done, size: .sm); Text("Connected").font(KFont.ui(12)) }
                } else if connect.outcomes[agent.agent] == .connecting {
                    KSpinner()
                } else {
                    KButton("Connect", icon: .connect, size: .small) {
                        Task { await connect.connect(agent, space: space, defaultSpace: model.defaultSpaceURL) }
                    }
                    .disabled(connect.isBusy || connect.refusal != nil)
                }
            }
            if connect.connectedNow.contains(agent.agent) {
                KExplanation(ConnectAgentsModel.nextStep(for: agent.title))
            }
            if agent.connected {
                AgentCheckView(result: checks.results[agent.agent])
            }
            switch connect.outcomes[agent.agent] {
            case .notConnected(let text), .failed(let text):
                KErrorText(text.isEmpty ? "\(agent.title) is not connected yet." : text).textSelection(.enabled)
            default:
                EmptyView()
            }
        }
    }
}

/// The Connect your agents step on its own, after the `knode` command is installed on the first
/// run and from the Knode menu.
struct ConnectAgentsSheet: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        KSheetSurface(width: 560) {
            KSheetTitle(kicker: "Get started", title: "Connect your agents", close: { model.sheet = nil })
            ConnectAgentsPanel()
            HStack {
                Spacer()
                KButton("Done", icon: .done, kind: .primary) { model.sheet = nil }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .knodeTheme()
    }
}

/// Which space an agent reaches, in Settings: checked against the default space, or against a
/// project folder the person picks. It never changes anything.
struct AgentCheckSection: View {
    let agent: ConnectableAgent
    let checks: AgentChecks

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            HStack(spacing: KSpace.x2) {
                Text("Space it reaches").kLineStyle(10)
                Spacer()
                KButton("Check a project", icon: .folder, size: .small) {
                    guard let project = SpacePanels.chooseFolder(title: "Check a Project", message: "Checks whether \(agent.title) reaches the space this folder's conversations go to. Nothing is changed.") else { return }
                    Task { await checks.check(agent.rawValue, project: project) }
                }
                KButton("Check again", icon: .search, size: .small) { Task { await checks.check(agent.rawValue) } }
            }
            .disabled(checks.results[agent.rawValue] == .checking)
            AgentCheckView(result: checks.results[agent.rawValue])
        }
    }
}

/// What `knode connect <agent> --check` found: OK, or each problem with the command that fixes
/// it, to run in Terminal. The app runs none of them.
struct AgentCheckView: View {
    let result: AgentChecks.Result?
    @Environment(\.kTheme) private var theme

    var body: some View {
        switch result {
        case nil:
            EmptyView()
        case .checking:
            HStack(spacing: KSpace.x2) { KSpinner(); KExplanation("Checking which space it reaches") }
        case .failed(let message):
            KErrorText(message)
        case .checked(let check):
            VStack(alignment: .leading, spacing: KSpace.x2) {
                if check.ok {
                    HStack(alignment: .firstTextBaseline, spacing: KSpace.x2) {
                        KIcon(.done, size: .sm, color: theme.isDark ? theme.indigo : theme.fg)
                        Text(Self.okText(check)).font(KFont.ui(12)).foregroundStyle(theme.fg)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                ForEach(Array(check.problems.enumerated()), id: \.offset) { _, problem in
                    KWarningText(problem.text)
                    ForEach(problem.commands, id: \.self) { command in
                        SuggestedCommand(command: command)
                    }
                }
                ForEach(check.notes, id: \.self) { note in KExplanation(note) }
                if !check.ok {
                    KExplanation("Nothing was changed. Run the command in Terminal, then check again.")
                }
            }
        }
    }

    /// What an OK check says, in one sentence.
    static func okText(_ check: AgentCheck) -> String {
        let space = check.intended.map { "\u{201C}\($0.shown)\u{201D}" } ?? "its space"
        let project = check.project.map { ", where the conversations of \(URL(filePath: $0).lastPathComponent) go" } ?? ""
        let access = check.access == "write" ? ", and may read and write there" : check.access == "read" ? ", and may read there" : ""
        return "Reaches \(space)\(project)\(access)."
    }
}

/// A command the check suggests, to copy into Terminal.
struct SuggestedCommand: View {
    let command: String
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(spacing: KSpace.x2) {
            Text(command).font(KFont.mono(12)).foregroundStyle(theme.fg).textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
            KIconButton(.copy, title: "Copy command", size: .small) {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(command, forType: .string)
            }
        }
        .padding(.horizontal, KSpace.x3)
        .padding(.vertical, KSpace.x1)
        .background(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous).fill(theme.bg))
        .overlay(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous).strokeBorder(theme.line))
        .padding(.leading, 14 + KSpace.x2)
    }
}
