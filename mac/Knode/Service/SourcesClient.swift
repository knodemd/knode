import Foundation

/// What a source is doing in a space, as `sources.list` says (`state`).
public enum SourceState: String, Sendable, Hashable, Decodable {
    /// An importer: it runs when the person imports something.
    case importer
    /// Not connected to the space.
    case off
    case paused
    /// Connected, but this Mac has no such connector.
    case unavailable
    /// Connected, but `sources` or `capture` is off in `config.toml`.
    case stopped
    case running
    case idle
    /// Its last run failed; it tries again later.
    case failing
    /// The other app's database changed: nothing is written until Knode is updated.
    case needsUpdate = "needs-update"
    /// A permission or a sign-in is missing.
    case needsAttention = "needs-attention"
    /// A state this version of the app does not know.
    case unknown

    public init(from decoder: any Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = SourceState(rawValue: raw) ?? .unknown
    }
}

/// The history a connector brings in, chosen once when it is connected.
public enum SourceHistory: String, Sendable, Hashable, CaseIterable, Identifiable, Decodable {
    /// From the start of the day it was connected: the default.
    case fromToday = "from-today"
    /// Everything the source has.
    case everything

    public var id: Self { self }
}

/// What a source needs to read, as `permissions` lists it.
public struct SourcePermission: RawRepresentable, Sendable, Hashable, Decodable {
    public var rawValue: String

    public init(rawValue: String) { self.rawValue = rawValue }

    public init(from decoder: any Decoder) throws {
        rawValue = try decoder.singleValueContainer().decode(String.self)
    }

    public static let fullDiskAccess = SourcePermission(rawValue: "full-disk-access")
    public static let signIn = SourcePermission(rawValue: "sign-in")
    public static let network = SourcePermission(rawValue: "network")
    public static let calendar = SourcePermission(rawValue: "calendar")
    public static let contacts = SourcePermission(rawValue: "contacts")
    public static let microphone = SourcePermission(rawValue: "microphone")
    public static let photos = SourcePermission(rawValue: "photos")
}

/// A chat, channel or folder of a source.
public struct SourceContainer: Identifiable, Sendable, Hashable, Decodable {
    public var id: String
    public var name: String
    /// Whether the person excluded it: `sources.containers` says; excluded ones in a setting are.
    public var excluded: Bool

    public init(id: String, name: String, excluded: Bool = false) {
        self.id = id
        self.name = name
        self.excluded = excluded
    }

    enum CodingKeys: String, CodingKey { case id, name, excluded }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? id
        excluded = try container.decodeIfPresent(Bool.self, forKey: .excluded) ?? true
    }
}

/// Where signing in to the account a connector reads is, on this Mac: `sources.account` and
/// each `sources.sign_in` answer it.
public enum SourceAccountState: Sendable, Hashable, Decodable {
    /// Not signed in: send `start`.
    case signedOut
    /// The service sent a code (Telegram): send `code`. `via` says where, for people.
    case codeSent(via: String?)
    /// The account has a password, or a mail account needs its app password: send `password`.
    case passwordNeeded(hint: String?)
    /// The person signs in in the browser, on this page (Google, Slack): then send `browser`.
    case openBrowser(url: URL)
    /// Signed in, as `account`; the session is in the Keychain.
    case signedIn(account: String)

    enum CodingKeys: String, CodingKey { case state, via, hint, url, account }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .state) {
        case "signed-out": self = .signedOut
        case "code-sent": self = .codeSent(via: try c.decodeIfPresent(String.self, forKey: .via))
        case "password-needed": self = .passwordNeeded(hint: try c.decodeIfPresent(String.self, forKey: .hint))
        case "open-browser":
            let text = try c.decode(String.self, forKey: .url)
            guard let url = URL(string: text) else {
                throw DecodingError.dataCorruptedError(forKey: .url, in: c, debugDescription: "not a URL: \(text)")
            }
            self = .openBrowser(url: url)
        case "signed-in": self = .signedIn(account: try c.decodeIfPresent(String.self, forKey: .account) ?? "")
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .state, in: c, debugDescription: "unknown sign-in state \(other)")
        }
    }

    public var isSignedIn: Bool {
        if case .signedIn = self { return true }
        return false
    }
}

/// One step of signing in, sent by the person: `sources.sign_in`'s `step`. Never logged.
public enum SourceSignInRequest: Sendable, Hashable {
    /// The account: a phone number, or a mail address with its server; empty when it is chosen on
    /// the service's page (Slack, Gmail).
    case start(account: String)
    case code(String)
    case password(String)
    /// The person finished signing in on the page `openBrowser` gave.
    case browser

    var json: JSONValue {
        switch self {
        case .start(let account): ["step": "start", "account": .string(account)]
        case .code(let code): ["step": "code", "code": .string(code)]
        case .password(let password): ["step": "password", "password": .string(password)]
        case .browser: ["step": "browser"]
        }
    }
}

/// A source of a space and what it did on this Mac, as `sources.list` returns it.
public struct SourceStatus: Identifiable, Sendable, Hashable, Decodable {
    /// The person's settings of a connected connector, kept in the space.
    public struct Setting: Sendable, Hashable, Decodable {
        public var on: Bool
        public var history: SourceHistory
        public var connectedAt: Int64
        /// With `fromToday`, nothing older is brought in.
        public var since: Int64
        public var exclude: [SourceContainer]

        public init(on: Bool, history: SourceHistory, connectedAt: Int64, since: Int64, exclude: [SourceContainer] = []) {
            self.on = on
            self.history = history
            self.connectedAt = connectedAt
            self.since = since
            self.exclude = exclude
        }

        enum CodingKeys: String, CodingKey { case on, history, connectedAt, since, exclude }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            on = try container.decode(Bool.self, forKey: .on)
            history = try container.decode(SourceHistory.self, forKey: .history)
            connectedAt = try container.decode(Int64.self, forKey: .connectedAt)
            since = try container.decode(Int64.self, forKey: .since)
            exclude = try container.decodeIfPresent([SourceContainer].self, forKey: .exclude) ?? []
        }
    }

    /// The error of the last run.
    public struct LastError: Sendable, Hashable, Decodable {
        /// `needs-update`, `unavailable`, `permission`, `sign-in`, `failed` or `space`.
        public var kind: String
        public var message: String
        public var at: Int64

        public init(kind: String, message: String, at: Int64) {
            self.kind = kind
            self.message = message
            self.at = at
        }
    }

    public var id: String
    public var name: String
    /// `connector` or `importer`.
    public var kind: String
    /// What it reads, in a sentence.
    public var reads: String
    public var permissions: [SourcePermission]
    /// What it calls the things it reads: `chat`, `channel`, `folder`.
    public var container: String?
    public var state: SourceState
    public var setting: Setting?
    public var lastRun: Int64?
    public var lastSuccess: Int64?
    public var lastItems: Int
    public var items: Int64
    public var lastError: LastError?
    public var nextRun: Int64?

    public init(
        id: String, name: String, kind: String, reads: String, permissions: [SourcePermission] = [], container: String? = nil,
        state: SourceState, setting: Setting? = nil, lastRun: Int64? = nil, lastSuccess: Int64? = nil, lastItems: Int = 0,
        items: Int64 = 0, lastError: LastError? = nil, nextRun: Int64? = nil
    ) {
        self.id = id
        self.name = name
        self.kind = kind
        self.reads = reads
        self.permissions = permissions
        self.container = container
        self.state = state
        self.setting = setting
        self.lastRun = lastRun
        self.lastSuccess = lastSuccess
        self.lastItems = lastItems
        self.items = items
        self.lastError = lastError
        self.nextRun = nextRun
    }

    enum CodingKeys: String, CodingKey {
        case id, name, kind, reads, permissions, container, state, setting, lastRun, lastSuccess, lastItems, items, lastError, nextRun
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? id
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? "connector"
        reads = try c.decodeIfPresent(String.self, forKey: .reads) ?? ""
        permissions = try c.decodeIfPresent([SourcePermission].self, forKey: .permissions) ?? []
        container = try c.decodeIfPresent(String.self, forKey: .container)
        state = try c.decodeIfPresent(SourceState.self, forKey: .state) ?? .unknown
        setting = try c.decodeIfPresent(Setting.self, forKey: .setting)
        lastRun = try c.decodeIfPresent(Int64.self, forKey: .lastRun)
        lastSuccess = try c.decodeIfPresent(Int64.self, forKey: .lastSuccess)
        lastItems = try c.decodeIfPresent(Int.self, forKey: .lastItems) ?? 0
        items = try c.decodeIfPresent(Int64.self, forKey: .items) ?? 0
        lastError = try c.decodeIfPresent(LastError.self, forKey: .lastError)
        nextRun = try c.decodeIfPresent(Int64.self, forKey: .nextRun)
    }

    public var isConnector: Bool { kind != "importer" }
    /// Connected to the space: it has settings there.
    public var isConnected: Bool { setting != nil }
    public var isOn: Bool { setting?.on ?? false }
    public var needsFullDiskAccess: Bool { permissions.contains(.fullDiskAccess) }
    public var needsSignIn: Bool { permissions.contains(.signIn) }
}

/// The sources of a space, managed as the person who owns the Knode home: Settings > Sources.
public protocol SourcesClient: Sendable {
    /// `sources.list`: every source of the space, connectors then importers.
    func list(in space: URL) async throws -> [SourceStatus]
    /// `sources.containers`: the chats, channels or folders a connected connector has now.
    func containers(of source: String, in space: URL) async throws -> [SourceContainer]
    /// `sources.connect`: connects it with a history, turned on, or with `on` false paused, so
    /// what it leaves out can be chosen before it reads anything.
    func connect(_ source: String, history: SourceHistory, on: Bool, in space: URL) async throws -> SourceStatus
    /// `sources.pause` (`on` false) or `sources.resume`.
    func setOn(_ on: Bool, _ source: String, in space: URL) async throws -> SourceStatus
    /// `sources.exclude`: the container is never read again; what came from it stays.
    func exclude(_ container: SourceContainer, from source: String, in space: URL) async throws -> SourceStatus
    /// `sources.include`: reads the container again from now on.
    func include(_ container: String, in source: String, in space: URL) async throws -> SourceStatus
    /// `sources.run`: runs it now, without waiting for it to finish.
    func run(_ source: String, in space: URL) async throws -> SourceStatus
    /// `sources.disconnect`: removes its settings and secrets; what it brought in stays.
    func disconnect(_ source: String, in space: URL) async throws
    /// `sources.account`: whether the connector is signed in to its account on this Mac.
    func account(of source: String) async throws -> SourceAccountState
    /// `sources.sign_in`: one step of signing in, and where it leads.
    func signIn(_ source: String, _ step: SourceSignInRequest) async throws -> SourceAccountState
    /// `sources.sign_out`: ends the session and forgets it. Whether it was signed in.
    @discardableResult func signOut(_ source: String) async throws -> Bool
}

/// The `SourcesClient` of Knode Service, over its socket as the person.
public struct ServiceSourcesClient: SourcesClient {
    public var socketURL: URL
    public var person: String

    public init(socketURL: URL = KnodeHome.current.socketURL, person: String = CoreSpaceStore.currentUserName) {
        self.socketURL = socketURL
        self.person = person
    }

    private var by: JSONValue { ["kind": "person", "actor": .string(person)] }

    private func params(_ space: URL, _ source: String, _ extra: [String: JSONValue] = [:], by: Bool = false) -> [String: JSONValue] {
        var params: [String: JSONValue] = ["space": .string(ServiceSpaceStore.path(of: space)), "source": .string(source)]
        if by { params["by"] = self.by }
        return params.merging(extra) { $1 }
    }

    public func list(in space: URL) async throws -> [SourceStatus] {
        try await call("sources.list", ["space": .string(ServiceSpaceStore.path(of: space))], as: [SourceStatus].self)
    }

    public func containers(of source: String, in space: URL) async throws -> [SourceContainer] {
        try await call("sources.containers", params(space, source), as: [SourceContainer].self)
    }

    public func connect(_ source: String, history: SourceHistory, on: Bool, in space: URL) async throws -> SourceStatus {
        let extra: [String: JSONValue] = ["history": .string(history.rawValue), "on": .bool(on)]
        return try await call("sources.connect", params(space, source, extra, by: true), as: SourceStatus.self)
    }

    public func setOn(_ on: Bool, _ source: String, in space: URL) async throws -> SourceStatus {
        try await call(on ? "sources.resume" : "sources.pause", params(space, source, by: true), as: SourceStatus.self)
    }

    public func exclude(_ container: SourceContainer, from source: String, in space: URL) async throws -> SourceStatus {
        let extra: [String: JSONValue] = ["id": .string(container.id), "name": .string(container.name)]
        return try await call("sources.exclude", params(space, source, extra, by: true), as: SourceStatus.self)
    }

    public func include(_ container: String, in source: String, in space: URL) async throws -> SourceStatus {
        try await call("sources.include", params(space, source, ["id": .string(container)], by: true), as: SourceStatus.self)
    }

    public func run(_ source: String, in space: URL) async throws -> SourceStatus {
        try await call("sources.run", params(space, source), as: SourceStatus.self)
    }

    public func disconnect(_ source: String, in space: URL) async throws {
        _ = try await call("sources.disconnect", params(space, source, by: true), as: Bool.self)
    }

    public func account(of source: String) async throws -> SourceAccountState {
        try await call("sources.account", ["source": .string(source)], as: SourceAccountState.self)
    }

    public func signIn(_ source: String, _ step: SourceSignInRequest) async throws -> SourceAccountState {
        try await call("sources.sign_in", ["source": .string(source), "step": step.json], as: SourceAccountState.self)
    }

    @discardableResult public func signOut(_ source: String) async throws -> Bool {
        try await call("sources.sign_out", ["source": .string(source)], as: Bool.self)
    }

    /// One short connection per call, like the Agents settings.
    private func call<Result: Decodable & Sendable>(_ method: String, _ params: [String: JSONValue], as type: Result.Type) async throws -> Result {
        let connection = try ServiceConnection.connect(to: socketURL)
        defer { connection.close() }
        return try await connection.call(method, params, as: type)
    }
}
