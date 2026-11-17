import Foundation
import Testing
@testable import Knode

/// The permissions screen's model against a fake Knode Service, and `knode connect` through a
/// stand-in command.
@MainActor
@Suite(.serialized) final class AgentsModelTests {
    let service: FakeKnodeService
    let work = URL(filePath: "/tmp/Work.knode", directoryHint: .isDirectory)
    let personal = URL(filePath: "/tmp/Personal.knode", directoryHint: .isDirectory)
    var started = 0

    init() throws {
        service = try FakeKnodeService()
    }

    deinit {
        service.stop()
    }

    func makeModel(socket: URL? = nil, command: KnodeCommand = KnodeCommand(executable: URL(filePath: "/usr/bin/false"))) -> AgentsModel {
        AgentsModel(
            directory: ServiceAgentDirectory(socketURL: socket ?? service.socketURL, person: "Ada"),
            command: { command },
            startService: { [weak self] in self?.started += 1 }
        )
    }

    @Test func listsAgentsWithTheirKeysAndLevels() async throws {
        service.addAgent("codex", levels: [:])
        service.addAgent("claude-code", levels: ["/tmp/Work.knode": "write", "/tmp/Personal.knode": "none"])
        let model = makeModel()
        await model.load()

        #expect(model.state == .loaded)
        #expect(model.agents.map(\.name) == ["claude-code", "codex"])
        let claude = try #require(model.agents.first)
        #expect(claude.validKey?.fingerprint.hasPrefix("sha256:") == true)
        #expect(claude.level(for: work) == .write)
        #expect(claude.level(for: personal) == .none)
        // A space it was never given a level in is no access.
        #expect(claude.level(for: URL(filePath: "/tmp/Other.knode")) == .none)
        #expect(service.requests.first == "agents.list")
    }

    @Test func setsALevelAsThePerson() async throws {
        service.addAgent("claude-code", levels: ["/tmp/Work.knode": "write"])
        let model = makeModel()
        await model.load()
        let claude = try #require(model.agents.first)

        await model.setLevel(.read, for: claude, in: personal)
        let params = try #require(service.params(of: "grants.set"))
        #expect(params["agent"] as? String == "claude-code")
        #expect(params["space"] as? String == "/tmp/Personal.knode")
        #expect(params["level"] as? String == "read")
        #expect((params["by"] as? [String: Any])?["kind"] as? String == "person")
        #expect((params["by"] as? [String: Any])?["actor"] as? String == "Ada")
        // The list is read again after the change.
        #expect(model.agents.first?.level(for: personal) == .read)
        #expect(model.agents.first?.level(for: work) == .write)

        // Setting the level it already has sends nothing.
        let count = service.requests.count
        await model.setLevel(.write, for: try #require(model.agents.first), in: work)
        #expect(service.requests.count == count)
    }

    @Test func revokesAnAgent() async throws {
        service.addAgent("claude-code", levels: ["/tmp/Work.knode": "write"])
        let model = makeModel()
        await model.load()
        await model.revoke(try #require(model.agents.first))

        #expect(service.params(of: "agents.revoke")?["agent"] as? String == "claude-code")
        let claude = try #require(model.agents.first)
        #expect(claude.validKey == nil)
        #expect(claude.level(for: work) == .none)
        #expect(model.errorMessage == nil)
    }

    @Test func offersToStartTheServiceWhenItIsNotRunning() async throws {
        let model = makeModel(socket: URL(filePath: "/tmp/kn-none/service.sock"))
        await model.load()
        #expect(model.state == .serviceStopped)
        #expect(model.agents.isEmpty)

        await model.startServiceAndLoad()
        #expect(started == 1)
        #expect(model.state == .serviceStopped)
    }

    @Test func connectRunsKnodeConnectAndShowsItsOutput() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        // A stand-in for `knode` that prints its arguments, and fails for codex.
        let script = folder.appending(path: "knode")
        try """
        #!/bin/sh
        echo "knode $*"
        [ "$2" = codex ] && { echo "codex is not installed" >&2; exit 1; }
        exit 0
        """.write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path(percentEncoded: false))
        service.addAgent("claude-code", levels: [:])
        let model = makeModel(command: KnodeCommand(executable: script))

        await model.connect(.claudeCode, preview: true)
        #expect(model.connectOutput == AgentsModel.ConnectOutput(agent: .claudeCode, preview: true, text: "knode connect claude-code --print\n", succeeded: true))
        #expect(!service.requests.contains("agents.list"))

        await model.connect(.claudeCode, preview: false)
        #expect(model.connectOutput?.text == "knode connect claude-code\n")
        // The agents are read again after connecting.
        #expect(model.agents.map(\.name) == ["claude-code"])

        await model.connect(.codex, preview: false)
        #expect(model.connectOutput?.succeeded == false)
        #expect(model.connectOutput?.text.contains("codex is not installed") == true)
    }

    @Test func connectableAgentsMatchTheirNames() {
        #expect(ConnectableAgent(agent: "gemini-cli") == .gemini)
        #expect(ConnectableAgent(agent: "claude-code") == .claudeCode)
        #expect(ConnectableAgent(agent: "claude-code-work") == nil)
    }

    @Test func spacesShownAreTheSidebarThenOtherGrants() {
        let agent = AgentInfo(id: "A", name: "claude-code", kind: "claude-code", createdAt: 0, keys: [], spaces: [
            AgentInfo.Grant(space: "/tmp/Work.knode", level: .write),
            AgentInfo.Grant(space: "/tmp/Elsewhere.knode", level: .read),
        ])
        let sidebar = [
            SpaceItem(url: work, name: "Work", icon: "💼", isDefault: true, isMissing: false, isExample: false),
            SpaceItem(url: URL(filePath: "/tmp/Gone.knode"), name: "Gone", icon: nil, isDefault: false, isMissing: true, isExample: false),
            SpaceItem(url: SpacePanels.exampleURL, name: "Example", icon: nil, isDefault: false, isMissing: false, isExample: true),
        ]
        let rows = AgentSpaces.rows(for: agent, sidebar: sidebar)
        #expect(rows.map(\.title) == ["Work", "Elsewhere"])
        #expect(rows.first?.icon == "💼")
    }
}

/// Connect your agents: which agents it shows and in what order, and what connecting them does,
/// against a fake `knode` that keeps a list of agents in memory.
@MainActor
@Suite struct ConnectAgentsModelTests {
    /// A stand-in for `knode connect --list --json` and `knode connect <agent>`.
    @MainActor final class FakeKnode {
        var agents: [AgentOnThisMachine]
        var calls: [[String]] = []
        /// Agents whose `knode connect` succeeds but leaves them unconnected, with what it prints.
        var printsWithoutConnecting: [String: String] = [:]
        var fails: Set<String> = []

        init(_ agents: [AgentOnThisMachine]) { self.agents = agents }

        func run(_ arguments: [String]) async throws -> String {
            calls.append(arguments)
            if arguments.starts(with: ["connect", "--list", "--json"]) {
                let rows = agents.map {
                    #"{"agent": "\#($0.agent)", "name": "\#($0.agent)", "title": "\#($0.title)", "detected": \#($0.detected), "connected": \#($0.connected), "can_connect": \#($0.canConnect)}"#
                }
                return "{\n  \"agents\": [\(rows.joined(separator: ", "))]\n}\n"
            }
            let agent = arguments[1]
            if fails.contains(agent) { throw KnodeCommand.Failure(arguments: arguments, status: 1, output: "Knode Service could not start") }
            if let text = printsWithoutConnecting[agent] { return text }
            if let index = agents.firstIndex(where: { $0.agent == agent }) { agents[index].connected = true }
            return "Added Knode to \(agent)."
        }
    }

    static func agent(_ name: String, _ title: String, detected: Bool = true, connected: Bool = false, canConnect: Bool = true) -> AgentOnThisMachine {
        AgentOnThisMachine(agent: name, title: title, detected: detected, connected: connected, canConnect: canConnect)
    }

    /// In the order the core lists them.
    static let everyAgent = [
        agent("claude-code", "Claude Code", detected: false),
        agent("codex", "Codex"),
        agent("gemini", "Gemini CLI", detected: false),
        agent("cursor", "Cursor"),
        agent("zed", "Zed", detected: false, connected: true),
        agent("aider", "Aider", canConnect: false),
    ]

    func model(_ fake: FakeKnode) -> ConnectAgentsModel {
        ConnectAgentsModel { try await fake.run($0) }
    }

    @Test func showsAgentsOnThisMachineOrConnectedClaudeCodeFirst() {
        #expect(AgentOnThisMachine.shown(Self.everyAgent).map(\.agent) == ["codex", "cursor", "zed"])

        let found = [Self.agent("cursor", "Cursor"), Self.agent("codex", "Codex"), Self.agent("claude-code", "Claude Code")]
        #expect(AgentOnThisMachine.shown(found).map(\.agent) == ["claude-code", "codex", "cursor"])
        #expect(AgentOnThisMachine.shown([Self.agent("aider", "Aider", canConnect: false)]).isEmpty)
    }

    @Test func readsTheListTheCorePrints() async throws {
        let fake = FakeKnode(Self.everyAgent)
        let model = model(fake)
        await model.load()

        #expect(model.state == .loaded)
        #expect(model.agents.map(\.agent) == ["codex", "cursor", "zed"])
        #expect(model.agents.last?.connected == true)
        #expect(model.detectedNotConnected.map(\.agent) == ["codex", "cursor"])
        #expect(fake.calls == [["connect", "--list", "--json"]])

        let warned = try AgentOnThisMachine.decodeList("warning: something\n{\"agents\": []}\n")
        #expect(warned.isEmpty)
    }

    @Test func aKnodeWithoutTheListSaysWhy() async {
        let model = ConnectAgentsModel { arguments in
            throw KnodeCommand.Failure(arguments: arguments, status: 2, output: "error: unexpected argument '--json' found\n")
        }
        await model.load()
        #expect(model.state == .failed("error: unexpected argument '--json' found"))
    }

    @Test func connectingOneSaysToOpenANewSession() async {
        let fake = FakeKnode(Self.everyAgent)
        let model = model(fake)
        await model.load()
        let codex = model.agents[0]
        await model.connect(codex, space: nil, defaultSpace: nil)

        #expect(fake.calls.contains(["connect", "codex"]))
        #expect(model.agents.first { $0.agent == "codex" }?.connected == true)
        #expect(model.connectedNow == ["codex"])
        #expect(model.outcomes["codex"] == nil)
        #expect(ConnectAgentsModel.nextStep(for: "Codex") == "Open a new Codex session: it will set up your memory.")
    }

    @Test func connectAllDetectedSkipsConnectedAndNotFound() async {
        let fake = FakeKnode(Self.everyAgent + [Self.agent("claude-code-desktop", "Elsewhere", detected: false)])
        let model = model(fake)
        await model.load()
        await model.connectAllDetected(space: nil, defaultSpace: nil)

        let connects = fake.calls.filter { $0.count == 2 }
        #expect(connects == [["connect", "codex"], ["connect", "cursor"]])
        #expect(model.connectedNow == ["codex", "cursor"])
        #expect(model.detectedNotConnected.isEmpty)
    }

    @Test func theCoreDecidesWhetherItWorked() async {
        let fake = FakeKnode([Self.agent("claude-code", "Claude Code"), Self.agent("cursor", "Cursor")])
        fake.printsWithoutConnecting["claude-code"] = "Claude Code (`claude`) is not on your PATH.\n"
        fake.fails = ["cursor"]
        let model = model(fake)
        await model.load()
        await model.connectAllDetected(space: nil, defaultSpace: nil)

        #expect(model.outcomes["claude-code"] == .notConnected("Claude Code (`claude`) is not on your PATH."))
        #expect(model.outcomes["cursor"] == .failed("Knode Service could not start"))
        #expect(model.connectedNow.isEmpty)
        #expect(!model.isBusy)
    }

    @Test func onlyAnotherSpaceThanTheDefaultIsNamed() {
        let work = URL(filePath: "/tmp/Work.knode", directoryHint: .isDirectory)
        let home = URL(filePath: "/tmp/Home.knode", directoryHint: .isDirectory)
        #expect(ConnectAgentsModel.arguments(connecting: "codex", to: work, defaultSpace: work) == ["connect", "codex"])
        #expect(ConnectAgentsModel.arguments(connecting: "codex", to: nil, defaultSpace: work) == ["connect", "codex"])
        #expect(ConnectAgentsModel.arguments(connecting: "codex", to: home, defaultSpace: work) == ["connect", "codex", "--space", "/tmp/Home.knode/"])
        #expect(ConnectAgentsModel.arguments(connecting: "codex", to: home, defaultSpace: nil) == ["connect", "codex", "--space", "/tmp/Home.knode/"])
    }
}
