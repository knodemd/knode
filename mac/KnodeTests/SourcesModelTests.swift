import Foundation
import Synchronization
import Testing
@testable import Knode

/// A stand-in for the sources of Knode Service: connectors and importers in memory, and every
/// call it was asked for. Nothing touches a real Knode home or another app's data.
final class FakeSourcesClient: SourcesClient {
    struct State {
        var sources: [SourceStatus] = []
        var containers: [String: [SourceContainer]] = [:]
        var calls: [String] = []
        var failing: [String: ServiceError] = [:]
        /// The account of each connector that reads one, as `sources.account` answers.
        var accounts: [String: SourceAccountState] = [:]
        /// How many `browser` steps still answer that the person has not finished on the page.
        var browserWaits: [String: Int] = [:]
        /// What a mail account was started with, until its app password comes.
        var mailAccount: String?
    }

    let state = Mutex(State())

    init(_ sources: [SourceStatus] = []) {
        state.withLock { $0.sources = sources }
    }

    var calls: [String] { state.withLock { $0.calls } }
    /// The calls that change something or ask a connector, without the reads of the list.
    var actions: [String] { calls.filter { !$0.hasPrefix("list ") } }
    var sources: [SourceStatus] { state.withLock { $0.sources } }

    func source(_ id: String) -> SourceStatus? { sources.first { $0.id == id } }

    func setContainers(_ containers: [SourceContainer], for source: String) {
        state.withLock { $0.containers[source] = containers }
    }

    /// Makes `method` fail with `error`.
    func fail(_ method: String, with error: ServiceError?) {
        state.withLock { $0.failing[method] = error }
    }

    /// Changes a source as the service would after a run.
    func update(_ id: String, _ change: (inout SourceStatus) -> Void) {
        state.withLock { state in
            if let index = state.sources.firstIndex(where: { $0.id == id }) { change(&state.sources[index]) }
        }
    }

    private func record(_ call: String, _ method: String) throws {
        try state.withLock { state in
            state.calls.append(call)
            if let error = state.failing[method] { throw error }
        }
    }

    private func change(_ id: String, _ work: (inout SourceStatus) throws -> Void) throws -> SourceStatus {
        try state.withLock { state in
            guard let index = state.sources.firstIndex(where: { $0.id == id }) else {
                throw ServiceError.remote(code: -32004, kind: "invalid", message: "no source \(id)")
            }
            try work(&state.sources[index])
            return state.sources[index]
        }
    }

    func list(in space: URL) async throws -> [SourceStatus] {
        try record("list \(space.lastPathComponent)", "list")
        return sources
    }

    func containers(of source: String, in space: URL) async throws -> [SourceContainer] {
        try record("containers \(source)", "containers")
        return state.withLock { state in
            let excluded = Set(state.sources.first { $0.id == source }?.setting?.exclude.map(\.id) ?? [])
            return (state.containers[source] ?? []).map { SourceContainer(id: $0.id, name: $0.name, excluded: excluded.contains($0.id)) }
        }
    }

    func connect(_ source: String, history: SourceHistory, on: Bool, in space: URL) async throws -> SourceStatus {
        try record("connect \(source) \(history.rawValue)\(on ? "" : " paused")", "connect")
        return try change(source) { status in
            status.setting = SourceStatus.Setting(on: on, history: history, connectedAt: 1_763_372_400_000, since: 1_763_337_600_000)
            status.state = on ? .idle : .paused
        }
    }

    func setOn(_ on: Bool, _ source: String, in space: URL) async throws -> SourceStatus {
        try record("\(on ? "resume" : "pause") \(source)", on ? "resume" : "pause")
        return try change(source) { status in
            status.setting?.on = on
            status.state = on ? .idle : .paused
        }
    }

    func exclude(_ container: SourceContainer, from source: String, in space: URL) async throws -> SourceStatus {
        try record("exclude \(source) \(container.id)", "exclude")
        return try change(source) { $0.setting?.exclude.append(SourceContainer(id: container.id, name: container.name, excluded: true)) }
    }

    func include(_ container: String, in source: String, in space: URL) async throws -> SourceStatus {
        try record("include \(source) \(container)", "include")
        return try change(source) { $0.setting?.exclude.removeAll { $0.id == container } }
    }

    func run(_ source: String, in space: URL) async throws -> SourceStatus {
        try record("run \(source)", "run")
        return try change(source) { $0.state = .running }
    }

    func disconnect(_ source: String, in space: URL) async throws {
        try record("disconnect \(source)", "disconnect")
        _ = try change(source) { status in
            status.setting = nil
            status.state = .off
        }
    }

    func setAccount(_ state: SourceAccountState, for source: String) {
        self.state.withLock { $0.accounts[source] = state }
    }

    func account(for source: String) -> SourceAccountState {
        state.withLock { $0.accounts[source] ?? .signedOut }
    }

    /// Makes the next `count` browser steps of `source` answer that the person is not done yet.
    func setBrowserWaits(_ count: Int, for source: String) {
        state.withLock { $0.browserWaits[source] = count }
    }

    func account(of source: String) async throws -> SourceAccountState {
        try record("account \(source)", "account")
        return account(for: source)
    }

    /// Signs in as the services do: Telegram by phone, code and password; Gmail and Slack in
    /// the browser; mail with an address and an app password.
    func signIn(_ source: String, _ step: SourceSignInRequest) async throws -> SourceAccountState {
        let described: String = switch step {
        case .start(let account): "start \(account)"
        case .code(let code): "code \(code)"
        case .password(let password): "password \(password)"
        case .browser: "browser"
        }
        try record("sign-in \(source) \(described)", "sign-in")
        func wrong(_ message: String) -> ServiceError { .remote(code: -32004, kind: "sign-in", message: "sign in again: \(message)") }
        return try state.withLock { state in
            let next: SourceAccountState
            switch (source, step) {
            case ("telegram", .start): next = .codeSent(via: "the Telegram app")
            case ("telegram", .code(let code)):
                guard code == "12345" else { throw wrong("that code is not right") }
                next = .passwordNeeded(hint: "the name of the cat")
            case ("telegram", .password(let password)):
                guard password == "secret" else { throw wrong("that password is not right") }
                next = .signedIn(account: "Ada Lovelace")
            case ("gmail", .start), ("slack", .start): next = .openBrowser(url: URL(string: "https://example.com/\(source)/sign-in")!)
            case ("gmail", .browser), ("slack", .browser):
                if let left = state.browserWaits[source], left > 0 {
                    state.browserWaits[source] = left - 1
                    throw wrong("finish signing in in the browser first, then go on")
                }
                next = .signedIn(account: source == "slack" ? "Example workspace" : "ada@example.com")
            case ("imap", .start(let account)):
                state.mailAccount = account
                next = .passwordNeeded(hint: account.components(separatedBy: " ").first)
            case ("imap", .password(let password)):
                guard password == "abcd efgh ijkl", let account = state.mailAccount else { throw wrong("the server refused the app password") }
                next = .signedIn(account: account.components(separatedBy: " ")[0])
            default: throw wrong("that step does not apply")
            }
            state.accounts[source] = next.isSignedIn ? next : state.accounts[source] ?? .signedOut
            return next
        }
    }

    func signOut(_ source: String) async throws -> Bool {
        try record("sign-out \(source)", "sign-out")
        return state.withLock { state in
            defer { state.accounts[source] = .signedOut }
            return state.accounts[source]?.isSignedIn ?? false
        }
    }
}

/// A sign-in of two steps, a phone number and a code, like the ones connectors add.
final class FakeSignIn: SourceSignIn {
    private let log = Mutex<[String]>([])

    var calls: [String] { log.withLock { $0 } }

    func start(source: String, space: URL) async throws -> SourceSignInStep? {
        log.withLock { $0.append("start \(source)") }
        return SourceSignInStep(id: "phone", title: "Your phone number", fields: [.init(id: "phone", label: "Phone", isCode: true)])
    }

    func answer(_ step: SourceSignInStep, with values: [String: String], source: String, space: URL) async throws -> SourceSignInStep? {
        log.withLock { $0.append("\(step.id) \(values[step.fields[0].id] ?? "")") }
        switch step.id {
        case "phone":
            return SourceSignInStep(id: "code", title: "The code", fields: [.init(id: "code", label: "Code", isCode: true)], continueTitle: "Sign in")
        default:
            guard values["code"] == "12345" else {
                throw ServiceError.remote(code: -32004, kind: "invalid", message: "that code is not right")
            }
            return nil
        }
    }
}

/// Settings > Sources against a fake service client: listing, the connect flow with its
/// permission, sign-in, history and exclusions, pause, run and disconnect.
@MainActor
@Suite(.serialized) struct SourcesModelTests {
    let space = URL(filePath: "/tmp/Work.knode", directoryHint: .isDirectory)

    static func whatsapp(connected: Bool = false, state: SourceState? = nil) -> SourceStatus {
        SourceStatus(
            id: "whatsapp", name: "WhatsApp", kind: "connector", reads: "The chats of WhatsApp for Mac",
            permissions: [.fullDiskAccess], container: "chat", state: state ?? (connected ? .idle : .off),
            setting: connected ? SourceStatus.Setting(on: true, history: .fromToday, connectedAt: 1, since: 0) : nil
        )
    }

    static let telegram = SourceStatus(
        id: "telegram-live", name: "Telegram", kind: "connector", reads: "Your Telegram chats", permissions: [.signIn, .network],
        container: "chat", state: .off
    )

    static let slackImport = SourceStatus(id: "slack", name: "Slack", kind: "importer", reads: "A Slack export", state: .importer)

    /// The pages the model opened.
    final class Opened: Sendable {
        private let urls = Mutex<[URL]>([])
        var all: [URL] { urls.withLock { $0 } }
        func add(_ url: URL) { urls.withLock { $0.append(url) } }
    }

    func makeModel(_ client: FakeSourcesClient, signIns: SourceSignIns = SourceSignIns(), opened: Opened? = nil) -> SourcesModel {
        SourcesModel(client: client, signIns: signIns, openURL: { url in opened?.add(url) }, pollInterval: .milliseconds(20))
    }

    @Test func listsConnectorsThenImportersOfTheSpace() async {
        let client = FakeSourcesClient([Self.slackImport, Self.whatsapp(connected: true)])
        let model = makeModel(client)
        await model.select(space)

        #expect(model.state == .loaded)
        #expect(model.connectors.map(\.id) == ["whatsapp"])
        #expect(model.importers.map(\.id) == ["slack"])
        #expect(client.calls == ["list Work.knode"])
    }

    @Test func offersToStartTheServiceWhenItIsNotRunning() async {
        let client = FakeSourcesClient([Self.whatsapp()])
        client.fail("list", with: .unavailable("no socket"))
        let model = makeModel(client)
        await model.select(space)
        #expect(model.state == .serviceStopped)
        #expect(model.sources.isEmpty)

        client.fail("list", with: nil)
        await model.startServiceAndLoad()
        #expect(model.state == .loaded)
        #expect(model.sources.count == 1)
    }

    @Test func pausesResumesAndRunsAConnectedSource() async throws {
        let client = FakeSourcesClient([Self.whatsapp(connected: true)])
        let model = makeModel(client)
        await model.select(space)
        let source = try #require(model.connectors.first)

        await model.setOn(true, for: source)
        #expect(!client.calls.contains("resume whatsapp"), "It is on already: nothing is sent.")

        await model.setOn(false, for: source)
        #expect(client.calls.contains("pause whatsapp"))
        #expect(model.connectors.first?.state == .paused)
        #expect(model.connectors.first?.isOn == false)

        await model.setOn(true, for: try #require(model.connectors.first))
        await model.run(try #require(model.connectors.first))
        #expect(client.actions == ["pause whatsapp", "resume whatsapp", "run whatsapp"])
        #expect(model.connectors.first?.state == .running)
    }

    @Test func disconnectsAndShowsWhyAFailureHappened() async throws {
        let client = FakeSourcesClient([Self.whatsapp(connected: true)])
        let model = makeModel(client)
        await model.select(space)

        client.fail("disconnect", with: .remote(code: -32004, kind: "invalid", message: "the space is read-only"))
        await model.disconnect(try #require(model.connectors.first))
        #expect(model.errorMessage == "The space is read-only.")
        #expect(model.connectors.first?.isConnected == true)

        client.fail("disconnect", with: nil)
        await model.disconnect(try #require(model.connectors.first))
        #expect(model.errorMessage == nil)
        #expect(model.connectors.first?.isConnected == false)
        #expect(model.connectors.first?.state == .off)
    }

    @Test func watchingShowsWhatARunBroughtIn() async throws {
        let client = FakeSourcesClient([Self.whatsapp(connected: true)])
        let model = makeModel(client)
        await model.select(space)
        let watching = Task { await model.watch() }
        defer { watching.cancel() }

        client.update("whatsapp") { status in
            status.items = 120
            status.lastItems = 4
            status.lastRun = 1_763_380_000_000
        }
        for _ in 0..<100 where model.connectors.first?.items != 120 {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(model.connectors.first?.items == 120)
        #expect(model.connectors.first?.lastItems == 4)

        client.update("whatsapp") { status in
            status.state = .needsUpdate
            status.lastError = .init(kind: "needs-update", message: "the table message has no column text", at: 1)
        }
        for _ in 0..<100 where model.connectors.first?.state != .needsUpdate {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(model.connectors.first?.state == .needsUpdate)
        #expect(model.connectors.first?.lastError?.kind == "needs-update")
    }

    @Test func connectingALocalSourceAsksForFullDiskAccessThenHistoryThenExclusions() async throws {
        let client = FakeSourcesClient([Self.whatsapp()])
        client.setContainers([
            SourceContainer(id: "2@g.us", name: "Family"), SourceContainer(id: "1@s.whatsapp.net", name: "Ada"),
        ], for: "whatsapp")
        let opened = Opened()
        let model = makeModel(client, opened: opened)
        await model.select(space)
        let first = try #require(model.connectors.first)
        let flow = try #require(model.connectFlow(for: first))

        #expect(flow.step == .fullDiskAccess)
        #expect(flow.steps == ["Access", "History", "Chats"])
        #expect(!flow.isManaging)
        flow.openFullDiskAccess()
        #expect(opened.all == [SourceConnectFlow.fullDiskAccessURL])
        #expect(SourceConnectFlow.fullDiskAccessURL.absoluteString.hasPrefix("x-apple.systempreferences:"))
        await flow.begin()
        #expect(flow.step == .fullDiskAccess, "Nothing is asked of the service before the person goes on.")
        #expect(client.calls == ["list Work.knode"])

        await flow.continueFromFullDiskAccess()
        #expect(flow.step == .history)
        #expect(flow.history == .fromToday, "From today is chosen unless the person chooses Everything.")

        flow.history = .everything
        await flow.connect()
        // Connected paused, so nothing is read before the exclusions are chosen.
        #expect(client.actions == ["connect whatsapp everything paused", "containers whatsapp"])
        #expect(client.source("whatsapp")?.isOn == false)
        #expect(flow.step == .containers)
        #expect(flow.stepIndex == 2)
        #expect(flow.containers.map(\.name) == ["Ada", "Family"])
        #expect(flow.containers.allSatisfy { !$0.excluded }, "Every chat comes in until it is switched off.")

        let family = try #require(flow.containers.last)
        await flow.setIncluded(false, family)
        #expect(client.calls.contains("exclude whatsapp 2@g.us"))
        #expect(flow.containers.last?.excluded == true)
        #expect(client.source("whatsapp")?.setting?.exclude.map(\.name) == ["Family"])
        // Switching it off twice sends nothing more.
        let count = client.actions.count
        await flow.setIncluded(false, try #require(flow.containers.last))
        #expect(client.actions.count == count)

        await flow.finish()
        #expect(client.calls.contains("resume whatsapp"))
        #expect(flow.isFinished)
        #expect(client.source("whatsapp")?.isOn == true)
        #expect(client.source("whatsapp")?.setting?.history == .everything)
    }

    @Test func choosingWhatAConnectedSourceReadsDoesNotPauseIt() async throws {
        var connected = Self.whatsapp(connected: true)
        connected.setting?.exclude = [SourceContainer(id: "2@g.us", name: "Family", excluded: true)]
        let client = FakeSourcesClient([connected])
        client.setContainers([SourceContainer(id: "2@g.us", name: "Family"), SourceContainer(id: "3@g.us", name: "Work")], for: "whatsapp")
        let model = makeModel(client)
        await model.select(space)
        let first = try #require(model.connectors.first)
        let flow = try #require(model.connectFlow(for: first))

        #expect(flow.isManaging)
        #expect(flow.step == .containers)
        await flow.begin()
        #expect(flow.containers.first { $0.id == "2@g.us" }?.excluded == true)

        await flow.setIncluded(true, try #require(flow.containers.first { $0.id == "2@g.us" }))
        #expect(client.calls.contains("include whatsapp 2@g.us"))
        await flow.finish()
        #expect(flow.isFinished)
        #expect(!client.calls.contains { $0.hasPrefix("pause") || $0.hasPrefix("resume") || $0.hasPrefix("connect") })
    }

    @Test func containersThatCannotBeListedShowTheExcludedOnes() async throws {
        var connected = Self.whatsapp(connected: true, state: .needsAttention)
        connected.setting?.exclude = [SourceContainer(id: "2@g.us", name: "Family", excluded: true)]
        let client = FakeSourcesClient([connected])
        client.fail("containers", with: .remote(code: -32004, kind: "permission", message: "Knode has no Full Disk Access"))
        let model = makeModel(client)
        await model.select(space)
        let first = try #require(model.connectors.first)
        let flow = try #require(model.connectFlow(for: first))
        await flow.begin()

        #expect(!flow.containersLoaded)
        #expect(flow.containers.map(\.name) == ["Family"])
        #expect(flow.errorMessage == "Knode has no Full Disk Access.")
    }

    @Test func aConnectorWithASignInGoesThroughItsSteps() async throws {
        let client = FakeSourcesClient([Self.telegram])
        let signIn = FakeSignIn()
        let model = makeModel(client, signIns: SourceSignIns(["telegram-live": signIn]))
        await model.select(space)
        let first = try #require(model.connectors.first)
        let flow = try #require(model.connectFlow(for: first))

        #expect(flow.hasSignIn)
        #expect(flow.steps == ["Sign in", "History", "Chats"])
        await flow.begin()
        guard case .signIn(let phone) = flow.step else { Issue.record("expected the phone step"); return }
        #expect(phone.id == "phone")
        #expect(flow.stepIndex == 0)

        // Nothing goes on until every field has something in it.
        await flow.submitSignIn()
        #expect(signIn.calls == ["start telegram-live"])

        flow.values["phone"] = "+34600000000"
        await flow.submitSignIn()
        guard case .signIn(let code) = flow.step else { Issue.record("expected the code step"); return }
        #expect(code.continueTitle == "Sign in")
        #expect(flow.values.isEmpty, "A new step starts empty.")

        flow.values["code"] = "00000"
        await flow.submitSignIn()
        #expect(flow.errorMessage == "That code is not right.")
        #expect(flow.step == .signIn(code), "A wrong code keeps the step to try again.")

        flow.values["code"] = "12345"
        await flow.submitSignIn()
        #expect(flow.step == .history)
        #expect(flow.errorMessage == nil)
        #expect(signIn.calls == ["start telegram-live", "phone +34600000000", "code 00000", "code 12345"])
        #expect(!client.calls.contains { $0.hasPrefix("connect") }, "Signing in does not connect it yet.")
    }

    @Test func signingInAgainEndsAtWhatItReads() async throws {
        var connected = Self.telegram
        connected.state = .needsAttention
        connected.setting = SourceStatus.Setting(on: true, history: .fromToday, connectedAt: 1, since: 0)
        let client = FakeSourcesClient([connected])
        let signIn = FakeSignIn()
        let model = makeModel(client, signIns: SourceSignIns(["telegram-live": signIn]))
        await model.select(space)
        let first = try #require(model.connectors.first)
        let flow = try #require(model.connectFlow(for: first, signInAgain: true))

        await flow.begin()
        guard case .signIn = flow.step else { Issue.record("expected the sign-in"); return }
        flow.values["phone"] = "+34600000000"
        await flow.submitSignIn()
        flow.values["code"] = "12345"
        await flow.submitSignIn()
        #expect(flow.step == .containers)
        #expect(client.calls.contains("containers telegram-live"))
        await flow.finish()
        #expect(!client.calls.contains { $0.hasPrefix("connect") || $0.hasPrefix("resume") })
    }

    @Test func aSourceWithNoPermissionStartsAtTheHistory() async throws {
        let mail = SourceStatus(id: "mail", name: "Mail", kind: "connector", reads: "Mail", state: .off)
        let client = FakeSourcesClient([mail])
        let model = makeModel(client)
        await model.select(space)
        let first = try #require(model.connectors.first)
        let flow = try #require(model.connectFlow(for: first))
        await flow.begin()
        #expect(flow.step == .history)
        #expect(flow.steps == ["History", "Conversations"])
        await flow.connect()
        #expect(client.actions == ["connect mail from-today paused", "containers mail"])
        #expect(flow.containersLoaded)
        #expect(flow.containers.isEmpty)
    }


    // MARK: Signing in through Knode Service

    static func connector(_ id: String, _ name: String, container: String = "chat", connected: Bool = false) -> SourceStatus {
        SourceStatus(
            id: id, name: name, kind: "connector", reads: "\(name) things", permissions: [.signIn, .network], container: container,
            state: connected ? .idle : .off,
            setting: connected ? SourceStatus.Setting(on: true, history: .fromToday, connectedAt: 1, since: 0) : nil
        )
    }

    /// A model whose connectors sign in through the service's shared sign-in, as in the app.
    func serviceModel(_ client: FakeSourcesClient, opened: Opened? = nil) -> SourcesModel {
        SourcesModel(client: client, openURL: { url in opened?.add(url) }, pollInterval: .milliseconds(20))
    }

    func flow(_ model: SourcesModel, signInAgain: Bool = false) throws -> SourceConnectFlow {
        let first = try #require(model.connectors.first)
        return try #require(model.connectFlow(for: first, signInAgain: signInAgain))
    }

    @Test func telegramSignsInWithPhoneCodeAndPassword() async throws {
        let client = FakeSourcesClient([Self.connector("telegram", "Telegram")])
        client.setContainers([SourceContainer(id: "42", name: "Family")], for: "telegram")
        let model = serviceModel(client)
        await model.select(space)
        #expect(model.account(of: try #require(model.connectors.first)) == .signedOut)
        let flow = try flow(model)
        #expect(flow.step == .checkingSignIn)
        #expect(flow.steps == ["Sign in", "History", "Chats"])

        await flow.begin()
        guard case .signIn(let phone) = flow.step else { Issue.record("expected the phone step"); return }
        #expect(phone.fields.map(\.id) == ["account"])
        flow.values["account"] = " +34600000000 "
        await flow.submitSignIn()

        guard case .signIn(let code) = flow.step else { Issue.record("expected the code step"); return }
        #expect(code.message?.contains("the Telegram app") == true)
        flow.values["code"] = "11111"
        await flow.submitSignIn()
        #expect(flow.errorMessage == "Sign in again: that code is not right.")
        #expect(flow.step == .signIn(code))
        flow.values["code"] = "12345"
        await flow.submitSignIn()

        guard case .signIn(let password) = flow.step else { Issue.record("expected the password step"); return }
        #expect(password.fields.first?.isSecret == true)
        #expect(password.message?.contains("the name of the cat") == true)
        flow.values["password"] = "secret"
        await flow.submitSignIn()
        #expect(flow.step == .history)
        #expect(flow.errorMessage == nil)

        await flow.connect()
        await flow.finish()
        #expect(client.actions.filter { !$0.hasPrefix("account") } == [
            "sign-in telegram start +34600000000", "sign-in telegram code 11111", "sign-in telegram code 12345",
            "sign-in telegram password secret", "connect telegram from-today paused", "containers telegram", "resume telegram",
        ])
        for _ in 0..<100 where !(model.account(of: try #require(model.connectors.first))?.isSignedIn ?? false) {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(model.account(of: try #require(model.connectors.first)) == .signedIn(account: "Ada Lovelace"))
    }

    @Test func gmailOpensTheBrowserAndWaitsUntilItComesBack() async throws {
        let client = FakeSourcesClient([Self.connector("gmail", "Gmail", container: "label")])
        client.setBrowserWaits(2, for: "gmail")
        let opened = Opened()
        let model = serviceModel(client, opened: opened)
        await model.select(space)
        let flow = try flow(model)

        await flow.begin()
        guard case .signIn(let browser) = flow.step else { Issue.record("expected the browser step"); return }
        #expect(browser.browserURL == URL(string: "https://example.com/gmail/sign-in"))
        #expect(opened.all == [URL(string: "https://example.com/gmail/sign-in")!], "The page opens by itself.")
        await flow.browserWaitEnded()

        #expect(flow.step == .history)
        #expect(!flow.isWaitingForBrowser)
        #expect(flow.errorMessage == nil)
        #expect(client.actions.filter { $0.hasPrefix("sign-in") } == [
            "sign-in gmail start ", "sign-in gmail browser", "sign-in gmail browser", "sign-in gmail browser",
        ])
        #expect(client.account(for: "gmail") == .signedIn(account: "ada@example.com"))
    }

    @Test func slackSignInCanBeCancelledWhileWaiting() async throws {
        let client = FakeSourcesClient([Self.connector("slack", "Slack", container: "channel")])
        client.setBrowserWaits(Int.max, for: "slack")
        let opened = Opened()
        let model = serviceModel(client, opened: opened)
        await model.select(space)
        let flow = try flow(model)

        await flow.begin()
        guard case .signIn(let browser) = flow.step else { Issue.record("expected the browser step"); return }
        for _ in 0..<100 where !flow.isWaitingForBrowser {
            try await Task.sleep(for: .milliseconds(5))
        }
        #expect(flow.isWaitingForBrowser)
        flow.cancel()
        await flow.browserWaitEnded()
        #expect(flow.step == .signIn(browser), "Cancelling stays where it was; the sheet closes.")
        #expect(flow.errorMessage == nil)
        #expect(!client.actions.contains { $0.hasPrefix("connect") })

        // Open the page again: it opens once more and waits again.
        client.setBrowserWaits(0, for: "slack")
        flow.openBrowser()
        await flow.browserWaitEnded()
        #expect(opened.all.count == 2)
        #expect(flow.step == .history)
    }

    @Test func mailSignsInWithAddressServerAndAppPasswordInOneStep() async throws {
        let client = FakeSourcesClient([Self.connector("imap", "Mail (IMAP)", container: "mailbox")])
        let model = serviceModel(client)
        await model.select(space)
        let flow = try flow(model)

        await flow.begin()
        guard case .signIn(let mail) = flow.step else { Issue.record("expected the mail step"); return }
        #expect(mail.fields.map(\.id) == ["address", "server", "password"])
        #expect(mail.fields[1].isOptional)
        #expect(mail.fields[2].isSecret)
        flow.values = ["address": "ada@example.com", "password": "wrong"]
        await flow.submitSignIn()
        #expect(flow.errorMessage == "Sign in again: the server refused the app password.")

        flow.values = ["address": "ada@example.com", "server": "imap.example.com", "password": "abcd efgh ijkl"]
        await flow.submitSignIn()
        #expect(flow.step == .history)
        #expect(client.actions.filter { $0.hasPrefix("sign-in") }.suffix(2) == [
            "sign-in imap start ada@example.com imap.example.com", "sign-in imap password abcd efgh ijkl",
        ])
    }

    @Test func aConnectorSignedInAlreadyGoesStraightToTheHistory() async throws {
        let client = FakeSourcesClient([Self.connector("slack", "Slack", container: "channel")])
        client.setAccount(.signedIn(account: "Example workspace"), for: "slack")
        let model = serviceModel(client)
        await model.select(space)
        let flow = try flow(model)
        await flow.begin()
        #expect(flow.step == .history)
        #expect(!client.actions.contains { $0.hasPrefix("sign-in") })
    }

    @Test func signsOutAndSignsInAgainFromTheCard() async throws {
        let client = FakeSourcesClient([Self.connector("slack", "Slack", container: "channel", connected: true)])
        client.setAccount(.signedIn(account: "Example workspace"), for: "slack")
        client.setContainers([SourceContainer(id: "C1", name: "general")], for: "slack")
        let model = serviceModel(client)
        await model.select(space)
        let slack = try #require(model.connectors.first)
        #expect(model.account(of: slack) == .signedIn(account: "Example workspace"))

        await model.signOut(slack)
        #expect(client.actions.contains("sign-out slack"))
        #expect(model.account(of: slack) == .signedOut)
        #expect(model.errorMessage == nil)

        let flow = try flow(model, signInAgain: true)
        #expect(flow.isManaging)
        await flow.begin()
        await flow.browserWaitEnded()
        #expect(flow.step == .containers, "Signing in again ends at what it reads.")
        #expect(flow.containers.map(\.name) == ["general"])
        await flow.finish()
        #expect(!client.actions.contains { $0.hasPrefix("connect") || $0.hasPrefix("resume") })
        for _ in 0..<100 where model.account(of: slack) != .signedIn(account: "Example workspace") {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(model.account(of: slack) == .signedIn(account: "Example workspace"))
    }

    @Test func aSourceThatNeedsNoSignInAsksForNoAccount() async throws {
        let client = FakeSourcesClient([Self.whatsapp(connected: true)])
        let model = serviceModel(client)
        await model.select(space)
        #expect(model.account(of: try #require(model.connectors.first)) == nil)
        #expect(!client.calls.contains { $0.hasPrefix("account") })
    }

    @Test func onlyTheServiceWaitingForTheBrowserIsTriedAgain() {
        #expect(ServiceSourceSignIn.isStillWaiting(.remote(code: -32004, kind: "sign-in", message: "sign in again: allow Knode on Slack's page in the browser first, then go on")))
        #expect(!ServiceSourceSignIn.isStillWaiting(.remote(code: -32004, kind: "sign-in", message: "sign in again: the browser did not come back: sign in again")))
        #expect(!ServiceSourceSignIn.isStillWaiting(.disconnected))
    }

    @Test func signInStepsAreCompleteOnlyWithEveryField() {
        let step = SourceSignInStep(id: "imap", title: "Mail server", fields: [
            .init(id: "user", label: "Address"), .init(id: "password", label: "App password", isSecret: true),
        ])
        #expect(!step.isComplete(["user": "ada@example.com"]))
        #expect(!step.isComplete(["user": "ada@example.com", "password": "  "]))
        #expect(step.isComplete(["user": "ada@example.com", "password": "abcd efgh"]))
        #expect(SourceSignIns().flow(for: SourcesModelTests.telegram) == nil)
        let fallback = SourceSignIns(fallback: FakeSignIn())
        #expect(fallback.flow(for: SourcesModelTests.telegram) != nil, "A connector that reads an account signs in.")
        #expect(fallback.flow(for: SourcesModelTests.whatsapp()) == nil, "One that reads this Mac does not.")
        let optional = SourceSignInStep(id: "mail", title: "Mail", fields: [.init(id: "server", label: "Server", isOptional: true)])
        #expect(optional.isComplete([:]))
    }
}

/// The Sources calls over Knode Service's socket, against the fake service: what is sent, and
/// that the answers of `PROTOCOL.md` are read.
@MainActor
@Suite(.serialized) final class ServiceSourcesClientTests {
    let service: FakeKnodeService
    let space = URL(filePath: "/tmp/Work.knode", directoryHint: .isDirectory)

    init() throws {
        service = try FakeKnodeService()
    }

    deinit {
        service.stop()
    }

    /// The example of `PROTOCOL.md`.
    static let whatsapp: [String: Any] = [
        "id": "whatsapp", "name": "WhatsApp", "kind": "connector", "reads": "The chats of WhatsApp for Mac",
        "permissions": ["full-disk-access"], "container": "chat", "state": "idle",
        "setting": ["on": true, "history": "from-today", "connected_at": 1_763_372_400_000,
                    "since": 1_763_337_600_000, "exclude": [["id": "123@g.us", "name": "Family"]]],
        "last_run": 1_763_380_000_000, "last_success": 1_763_380_000_000, "last_items": 4, "items": 120,
        "next_run": 1_763_380_300_000,
    ]

    @Test func readsTheSourcesOfASpace() async throws {
        service.answer("sources.list", with: [
            Self.whatsapp,
            ["id": "imessage", "name": "Messages", "kind": "connector", "reads": "Messages", "permissions": ["full-disk-access", "contacts"],
             "state": "needs-update", "last_error": ["kind": "needs-update", "message": "the table message changed", "at": 5], "items": 0, "last_items": 0],
            ["id": "telegram", "name": "Telegram", "kind": "importer", "reads": "A Telegram export", "permissions": [], "state": "importer", "items": 0, "last_items": 0],
            ["id": "future", "name": "Future", "kind": "connector", "reads": "", "permissions": ["teleport"], "state": "dreaming", "items": 0, "last_items": 0],
        ])
        let client = ServiceSourcesClient(socketURL: service.socketURL, person: "Ada")
        let sources = try await client.list(in: space)

        #expect(service.params(of: "sources.list")?["space"] as? String == "/tmp/Work.knode")
        #expect(sources.map(\.id) == ["whatsapp", "imessage", "telegram", "future"])
        let whatsapp = try #require(sources.first)
        #expect(whatsapp.state == .idle)
        #expect(whatsapp.needsFullDiskAccess)
        #expect(whatsapp.setting?.history == .fromToday)
        #expect(whatsapp.setting?.since == 1_763_337_600_000)
        #expect(whatsapp.setting?.exclude == [SourceContainer(id: "123@g.us", name: "Family", excluded: true)])
        #expect(whatsapp.items == 120)
        #expect(whatsapp.lastItems == 4)
        #expect(whatsapp.nextRun == 1_763_380_300_000)
        #expect(sources[1].state == .needsUpdate)
        #expect(sources[1].lastError?.message == "the table message changed")
        #expect(sources[1].permissions.map(SourceText.permission) == ["Full Disk Access", "Contacts"])
        #expect(!sources[2].isConnector)
        #expect(sources[3].state == .unknown, "A state this app does not know yet is still listed.")
        #expect(sources[3].permissions.map(SourceText.permission) == ["teleport"])
    }

    @Test func changesASourceAsThePerson() async throws {
        for method in ["sources.connect", "sources.pause", "sources.resume", "sources.exclude", "sources.include", "sources.run"] {
            service.answer(method, with: Self.whatsapp)
        }
        service.answer("sources.disconnect", with: true)
        service.answer("sources.containers", with: [["id": "123@g.us", "name": "Family", "excluded": true], ["id": "9@s.whatsapp.net", "name": "Ada", "excluded": false]])
        let client = ServiceSourcesClient(socketURL: service.socketURL, person: "Ada")

        _ = try await client.connect("whatsapp", history: .everything, on: false, in: space)
        let connect = try #require(service.params(of: "sources.connect"))
        #expect(connect["space"] as? String == "/tmp/Work.knode")
        #expect(connect["source"] as? String == "whatsapp")
        #expect(connect["history"] as? String == "everything")
        #expect(connect["on"] as? Bool == false)
        #expect((connect["by"] as? [String: Any])?["kind"] as? String == "person")
        #expect((connect["by"] as? [String: Any])?["actor"] as? String == "Ada")

        _ = try await client.setOn(false, "whatsapp", in: space)
        _ = try await client.setOn(true, "whatsapp", in: space)
        #expect(service.params(of: "sources.pause")?["source"] as? String == "whatsapp")
        #expect(service.params(of: "sources.resume")?["by"] != nil)

        _ = try await client.exclude(SourceContainer(id: "123@g.us", name: "Family"), from: "whatsapp", in: space)
        #expect(service.params(of: "sources.exclude")?["id"] as? String == "123@g.us")
        #expect(service.params(of: "sources.exclude")?["name"] as? String == "Family")
        _ = try await client.include("123@g.us", in: "whatsapp", in: space)
        #expect(service.params(of: "sources.include")?["id"] as? String == "123@g.us")

        _ = try await client.run("whatsapp", in: space)
        #expect(service.params(of: "sources.run")?["by"] == nil, "Running needs no author.")

        let containers = try await client.containers(of: "whatsapp", in: space)
        #expect(containers.map(\.excluded) == [true, false])

        try await client.disconnect("whatsapp", in: space)
        #expect(service.params(of: "sources.disconnect")?["source"] as? String == "whatsapp")
    }

    @Test func signsInOverTheSocket() async throws {
        let client = ServiceSourcesClient(socketURL: service.socketURL, person: "Ada")
        service.answer("sources.account", with: ["state": "signed-out"])
        #expect(try await client.account(of: "slack") == .signedOut)
        #expect(service.params(of: "sources.account")?["source"] as? String == "slack")

        service.answer("sources.sign_in", with: ["state": "open-browser", "url": "https://slack.com/oauth/v2/authorize?x=1"])
        #expect(try await client.signIn("slack", .start(account: "")) == .openBrowser(url: URL(string: "https://slack.com/oauth/v2/authorize?x=1")!))
        let start = try #require(service.params(of: "sources.sign_in"))
        #expect(start["source"] as? String == "slack")
        #expect((start["step"] as? [String: Any])?["step"] as? String == "start")
        #expect((start["step"] as? [String: Any])?["account"] as? String == "")

        service.answer("sources.sign_in", with: ["state": "code-sent", "via": "the Telegram app"])
        #expect(try await client.signIn("telegram", .code("12345")) == .codeSent(via: "the Telegram app"))
        #expect((service.params(of: "sources.sign_in")?["step"] as? [String: Any])?["code"] as? String == "12345")

        service.answer("sources.sign_in", with: ["state": "password-needed"])
        #expect(try await client.signIn("telegram", .password("secret")) == .passwordNeeded(hint: nil))
        #expect((service.params(of: "sources.sign_in")?["step"] as? [String: Any])?["password"] as? String == "secret")

        service.answer("sources.sign_in", with: ["state": "signed-in", "account": "Example workspace"])
        #expect(try await client.signIn("slack", .browser) == .signedIn(account: "Example workspace"))
        #expect((service.params(of: "sources.sign_in")?["step"] as? [String: Any])?["step"] as? String == "browser")

        service.answer("sources.sign_out", with: true)
        #expect(try await client.signOut("slack"))
        #expect(service.params(of: "sources.sign_out")?["source"] as? String == "slack")
    }

    @Test func aMissingServiceMeansItIsNotRunning() async throws {
        let client = ServiceSourcesClient(socketURL: URL(filePath: "/tmp/kn-none/service.sock"), person: "Ada")
        let model = SourcesModel(client: client)
        await model.select(space)
        #expect(model.state == .serviceStopped)
    }
}
