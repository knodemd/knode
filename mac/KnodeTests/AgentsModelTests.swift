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
