import Foundation
import Observation

/// An agent `knode connect` can add Knode to.
enum ConnectableAgent: String, CaseIterable, Identifiable {
    case claudeCode = "claude-code"
    case codex
    case gemini
    case claudeDesktop = "claude-desktop"

    var id: Self { self }

    var title: String {
        switch self {
        case .claudeCode: "Claude Code"
        case .codex: "Codex"
        case .gemini: "Gemini CLI"
        case .claudeDesktop: "Claude Desktop"
        }
    }

    /// The agent's name in Knode once connected.
    var agentName: String {
        self == .gemini ? "gemini-cli" : rawValue
    }

    /// The agent behind a name or kind, if `knode connect` knows it.
    init?(agent: String) {
        guard let match = Self.allCases.first(where: { $0.agentName == agent || $0.rawValue == agent }) else { return nil }
        self = match
    }
}

/// The agents of this machine and what each may do in each space, managed through Knode Service
/// as the person: the Agents settings and the Agent Access sheet of a space.
@MainActor
@Observable
final class AgentsModel {
    enum State: Equatable {
        case loading
        case loaded
        /// Knode Service is not running: agents are managed through it.
        case serviceStopped
    }

    /// What `knode connect` printed, and whether it worked.
    struct ConnectOutput: Equatable {
        var agent: ConnectableAgent
        var preview: Bool
        var text: String
        var succeeded: Bool
    }

    private(set) var agents: [AgentInfo] = []
    private(set) var state = State.loading
    /// A `knode` command or a call is running.
    private(set) var isBusy = false
    private(set) var connectOutput: ConnectOutput?
    var errorMessage: String?

    @ObservationIgnored private let directory: any AgentDirectory
    @ObservationIgnored private let command: @MainActor () -> KnodeCommand
    @ObservationIgnored private let startService: @MainActor () async -> Void

    /// - Parameters:
    ///   - directory: where agents are read and changed: Knode Service.
    ///   - command: the `knode` command, for `knode connect`.
    ///   - startService: starts Knode Service, as the menu bar does.
    init(directory: any AgentDirectory, command: @escaping @MainActor () -> KnodeCommand, startService: @escaping @MainActor () async -> Void) {
        self.directory = directory
        self.command = command
        self.startService = startService
    }

    func load() async {
        if agents.isEmpty { state = .loading }
        do {
            agents = try await directory.agents().sorted { $0.name < $1.name }
            state = .loaded
            errorMessage = nil
        } catch let error as ServiceError where error.meansNotRunning {
            agents = []
            state = .serviceStopped
        } catch {
            state = .loaded
            errorMessage = error.localizedDescription
        }
    }

    /// Starts Knode Service, then lists the agents.
    func startServiceAndLoad() async {
        isBusy = true
        await startService()
        isBusy = false
        await load()
    }

    func setLevel(_ level: GrantLevel, for agent: AgentInfo, in space: URL) async {
        guard agent.level(for: space) != level else { return }
        await perform { try await self.directory.setLevel(level, for: agent.name, in: space) }
    }

    func revoke(_ agent: AgentInfo) async {
        await perform { try await self.directory.revoke(agent.name) }
    }

    /// Runs `knode connect <agent>`, or with `preview` only shows what it would do
    /// (`--print`), and keeps what it printed. Connecting gives the agent a new key and, the first
    /// time, write access to the default space only.
    func connect(_ agent: ConnectableAgent, preview: Bool) async {
        isBusy = true
        defer { isBusy = false }
        var arguments = ["connect", agent.rawValue]
        if preview { arguments.append("--print") }
        do {
            let text = try await command().run(arguments)
            connectOutput = ConnectOutput(agent: agent, preview: preview, text: text, succeeded: true)
        } catch let failure as KnodeCommand.Failure {
            connectOutput = ConnectOutput(agent: agent, preview: preview, text: failure.output.isEmpty ? (failure.errorDescription ?? "") : failure.output, succeeded: false)
        } catch {
            connectOutput = ConnectOutput(agent: agent, preview: preview, text: error.localizedDescription, succeeded: false)
        }
        if !preview { await load() }
    }

    func clearConnectOutput() {
        connectOutput = nil
    }

    private func perform(_ work: @escaping () async throws -> Void) async {
        isBusy = true
        do {
            try await work()
            errorMessage = nil
        } catch let error as ServiceError where error.meansNotRunning {
            state = .serviceStopped
        } catch {
            errorMessage = error.localizedDescription
        }
        isBusy = false
        await load()
    }
}
