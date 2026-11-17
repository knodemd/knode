import Foundation

/// One step of signing in to a source, shown in the Connect sheet: a few fields to fill in (a
/// phone number, a code, a password, an app password) or a page to open in the browser (Google
/// or Slack), then a button to go on.
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
    }

    /// A stable name of the step, such as `phone` or `code`.
    var id: String
    var title: String
    var message: String?
    var fields: [Field] = []
    /// A page to open in the browser before going on, such as a sign-in page.
    var browserURL: URL?
    /// The title of the button that goes on.
    var continueTitle = "Continue"

    /// Whether every field has something in it.
    func isComplete(_ values: [String: String]) -> Bool {
        fields.allSatisfy { !(values[$0.id] ?? "").trimmingCharacters(in: .whitespaces).isEmpty }
    }
}

/// How one connector signs in, step by step, before it is connected. Each connector that needs a
/// sign-in (`sign-in` in its permissions) adds one to `SourceSignIns`; the Connect sheet asks it
/// for its first step, shows each step and gives it what the person entered, until it says the
/// person is signed in. What the steps collect goes to Knode Service, which keeps the secrets in
/// the Keychain: never in the app's settings or a space.
protocol SourceSignIn: Sendable {
    /// The first step, or `nil` when the person is signed in already.
    func start(source: String, space: URL) async throws -> SourceSignInStep?
    /// Gives a step what the person entered and returns the next one, or `nil` once signed in.
    /// Throwing shows the error under the step, which stays to be tried again.
    func answer(_ step: SourceSignInStep, with values: [String: String], source: String, space: URL) async throws -> SourceSignInStep?
}

/// The sign-in of each connector that has one, by source id.
struct SourceSignIns: Sendable {
    var flows: [String: any SourceSignIn]

    init(_ flows: [String: any SourceSignIn] = [:]) {
        self.flows = flows
    }

    /// The connectors of this build. Telegram, Gmail, Slack and mail over IMAP add theirs here.
    static let builtIn = SourceSignIns()

    func flow(for source: String) -> (any SourceSignIn)? {
        flows[source]
    }
}
