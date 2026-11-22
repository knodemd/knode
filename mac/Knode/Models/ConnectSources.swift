import Foundation

/// How the Connect sheet notices that Full Disk Access was turned on, without the person clicking
/// anything: it asks Knode Service, which is what reads the source, and looks at whether this app
/// has access, which is what the person turns on in System Settings.
struct SourceAccessCheck: Sendable {
    /// What Knode Service says now about the source: its `readiness` in `sources.list`, or `nil`
    /// when it cannot tell (an older service, or a connector that cannot check cheaply).
    var service: @Sendable (_ source: String, _ space: URL) async -> SourceReadiness?
    /// Whether this app can read a place only Full Disk Access opens.
    var app: @Sendable () -> Bool
    /// Restarts Knode Service, so it starts with the access granted.
    var restartService: @MainActor () async -> Void
    var interval: Duration = .seconds(1)

    enum Decision: Equatable {
        /// Knode can read the source: go on.
        case granted
        /// This app has access but the service does not see it yet: restart it, once.
        case restartService
        /// Not yet.
        case wait
    }

    /// What to do after one check.
    static func decide(service: SourceReadiness?, appHasAccess: Bool, restarted: Bool) -> Decision {
        switch service {
        case .some(let readiness) where readiness.lacksFullDiskAccess:
            return appHasAccess && !restarted ? .restartService : .wait
        case .some:
            // Ready, or blocked by something access does not change (no Mail account yet): the
            // next steps say so.
            return .granted
        case nil:
            // The service cannot tell: trust what this app sees.
            return appHasAccess ? .granted : .wait
        }
    }

    /// The check of the app: Knode Service through `client`, this app with `probe`.
    static func live(client: any SourcesClient, probe: FullDiskAccessProbe = .live,
                     restartService: @escaping @MainActor () async -> Void) -> SourceAccessCheck {
        SourceAccessCheck(
            service: { source, space in
                guard let list = try? await client.list(in: space) else { return nil }
                return list.first { $0.id == source }?.readiness
            },
            app: { probe.hasAccess() },
            restartService: restartService
        )
    }
}

/// Whether this app has Full Disk Access: it tries to list folders of the home folder that macOS
/// opens only with it. Quiet: macOS shows no prompt for Full Disk Access.
struct FullDiskAccessProbe: Sendable {
    var home: URL
    var exists: @Sendable (URL) -> Bool
    var list: @Sendable (URL) throws -> [String]

    /// Folders only Full Disk Access opens, on every Mac or once the app is used.
    static let protectedFolders = ["Library/Safari", "Library/Mail", "Library/Messages", "Library/Containers/com.apple.stocks"]

    static let live = FullDiskAccessProbe(
        home: FileManager.default.homeDirectoryForCurrentUser,
        exists: { FileManager.default.fileExists(atPath: $0.path(percentEncoded: false)) },
        list: { try FileManager.default.contentsOfDirectory(atPath: $0.path(percentEncoded: false)) }
    )

    /// Whether one of the protected folders that exist can be listed.
    func hasAccess() -> Bool {
        Self.protectedFolders
            .map { home.appending(path: $0, directoryHint: .isDirectory) }
            .filter(exists)
            .contains { (try? list($0)) != nil }
    }
}

/// A guided way to connect a source: what its sign-in is filled in with, and a short guide shown
/// above it, such as Gmail through IMAP with an app password.
struct SourceConnectPreset: Equatable, Sendable {
    /// The sign-in step it fills in (`mail` for IMAP).
    var stepID: String
    var values: [String: String]
    var title: String
    var steps: [String]
    var linkTitle: String
    var link: URL

    /// Gmail over IMAP, signed in with an app password Google makes.
    static let gmailAppPassword = SourceConnectPreset(
        stepID: "mail",
        values: ["server": "imap.gmail.com"],
        title: "Gmail with an app password",
        steps: [
            "Open App passwords in your Google account. It needs 2-Step Verification turned on.",
            "Name it Knode and create it. Google shows a 16-letter password once.",
            "Enter your Gmail address and paste that password here. Knode reads over TLS and changes nothing.",
        ],
        linkTitle: "Open App passwords",
        link: URL(string: "https://myaccount.google.com/apppasswords")!
    )
}

/// What a source's tile shows in Connect your things.
enum SourceTileState: Equatable {
    /// Not connected: one click connects it.
    case connect
    /// Connected and reading.
    case connected
    /// Connected or not, Knode cannot read it until access is given.
    case needsAccess
    /// Gmail in a build without Sign in with Google: other ways to read it.
    case otherWays
    /// This build cannot use it.
    case comingSoon(String)

    var label: String {
        switch self {
        case .connect, .otherWays: "Connect"
        case .connected: "Connected"
        case .needsAccess: "Needs access"
        case .comingSoon: "Coming soon"
        }
    }

    var isEnabled: Bool {
        if case .comingSoon = self { return false }
        return true
    }
}

/// The logic of Connect your things, apart from its views: which sources show, their state, and
/// how Gmail is connected.
enum ConnectSources {
    /// The connectors to show: every one `sources.list` gives that this Mac has, ready ones first
    /// and the ones this build cannot use last, keeping the service's order otherwise.
    static func tiles(_ sources: [SourceStatus]) -> [SourceStatus] {
        let shown = sources.filter { $0.isConnector && $0.state != .unavailable }
        let usable = shown.filter { $0.unsupportedReason == nil || $0.id == gmail }
        let soon = shown.filter { $0.unsupportedReason != nil && $0.id != gmail }
        return usable + soon
    }

    static func state(of source: SourceStatus) -> SourceTileState {
        if source.isConnected {
            switch source.state {
            case .needsAttention: return .needsAccess
            default: return .connected
            }
        }
        if let reason = source.unsupportedReason {
            return source.id == gmail ? .otherWays : .comingSoon(reason)
        }
        if source.readiness?.lacksFullDiskAccess == true { return .needsAccess }
        return .connect
    }

    static let gmail = "gmail"
    static let appleMail = "apple-mail"
    static let imap = "imap"

    /// How Apple Mail can stand in for Gmail.
    enum AppleMailOption: Equatable {
        /// Connect Apple Mail: it has an account, or Knode cannot tell until it has access.
        case connect
        /// Mail has no account yet: open it and add the Gmail account first.
        case addAccount
    }

    /// The ways to read Gmail when this build has no Sign in with Google, in the order to offer
    /// them: Apple Mail, then IMAP with an app password.
    struct GmailWays: Equatable {
        var appleMail: AppleMailOption?
        var appPassword: Bool
    }

    /// `nil` when Gmail signs in with Google in this build; the other ways otherwise.
    static func gmailWays(in sources: [SourceStatus]) -> GmailWays? {
        guard let gmail = sources.first(where: { $0.id == gmail }), gmail.unsupportedReason != nil else { return nil }
        let mail = sources.first { $0.id == appleMail && $0.state != .unavailable }
        let appleMail: AppleMailOption? = mail.map { mail in
            if case .blocked(kind: "unavailable", _) = mail.readiness { return .addAccount }
            return .connect
        }
        let imap = sources.contains { $0.id == imap && $0.unsupportedReason == nil && $0.state != .unavailable }
        return GmailWays(appleMail: appleMail, appPassword: imap)
    }

    /// The space Connect your things starts on: the default space, else the open one, else the
    /// first.
    static func initialSpace(_ spaces: [SpaceItem], selected: URL?) -> URL? {
        spaces.first(where: \.isDefault)?.url ?? selected.flatMap { url in spaces.first { $0.url == url }?.url } ?? spaces.first?.url
    }
}
