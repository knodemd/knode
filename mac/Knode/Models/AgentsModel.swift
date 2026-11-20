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

    /// The agent `knode connect --check` checks for `agent`, when it has a key: by its own name,
    /// never by its kind, which would show the check of another agent of that kind.
    static func checkable(_ agent: AgentInfo) -> ConnectableAgent? {
        guard agent.validKey != nil else { return nil }
        return ConnectableAgent(agent: agent.name)
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

// MARK: Connect your agents

/// An agent as `knode connect --list --json` describes it: the core decides what is on this
/// machine and what is already connected; the app only shows it.
struct AgentOnThisMachine: Decodable, Equatable, Identifiable, Sendable {
    /// What to type after `knode connect`: `claude-code`.
    var agent: String
    var title: String
    /// Its program or its settings folder is on this machine.
    var detected: Bool
    /// Its settings already run Knode as itself.
    var connected: Bool
    /// Knode can be added to it (Aider has no MCP client).
    var canConnect: Bool

    var id: String { agent }

    enum CodingKeys: String, CodingKey {
        case agent, title, detected, connected
        case canConnect = "can_connect"
    }

    /// The list `knode connect --list --json` printed. Anything printed before the JSON, such
    /// as a warning, is skipped.
    static func decodeList(_ text: String) throws -> [AgentOnThisMachine] {
        struct List: Decodable { var agents: [AgentOnThisMachine] }
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        let json = lines.firstIndex { $0.hasPrefix("{") }.map { lines[$0...].joined(separator: "\n") } ?? text
        return try JSONDecoder().decode(List.self, from: Data(json.utf8)).agents
    }

    /// The agents the Connect your agents step shows, in order: the ones Knode can be added to
    /// that are on this machine or already connected. Claude Code first, then Codex, then the
    /// others in the order the core lists them.
    static func shown(_ all: [AgentOnThisMachine]) -> [AgentOnThisMachine] {
        let rank = { (agent: AgentOnThisMachine) -> Int in
            switch agent.agent {
            case "claude-code": 0
            case "codex": 1
            default: 2
            }
        }
        return all.enumerated()
            .filter { $0.element.canConnect && ($0.element.detected || $0.element.connected) }
            .sorted { (rank($0.element), $0.offset) < (rank($1.element), $1.offset) }
            .map(\.element)
    }
}

/// The Connect your agents step, on the first run and in Get started: the agents of this machine
/// with a Connect button each, run through `knode connect <agent>`. Once connected, the person
/// opens a new session of the agent and it sets their memory up.
@MainActor
@Observable
final class ConnectAgentsModel {
    enum State: Equatable {
        case loading
        case loaded
        /// `knode connect --list --json` failed, with what it said.
        case failed(String)
    }

    /// What happened when the person connected one agent.
    enum Outcome: Equatable {
        case connecting
        /// `knode connect` ran, but the agent's settings still do not run Knode, with what it
        /// printed (for example: the agent's own command is not installed).
        case notConnected(String)
        case failed(String)
    }

    private(set) var agents: [AgentOnThisMachine] = []
    private(set) var state = State.loading
    private(set) var outcomes: [String: Outcome] = [:]
    /// The agents connected in this step, which the person still has to open a session of.
    private(set) var connectedNow: [String] = []

    /// Why agents cannot be connected from where the app runs (see `moveFirst`), if so.
    let refusal: String?

    @ObservationIgnored private let run: @MainActor ([String]) async throws -> String

    /// - Parameters:
    ///   - refusal: why connecting is refused; listing still works.
    ///   - run: runs `knode` with these arguments and returns what it printed.
    init(refusal: String? = nil, run: @escaping @MainActor ([String]) async throws -> String) {
        self.refusal = refusal
        self.run = run
    }

    /// Why agents must not be connected yet: `executable` (symbolic links resolved) is the `knode`
    /// inside this app while it runs from its disk image, from where macOS moved it aside, from
    /// Downloads or from a temporary folder. Agents' settings would point at a copy that goes
    /// away, so the person moves Knode to Applications first.
    static func moveFirst(executable: URL, bundle: URL, home: URL) -> String? {
        let app = bundle.standardizedFileURL.path(percentEncoded: false)
        let folder = app.hasSuffix("/") ? app : app + "/"
        guard executable.standardizedFileURL.path(percentEncoded: false).hasPrefix(folder) else { return nil }
        let temporary = [home.appending(path: "Downloads").path(percentEncoded: false), "/private/var/folders", "/var/folders", "/private/tmp", "/tmp"]
            .map { $0.hasSuffix("/") ? $0 : $0 + "/" }
        switch ApplicationLocation(bundle: bundle, home: home) {
        case .installed:
            return nil
        case .diskImage, .translocated:
            break
        case .elsewhere:
            guard temporary.contains(where: { folder.hasPrefix($0) }) else { return nil }
        }
        return "Move Knode to the Applications folder first. Agents run the knode command inside Knode, "
            + "and this copy goes away when its disk image is ejected or its folder is cleaned."
    }

    var isBusy: Bool { outcomes.values.contains(.connecting) }

    /// The shown agents that are on this machine and not connected yet: what Connect all
    /// detected connects.
    var detectedNotConnected: [AgentOnThisMachine] {
        agents.filter { $0.detected && !$0.connected }
    }

    func load() async {
        if agents.isEmpty { state = .loading }
        do {
            agents = AgentOnThisMachine.shown(try AgentOnThisMachine.decodeList(try await run(["connect", "--list", "--json"])))
            state = .loaded
        } catch {
            state = .failed(Self.isTooOld(error) ? Self.updateKnode : Self.message(error))
        }
    }

    /// The arguments that connect `agent` to `space`. The default space is not named, so the
    /// agent follows it when the person changes it.
    static func arguments(connecting agent: String, to space: URL?, defaultSpace: URL?) -> [String] {
        var arguments = ["connect", agent]
        if let space, space.standardizedFileURL != defaultSpace?.standardizedFileURL {
            arguments += ["--space", space.path(percentEncoded: false)]
        }
        return arguments
    }

    /// Runs `knode connect <agent>`, then reads again which agents are connected: the core says
    /// whether it worked, not the exit status alone.
    func connect(_ agent: AgentOnThisMachine, space: URL?, defaultSpace: URL?) async {
        if let refusal {
            outcomes[agent.agent] = .failed(refusal)
            return
        }
        outcomes[agent.agent] = .connecting
        let output: String
        do {
            output = try await run(Self.arguments(connecting: agent.agent, to: space, defaultSpace: defaultSpace))
        } catch {
            outcomes[agent.agent] = .failed(Self.message(error))
            return
        }
        await load()
        if agents.first(where: { $0.agent == agent.agent })?.connected == true {
            outcomes[agent.agent] = nil
            if !connectedNow.contains(agent.agent) { connectedNow.append(agent.agent) }
        } else {
            outcomes[agent.agent] = .notConnected(output.trimmingCharacters(in: .whitespacesAndNewlines))
        }
    }

    /// Connects every shown agent on this machine that is not connected yet, one after another.
    func connectAllDetected(space: URL?, defaultSpace: URL?) async {
        guard refusal == nil else { return }
        for agent in detectedNotConnected {
            await connect(agent, space: space, defaultSpace: defaultSpace)
        }
    }

    /// What to do next once `agent` is connected.
    static func nextStep(for title: String) -> String {
        "Open a new \(title) session: it will set up your memory."
    }

    /// What to do when `knode` is older than `knode connect --list --json`.
    static let updateKnode = "This knode is too old to list your agents. Update it with "
        + "brew upgrade knodemd/tap/knode (or install the latest Knode), then check again."

    /// Whether `knode` refused `--list --json` because it does not know them yet.
    private static func isTooOld(_ error: any Error) -> Bool {
        guard let failure = error as? KnodeCommand.Failure else { return false }
        return failure.output.contains("unexpected argument '--json'") || failure.output.contains("unexpected argument '--list'")
    }

    private static func message(_ error: any Error) -> String {
        if let failure = error as? KnodeCommand.Failure, !failure.output.isEmpty {
            return failure.output.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return error.localizedDescription
    }
}

// MARK: Check an agent

/// What `knode connect <agent> --check --json` reports: whether the agent reaches the space meant
/// for it, and if not, why and what to run. The core decides; the app only shows it. The check
/// writes nothing and grants nothing.
struct AgentCheck: Decodable, Equatable, Sendable {
    struct Problem: Decodable, Equatable, Sendable {
        /// `not_connected`, `no_key`, `other_agent`, `no_space`, `wrong_space`, `unknown_agent`,
        /// `no_access` or `ambiguous_project`, or a code of a newer version.
        var code: String
        var message: String

        /// The message as a sentence, without the backticks around commands.
        var text: String { message.replacingOccurrences(of: "`", with: "") }
        /// The `knode` commands the message suggests, in order.
        var commands: [String] { AgentCheck.commands(in: message) }
    }

    /// The space the agent is meant to use, and why.
    struct Intended: Decodable, Equatable, Sendable {
        var space: String
        var name: String?
        /// `flag`, `project` or `default`.
        var from: String

        var shown: String { name ?? URL(filePath: space).deletingPathExtension().lastPathComponent }
    }

    /// What to type after `knode connect`.
    var agent: String
    /// The agent's name in Knode.
    var name: String
    /// The project folder checked, if any.
    var project: String?
    var intended: Intended?
    /// `none`, `read` or `write` in the space the agent uses, when known.
    var access: String?
    var ok: Bool
    var problems: [Problem]
    var notes: [String]

    /// The report `--check --json` printed. Anything printed before the JSON is skipped.
    static func decode(_ text: String) throws -> AgentCheck {
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        let json = lines.firstIndex { $0.hasPrefix("{") }.map { lines[$0...].joined(separator: "\n") } ?? text
        return try JSONDecoder().decode(AgentCheck.self, from: Data(json.utf8))
    }

    /// The commands between backticks in a message that run `knode`.
    static func commands(in message: String) -> [String] {
        message.split(separator: "`", omittingEmptySubsequences: false)
            .enumerated()
            .filter { $0.offset % 2 == 1 && $0.element.hasPrefix("knode ") }
            .map { String($0.element) }
    }

    /// The arguments that check `agent`: against the space a `project` folder's conversations go
    /// to, else against `space` when it is not the default space (the default is what the check
    /// compares with when none is named).
    static func arguments(agent: String, space: URL? = nil, defaultSpace: URL? = nil, project: URL? = nil) -> [String] {
        var arguments = ["connect", agent, "--check", "--json"]
        if let project {
            arguments += ["--project", project.path(percentEncoded: false)]
        } else if let space, space.standardizedFileURL != defaultSpace?.standardizedFileURL {
            arguments += ["--space", space.path(percentEncoded: false)]
        }
        return arguments
    }
}

/// The checks of the agents on one screen, by what to type after `knode connect`.
@MainActor
@Observable
final class AgentChecks {
    enum Result: Equatable {
        case checking
        case checked(AgentCheck)
        case failed(String)
    }

    private(set) var results: [String: Result] = [:]

    @ObservationIgnored private let run: @MainActor ([String]) async throws -> String

    /// - Parameter run: runs `knode` with these arguments and returns what it printed.
    init(run: @escaping @MainActor ([String]) async throws -> String) {
        self.run = run
    }

    /// Runs `knode connect <agent> --check --json` and keeps its report. Nothing is changed.
    func check(_ agent: String, space: URL? = nil, defaultSpace: URL? = nil, project: URL? = nil) async {
        results[agent] = .checking
        do {
            let output = try await run(AgentCheck.arguments(agent: agent, space: space, defaultSpace: defaultSpace, project: project))
            results[agent] = .checked(try AgentCheck.decode(output))
        } catch let failure as KnodeCommand.Failure where failure.output.contains("unexpected argument '--check'") {
            results[agent] = .failed(Self.updateKnode)
        } catch let failure as KnodeCommand.Failure where !failure.output.isEmpty {
            results[agent] = .failed(failure.output.trimmingCharacters(in: .whitespacesAndNewlines))
        } catch is DecodingError {
            results[agent] = .failed("Could not read what knode connect --check printed.")
        } catch {
            results[agent] = .failed(error.localizedDescription)
        }
    }

    /// What to do when `knode` is older than `knode connect --check`.
    static let updateKnode = "This knode is too old to check agents. Update it with "
        + "brew upgrade knodemd/tap/knode (or install the latest Knode), then check again."
}
