import KnodeBrand
import SwiftUI

/// The settings window: the Agents tab.
struct SettingsView: View {
    var body: some View {
        TabView {
            Tab("Agents", systemImage: "sparkles") {
                AgentsSettingsView()
            }
        }
        .frame(width: 600, height: 560)
    }
}

/// Every agent of this machine: its key, and what it may do in each space. Agents are connected
/// with `knode connect`, from here or from the terminal.
struct AgentsSettingsView: View {
    @Environment(AgentsModel.self) private var agents
    @Environment(AppModel.self) private var model
    @State private var connecting: ConnectableAgent?
    @State private var revoking: AgentInfo?

    var body: some View {
        Form {
            Section {
                GrantsExplanation()
                HStack {
                    Spacer()
                    Button("Connect an Agent…", systemImage: "link") { connecting = .claudeCode }
                        .disabled(agents.isBusy)
                }
            }
            if agents.state == .serviceStopped {
                Section { ServiceStoppedNotice() }
            } else if agents.state == .loaded, agents.agents.isEmpty {
                Section {
                    Text("No agents yet. Connect one to let it read or write your spaces.")
                        .foregroundStyle(.secondary)
                }
            }
            ForEach(agents.agents) { agent in
                Section {
                    AgentDetails(agent: agent, onConnect: { connecting = $0 })
                    let spaces = AgentSpaces.rows(for: agent, sidebar: model.sidebarSpaces)
                    if spaces.isEmpty {
                        Text("No spaces yet.").foregroundStyle(.secondary)
                    }
                    ForEach(spaces) { space in
                        GrantPicker(agent: agent, title: space.title, icon: space.icon, url: space.url)
                    }
                    HStack {
                        Spacer()
                        Button("Revoke…", systemImage: "key.slash", role: .destructive) { revoking = agent }
                            .disabled(agents.isBusy || agent.validKey == nil)
                    }
                } header: {
                    AgentHeader(agent: agent)
                }
            }
            if let error = agents.errorMessage {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.red)
                }
            }
        }
        .formStyle(.grouped)
        .task { await agents.load() }
        .sheet(item: $connecting) { agent in
            ConnectAgentSheet(initial: agent)
        }
        .confirmationDialog(
            "Revoke \(revoking?.name ?? "this agent")?",
            isPresented: Binding(get: { revoking != nil }, set: { if !$0 { revoking = nil } }),
            presenting: revoking
        ) { agent in
            Button("Revoke", role: .destructive) { Task { await agents.revoke(agent) } }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("Its keys stop working on its next call, and it loses access to every space. Its past changes keep its name. Connect it again to give it a new key.")
        }
    }
}

/// Which agents may do what in one space: from the Space menu and the space's context menu.
struct SpaceAccessSheet: View {
    @Environment(AgentsModel.self) private var agents
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let url: URL
    @State private var connecting: ConnectableAgent?

    var body: some View {
        Form {
            Section {
                GrantsExplanation()
            }
            if agents.state == .serviceStopped {
                Section { ServiceStoppedNotice() }
            } else {
                Section {
                    if agents.state == .loaded, agents.agents.isEmpty {
                        Text("No agents yet.").foregroundStyle(.secondary)
                    }
                    ForEach(agents.agents) { agent in
                        GrantPicker(agent: agent, title: agent.name, icon: nil, url: url)
                    }
                } header: {
                    Text(spaceName)
                } footer: {
                    HStack {
                        SettingsLink {
                            Label("All Agents…", systemImage: "gearshape")
                        }
                        Spacer()
                        Button("Connect an Agent…", systemImage: "link") { connecting = .claudeCode }
                            .disabled(agents.isBusy)
                    }
                }
            }
            if let error = agents.errorMessage {
                Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.red)
            }
        }
        .formStyle(.grouped)
        .frame(width: 480)
        .frame(minHeight: 340)
        .navigationTitle("Agent Access")
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done", systemImage: "checkmark") { dismiss() }
            }
        }
        .task { await agents.load() }
        .sheet(item: $connecting) { agent in
            ConnectAgentSheet(initial: agent)
        }
    }

    private var spaceName: String {
        let item = model.sidebarSpaces.first { $0.url == url.standardizedFileURL }
        let name = item?.name ?? url.deletingPathExtension().lastPathComponent
        return item?.icon.map { "\($0) \(name)" } ?? name
    }
}

/// Runs `knode connect <agent>` and shows what it printed.
struct ConnectAgentSheet: View {
    @Environment(AgentsModel.self) private var agents
    @Environment(\.dismiss) private var dismiss
    @State private var agent: ConnectableAgent

    init(initial: ConnectableAgent) {
        _agent = State(initialValue: initial)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Form {
                Picker("Agent", selection: $agent) {
                    ForEach(ConnectableAgent.allCases) { Text($0.title).tag($0) }
                }
                Text("Connecting gives \(agent.title) a key of its own, kept in your Keychain, and adds Knode to its settings with `knode connect \(agent.rawValue)`. The first time, it can read and write only the space for agents. Connecting an agent again gives it a new key; its old one stops working.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .formStyle(.grouped)

            if let output = agents.connectOutput, output.agent == agent {
                VStack(alignment: .leading, spacing: 6) {
                    Label(
                        output.preview ? "What it will do" : (output.succeeded ? "Connected" : "It did not work"),
                        systemImage: output.succeeded ? (output.preview ? "doc.text.magnifyingglass" : "checkmark.circle") : "exclamationmark.triangle"
                    )
                    .font(.headline)
                    .foregroundStyle(output.succeeded ? Color.primary : Color.red)
                    ScrollView {
                        Text(output.text.isEmpty ? "No output." : output.text)
                            .font(.system(size: 12, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(10)
                    }
                    .frame(height: 160)
                    .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
                }
                .padding(.horizontal, 20)
            }
        }
        .padding(.bottom, 16)
        .frame(width: 520)
        .navigationTitle("Connect an Agent")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Close", systemImage: "xmark") { dismiss() }
            }
            ToolbarItem {
                Button("Show What It Will Do", systemImage: "doc.text.magnifyingglass") {
                    Task { await agents.connect(agent, preview: true) }
                }
                .disabled(agents.isBusy)
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Connect", systemImage: "link") {
                    Task { await agents.connect(agent, preview: false) }
                }
                .disabled(agents.isBusy)
            }
        }
        .onAppear { agents.clearConnectOutput() }
    }
}

/// What grants do, and what they do not.
struct GrantsExplanation: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("Each agent sees only the spaces you give it", systemImage: "lock.shield")
                .font(.headline)
            Text("No Access hides a space from the agent completely. Read lets it read and search. Read and Write also lets it write, and undo its own changes. Changes take effect on the agent's next call.")
                .foregroundStyle(.secondary)
            Text("Grants are not a sandbox: they decide what an agent can do through Knode. A program running as you can still open the files of a space, or run the knode command, which acts for you.")
                .foregroundStyle(.secondary)
        }
        .font(.callout)
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// Agents are managed through Knode Service: offers to start it.
struct ServiceStoppedNotice: View {
    @Environment(AgentsModel.self) private var agents

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Knode Service is not running", systemImage: "pause.circle")
                .font(.headline)
            Text("Agents and their access are managed through Knode Service. Start it to see and change them.")
                .font(.callout)
                .foregroundStyle(.secondary)
            Button("Start Knode Service", systemImage: "play.circle") {
                Task { await agents.startServiceAndLoad() }
            }
            .disabled(agents.isBusy)
        }
    }
}

/// An agent's name and kind.
struct AgentHeader: View {
    let agent: AgentInfo

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "sparkles").foregroundStyle(KnodeColor.indigo)
            Text(agent.name).font(.headline)
            if agent.kind != agent.name {
                Text(agent.kind).foregroundStyle(.secondary)
            }
        }
    }
}

/// When an agent was added and the state of its key.
struct AgentDetails: View {
    let agent: AgentInfo
    let onConnect: (ConnectableAgent) -> Void

    var body: some View {
        LabeledContent("Added") {
            Text(agent.created, format: .dateTime.day().month(.abbreviated).year())
        }
        LabeledContent("Key") {
            if let key = agent.validKey {
                Text("Active, \(key.fingerprint.prefix(15))…")
                    .help("Fingerprint \(key.fingerprint). The key itself stays in your Keychain.")
            } else {
                HStack {
                    Text("Revoked").foregroundStyle(.secondary)
                    if let connectable = ConnectableAgent(agent: agent.kind) {
                        Button("Connect Again…", systemImage: "link") { onConnect(connectable) }
                    }
                }
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
            LabeledContent(title) {
                Text("Space unavailable").foregroundStyle(.secondary)
            }
        } else {
            Picker(selection: Binding(
                get: { agent.level(for: url) },
                set: { level in Task { await agents.setLevel(level, for: agent, in: url) } }
            )) {
                ForEach(GrantLevel.allCases) { level in
                    Text(level.title).tag(level)
                }
            } label: {
                HStack(spacing: 6) {
                    if let icon { Text(icon) }
                    Text(title)
                }
                Text(agent.validKey == nil ? "Revoked: connect it again first." : agent.level(for: url).explanation)
            }
            .disabled(agents.isBusy || agent.validKey == nil)
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
