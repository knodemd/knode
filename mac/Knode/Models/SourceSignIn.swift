import Foundation

/// One step of signing in to a source, shown in the Connect sheet: a few fields to fill in (a
/// phone number, a code, a password, an app password) or a page the person signs in on in the
/// browser (Google, Slack), then a button to go on.
struct SourceSignInStep: Identifiable, Equatable, Sendable {
    /// A field the person fills in.
    struct Field: Identifiable, Equatable, Sendable {
        var id: String
        var label: String
        var placeholder: String = ""
        /// Hidden as it is typed, such as a password.
        var isSecret = false
        /// Monospaced, such as a code or a phone number.
        var isCode = false
        /// May be left empty.
        var isOptional = false
    }

    /// A stable name of the step, such as `phone` or `code`.
    var id: String
    var title: String
    var message: String?
    var fields: [Field] = []
    /// The page the person signs in on. The sheet opens it and waits until the browser comes
    /// back to Knode, or the person cancels.
    var browserURL: URL?
    /// The title of the button that goes on.
    var continueTitle = "Continue"

    /// Whether every field that must be filled in has something in it.
    func isComplete(_ values: [String: String]) -> Bool {
        fields.allSatisfy { $0.isOptional || !(values[$0.id] ?? "").trimmingCharacters(in: .whitespaces).isEmpty }
    }
}

/// How one connector signs in, step by step, before it is connected. The Connect sheet asks it
/// for its first step, shows each step and gives it what the person entered, until it says the
/// person is signed in. What the steps collect goes to Knode Service, which keeps the session in
/// the Keychain: never in the app's settings or a space.
protocol SourceSignIn: Sendable {
    /// The first step, or `nil` when the person is signed in already.
    func start(source: String, space: URL) async throws -> SourceSignInStep?
    /// Gives a step what the person entered and returns the next one, or `nil` once signed in.
    /// Throwing shows the error under the step, which stays to be tried again. A browser step
    /// waits until the person signed in on the page, and stops when its task is cancelled.
    func answer(_ step: SourceSignInStep, with values: [String: String], source: String, space: URL) async throws -> SourceSignInStep?
}

/// The sign-in of each connector: its own one by source id, else the shared one of Knode
/// Service for every connector that reads an account (`sign-in` in its permissions).
struct SourceSignIns: Sendable {
    var flows: [String: any SourceSignIn]
    var fallback: (any SourceSignIn)?

    init(_ flows: [String: any SourceSignIn] = [:], fallback: (any SourceSignIn)? = nil) {
        self.flows = flows
        self.fallback = fallback
    }

    func flow(for source: SourceStatus) -> (any SourceSignIn)? {
        flows[source.id] ?? (source.needsSignIn ? fallback : nil)
    }
}

/// Signing in through Knode Service's shared interface: `sources.account`, then
/// `sources.sign_in` one step at a time, as `knode sources sign-in` does. Each state the service
/// answers becomes a step of the sheet:
///
/// - Telegram: the phone number, the code Telegram sent, then the password when the account has
///   one.
/// - Gmail and Slack: the service's sign-in page, opened in the browser; the sheet waits until
///   the browser comes back to Knode.
/// - Mail (IMAP): the address, the server unless Knode knows it, and an app password, in one step.
struct ServiceSourceSignIn: SourceSignIn {
    let client: any SourcesClient

    /// How a connector's account is named and signed in to.
    enum Style: Equatable {
        case phone, browser, mail, account
    }

    static func style(of source: String) -> Style {
        switch source {
        case "telegram": .phone
        case "gmail", "slack": .browser
        case "imap": .mail
        default: .account
        }
    }

    func start(source: String, space: URL) async throws -> SourceSignInStep? {
        let state = try await client.account(of: source)
        if state.isSignedIn { return nil }
        return try await step(for: state, source: source)
    }

    func answer(_ step: SourceSignInStep, with values: [String: String], source: String, space: URL) async throws -> SourceSignInStep? {
        func value(_ id: String) -> String { (values[id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines) }
        let state: SourceAccountState
        switch step.id {
        case "mail":
            let account = [value("address"), value("server")].filter { !$0.isEmpty }.joined(separator: " ")
            var next = try await client.signIn(source, .start(account: account))
            if case .passwordNeeded = next {
                // As typed: an app password is given as it is.
                next = try await client.signIn(source, .password(values["password"] ?? ""))
            }
            state = next
        case "code":
            state = try await client.signIn(source, .code(value("code")))
        case "password":
            state = try await client.signIn(source, .password(values["password"] ?? ""))
        case "browser":
            state = try await waitForBrowser(source)
        default:
            state = try await client.signIn(source, .start(account: value("account")))
        }
        return try await self.step(for: state, source: source)
    }

    /// Sends `browser` until the service says the person signed in. Each call waits a while for
    /// the browser to come back, then fails asking to finish on the page first: that one is
    /// sent again, any other error ends it.
    private func waitForBrowser(_ source: String) async throws -> SourceAccountState {
        while true {
            try Task.checkCancellation()
            do {
                return try await client.signIn(source, .browser)
            } catch let error as ServiceError where Self.isStillWaiting(error) {
                try Task.checkCancellation()
                try await Task.sleep(for: .milliseconds(200))
            }
        }
    }

    /// Whether the service is still waiting for the person to finish on the sign-in page.
    static func isStillWaiting(_ error: ServiceError) -> Bool {
        guard case .remote(_, let kind, let message, _) = error, kind == "sign-in" else { return false }
        return message.contains("then go on")
    }

    /// The step a state of the account asks for, or `nil` once signed in.
    func step(for state: SourceAccountState, source: String) async throws -> SourceSignInStep? {
        switch state {
        case .signedIn:
            return nil
        case .signedOut:
            return try await firstStep(source)
        case .codeSent(let via):
            return SourceSignInStep(
                id: "code", title: "The code", message: via.map { "Enter the code sent to \($0)." } ?? "Enter the code Telegram sent you.",
                fields: [.init(id: "code", label: "Code", placeholder: "12345", isCode: true)], continueTitle: "Sign in"
            )
        case .passwordNeeded(let hint):
            let mail = Self.style(of: source) == .mail
            let detail: String = switch (mail, hint) {
            case (true, let hint?): "The app password for \(hint)."
            case (true, nil): "The app password of the account."
            case (false, let hint?) where !hint.isEmpty: "This account has a password too. Its hint: \(hint)."
            default: "This account has a password too."
            }
            return SourceSignInStep(
                id: "password", title: mail ? "App password" : "Password", message: detail,
                fields: [.init(id: "password", label: mail ? "App password" : "Password", isSecret: true)], continueTitle: "Sign in"
            )
        case .openBrowser(let url):
            return SourceSignInStep(
                id: "browser", title: "Sign in in your browser",
                message: "Knode opened the sign-in page in your browser. Sign in there and allow Knode to read. This sheet goes on by itself when the browser comes back to Knode.",
                browserURL: url
            )
        }
    }

    /// The first step of a connector that is signed out.
    private func firstStep(_ source: String) async throws -> SourceSignInStep? {
        switch Self.style(of: source) {
        case .browser:
            // The account is chosen on the service's page: ask for the page at once.
            let state = try await client.signIn(source, .start(account: ""))
            if case .signedOut = state { throw ServiceError.badResponse("the sign-in did not start") }
            return try await step(for: state, source: source)
        case .phone:
            return SourceSignInStep(
                id: "phone", title: "Your phone number",
                message: "The phone number of your Telegram account, in international format. Telegram sends you a code.",
                fields: [.init(id: "account", label: "Phone number", placeholder: "+34 600 000 000", isCode: true)]
            )
        case .mail:
            return SourceSignInStep(
                id: "mail", title: "Your mail account",
                message: "Use an app password, made in your account's security settings, not your usual password. Knode connects over TLS only.",
                fields: [
                    .init(id: "address", label: "Address", placeholder: "you@example.com"),
                    .init(id: "server", label: "IMAP server", placeholder: "Not needed for Gmail, iCloud, Outlook, Yahoo or Fastmail", isCode: true, isOptional: true),
                    .init(id: "password", label: "App password", isSecret: true),
                ],
                continueTitle: "Sign in"
            )
        case .account:
            return SourceSignInStep(id: "account", title: "Your account", fields: [.init(id: "account", label: "Account")])
        }
    }
}
