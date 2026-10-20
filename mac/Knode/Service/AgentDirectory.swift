import Foundation

/// What an agent may do in one space (section 16 of the .knode specification). `write` includes
/// `read`; `none` makes the space invisible to the agent.
public enum GrantLevel: String, CaseIterable, Identifiable, Sendable, Decodable {
    case none, read, write

    public var id: Self { self }

    public var title: String {
        switch self {
        case .none: "No Access"
        case .read: "Read"
        case .write: "Read and Write"
        }
    }

    /// What the level means, in a sentence.
    public var explanation: String {
        switch self {
        case .none: "The space is invisible to it."
        case .read: "It can read and search the space, but not change it."
        case .write: "It can read, write and undo its own changes."
        }
    }
}

/// An agent of this machine, as `agents.list` returns it: its keys by fingerprint, and the level
/// it has in each space it was given one in.
public struct AgentInfo: Identifiable, Hashable, Sendable, Decodable {
    public struct Key: Hashable, Sendable, Decodable {
        /// `sha256:` and 64 hexadecimal digits. The key itself never leaves the credential store.
        public var fingerprint: String
        public var createdAt: Int64
        public var revokedAt: Int64?
    }

    public struct Grant: Hashable, Sendable, Decodable {
        /// The path of the space, as Knode Service opened it.
        public var space: String
        /// `nil` when the space cannot be opened now.
        public var level: GrantLevel?
    }

    public var id: String
    /// The agent's name, such as `claude-code`: the actor of its changes.
    public var name: String
    /// The program, such as `claude-code`.
    public var kind: String
    public var createdAt: Int64
    public var keys: [Key]
    public var spaces: [Grant]

    public init(id: String, name: String, kind: String, createdAt: Int64, keys: [Key], spaces: [Grant]) {
        self.id = id
        self.name = name
        self.kind = kind
        self.createdAt = createdAt
        self.keys = keys
        self.spaces = spaces
    }

    /// The key the agent connects with now, if it has one that is not revoked.
    public var validKey: Key? {
        keys.last { $0.revokedAt == nil }
    }

    public var created: Date { Date(unixMilliseconds: createdAt) }

    /// The grant for the space at `url`, matching paths after resolving symbolic links.
    public func grant(for url: URL) -> Grant? {
        let path = AgentInfo.canonicalPath(url)
        return spaces.first { AgentInfo.canonicalPath(URL(filePath: $0.space, directoryHint: .isDirectory)) == path }
    }

    /// The level in the space at `url`: `none` when it was never given one.
    public func level(for url: URL) -> GrantLevel {
        grant(for: url)?.level ?? .none
    }

    static func canonicalPath(_ url: URL) -> String {
        ServiceSpaceStore.path(of: url.resolvingSymlinksInPath())
    }
}

/// The agents of this machine and their grants, managed as the person who owns the Knode home.
public protocol AgentDirectory: Sendable {
    func agents() async throws -> [AgentInfo]
    /// Sets the level of an agent in a space.
    func setLevel(_ level: GrantLevel, for agent: String, in space: URL) async throws
    /// Revokes every key of the agent and takes away its access to every space.
    func revoke(_ agent: String) async throws
}

/// The `AgentDirectory` of Knode Service: `agents.list`, `grants.set` and `agents.revoke` over its
/// socket, authenticated with the owner token (only a person may manage agents).
public struct ServiceAgentDirectory: AgentDirectory {
    public var socketURL: URL
    public var person: String

    public init(socketURL: URL = KnodeHome.current.socketURL, person: String = CoreSpaceStore.currentUserName) {
        self.socketURL = socketURL
        self.person = person
    }

    private var by: JSONValue { ["kind": "person", "actor": .string(person)] }

    public func agents() async throws -> [AgentInfo] {
        try await call("agents.list", [:], as: [AgentInfo].self)
    }

    public func setLevel(_ level: GrantLevel, for agent: String, in space: URL) async throws {
        let params: [String: JSONValue] = [
            "agent": .string(agent), "space": .string(ServiceSpaceStore.path(of: space)), "level": .string(level.rawValue), "by": by,
        ]
        _ = try await call("grants.set", params, as: GrantAnswer.self)
    }

    public func revoke(_ agent: String) async throws {
        _ = try await call("agents.revoke", ["agent": .string(agent), "by": by], as: AgentInfo.self)
    }

    private struct GrantAnswer: Decodable, Sendable {
        var level: GrantLevel
    }

    /// One short connection per call: managing agents is rare, and each call stands alone.
    private func call<Result: Decodable & Sendable>(_ method: String, _ params: [String: JSONValue], as type: Result.Type) async throws -> Result {
        let connection = try ServiceConnection.connect(to: socketURL)
        defer { connection.close() }
        return try await connection.call(method, params, as: type)
    }
}
