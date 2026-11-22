import Foundation
import Observation

/// The sources of one space and what each did on this Mac, managed through Knode Service as the
/// person: Settings > Sources. While the pane shows, `watch()` asks for them every few seconds, as
/// Settings > Search does for the index, so a run, its items and its errors show as they happen.
@MainActor
@Observable
final class SourcesModel {
    enum State: Equatable {
        case loading
        case loaded
        /// Knode Service is not running: connectors run in it, and are managed through it.
        case serviceStopped
    }

    /// The space whose sources are shown.
    private(set) var space: URL?
    private(set) var sources: [SourceStatus] = []
    private(set) var state = State.loading
    /// A call that changes a source is running.
    private(set) var isBusy = false
    var errorMessage: String?
    /// The account each connector that reads one is signed in to on this Mac, by source id.
    private(set) var accounts: [String: SourceAccountState] = [:]

    /// Whether `errorMessage` is from reading the list, which the next good read clears; an
    /// action's error stays until the next action.
    @ObservationIgnored private var errorFromLoad = false
    @ObservationIgnored let client: any SourcesClient
    @ObservationIgnored let signIns: SourceSignIns
    @ObservationIgnored let openURL: @MainActor (URL) -> Void
    @ObservationIgnored private let startService: @MainActor () async -> Void
    @ObservationIgnored private let pollInterval: Duration

    /// - Parameters:
    ///   - client: where sources are read and changed: Knode Service.
    ///   - signIns: how the connectors that need one sign in.
    ///   - openURL: opens System Settings or a sign-in page.
    ///   - startService: starts Knode Service, as the menu bar does.
    ///   - pollInterval: how often `watch()` asks for the sources.
    init(
        client: any SourcesClient,
        signIns: SourceSignIns? = nil,
        openURL: @escaping @MainActor (URL) -> Void = { _ in },
        startService: @escaping @MainActor () async -> Void = {},
        pollInterval: Duration = .seconds(2)
    ) {
        self.client = client
        self.signIns = signIns ?? SourceSignIns(fallback: ServiceSourceSignIn(client: client))
        self.openURL = openURL
        self.startService = startService
        self.pollInterval = pollInterval
    }

    /// The connectors, then the importers, as the service lists them.
    var connectors: [SourceStatus] { sources.filter(\.isConnector) }
    var importers: [SourceStatus] { sources.filter { !$0.isConnector } }

    /// Shows the sources of another space.
    func select(_ space: URL?) async {
        guard space != self.space else { return }
        self.space = space
        sources = []
        state = .loading
        errorMessage = nil
        await load()
        await loadAccounts()
    }

    /// Asks which account each connector that reads one is signed in to. Not on every refresh
    /// of the list: when the pane opens, and after signing in or out.
    func loadAccounts() async {
        var found: [String: SourceAccountState] = [:]
        for source in connectors where source.needsSignIn && source.state != .unavailable {
            if let state = try? await client.account(of: source.id) { found[source.id] = state }
        }
        if accounts != found { accounts = found }
    }

    /// Whether the source is signed in, and as whom: `nil` when it reads no account, or the
    /// service could not say.
    func account(of source: SourceStatus) -> SourceAccountState? {
        accounts[source.id]
    }

    /// Signs a connector out of its account on this Mac: the service ends the session and forgets
    /// it. It reads nothing more until it is signed in again.
    func signOut(_ source: SourceStatus) async {
        isBusy = true
        do {
            try await client.signOut(source.id)
            errorMessage = nil
        } catch let error as ServiceError where error.meansNotRunning {
            state = .serviceStopped
        } catch {
            errorMessage = error.localizedDescription
            errorFromLoad = false
        }
        isBusy = false
        await load()
        await loadAccounts()
    }

    /// Asks Knode Service for the sources of the space.
    func load() async {
        guard let space else {
            if !sources.isEmpty { sources = [] }
            if state != .loaded { state = .loaded }
            return
        }
        do {
            let new = try await client.list(in: space)
            // A newer selection may have replaced this one meanwhile.
            guard space == self.space else { return }
            // Set only what changed, so a list asked for every few seconds redraws nothing.
            if sources != new { sources = new }
            if state != .loaded { state = .loaded }
            if errorFromLoad, errorMessage != nil { errorMessage = nil }
        } catch let error as ServiceError where error.meansNotRunning {
            if !sources.isEmpty { sources = [] }
            if state != .serviceStopped { state = .serviceStopped }
        } catch {
            if state != .loaded { state = .loaded }
            if errorMessage != error.localizedDescription { errorMessage = error.localizedDescription }
            errorFromLoad = true
        }
    }

    /// Asks for the sources again and again while the task runs, such as while Settings shows them.
    func watch() async {
        while !Task.isCancelled {
            await load()
            try? await Task.sleep(for: pollInterval)
        }
    }

    /// Starts Knode Service, then lists the sources.
    func startServiceAndLoad() async {
        isBusy = true
        await startService()
        isBusy = false
        await load()
    }

    /// Pauses a connected source, or turns it on again.
    func setOn(_ on: Bool, for source: SourceStatus) async {
        guard source.isConnected, source.isOn != on, let space else { return }
        await perform { try await self.client.setOn(on, source.id, in: space) }
    }

    /// Runs a connected source now.
    func run(_ source: SourceStatus) async {
        guard let space else { return }
        await perform { try await self.client.run(source.id, in: space) }
    }

    /// Sets the largest photo or file a connected source downloads, from what the person typed:
    /// megabytes, `0` for none, or nothing for the connector's default.
    func setMediaLimit(_ text: String, for source: SourceStatus) async {
        guard source.isConnected, source.media, let space else { return }
        guard let limit = MediaLimit(text) else {
            errorMessage = MediaLimit.invalidMessage
            errorFromLoad = false
            return
        }
        guard limit.megabytes != source.setting?.mediaMaxMB else { return }
        await perform { try await self.client.setMedia(limit.megabytes, source.id, in: space) }
    }

    /// Disconnects a source: its settings and secrets go, what it brought in stays.
    func disconnect(_ source: SourceStatus) async {
        guard let space else { return }
        await perform {
            try await self.client.disconnect(source.id, in: space)
            return nil
        }
        await loadAccounts()
    }

    /// The Connect sheet for a source: the whole flow when it is not connected, or only the
    /// chats, channels or folders it reads when it is.
    func connectFlow(for source: SourceStatus, signInAgain: Bool = false) -> SourceConnectFlow? {
        guard let space else { return nil }
        return SourceConnectFlow(
            source: source, space: space, client: client, signIn: signIns.flow(for: source), signInAgain: signInAgain, openURL: openURL,
            onChange: { [weak self] status in self?.apply(status) },
            onSignIn: { [weak self] in Task { await self?.loadAccounts() } }
        )
    }

    /// Puts the status a call answered in the list, then reads the list again.
    func apply(_ status: SourceStatus?) {
        if let status, let index = sources.firstIndex(where: { $0.id == status.id }) {
            sources[index] = status
        }
        Task { await load() }
    }

    private func perform(_ work: @escaping () async throws -> SourceStatus?) async {
        isBusy = true
        do {
            let status = try await work()
            if let status, let index = sources.firstIndex(where: { $0.id == status.id }) { sources[index] = status }
            errorMessage = nil
        } catch let error as ServiceError where error.meansNotRunning {
            state = .serviceStopped
        } catch {
            errorMessage = error.localizedDescription
            errorFromLoad = false
        }
        isBusy = false
        await load()
    }
}

/// Connecting one source to a space, step by step, in the Connect sheet: Full Disk Access for the
/// ones that read another app's files, the connector's own sign-in steps, the history, and the
/// chats, channels or folders to leave out.
///
/// The source is connected paused (`sources.connect` with `on` false), so that nothing is read
/// before the person has chosen what to leave out; Start (or closing the sheet) turns it on.
@MainActor
@Observable
final class SourceConnectFlow: Identifiable {
    enum Step: Equatable {
        /// What Full Disk Access is for, with a button to its pane of System Settings.
        case fullDiskAccess
        /// Asking the service whether the connector is signed in already.
        case checkingSignIn
        /// A step of the connector's sign-in.
        case signIn(SourceSignInStep)
        /// From today or everything.
        case history
        /// Every chat, channel or folder, each with a switch.
        case containers
    }

    /// Privacy & Security > Full Disk Access in System Settings.
    static let fullDiskAccessURL = URL(string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles")!

    let id = UUID()
    let space: URL
    private(set) var source: SourceStatus
    private(set) var step: Step
    var history = SourceHistory.fromToday
    /// The largest file to download, in megabytes as the person typed it, for a source whose
    /// `media` is true: `0` for none, empty for the connector's default.
    var mediaLimit: String
    /// What the person typed in the fields of the sign-in step.
    var values: [String: String] = [:]
    private(set) var containers: [SourceContainer] = []
    /// Whether `containers` holds what the service answered.
    private(set) var containersLoaded = false
    private(set) var isBusy = false
    private(set) var isFinished = false
    var errorMessage: String?
    /// Waiting for the person to sign in on the page opened in the browser.
    private(set) var isWaitingForBrowser = false

    /// Connected by this flow and paused until Done.
    @ObservationIgnored private var pausedForSetup = false
    /// Where the sign-in leads: the history, or what a connected source reads.
    @ObservationIgnored private let afterSignIn: Step
    /// Signing in again a connected source whose sign-in expired.
    @ObservationIgnored private let signInAgain: Bool
    @ObservationIgnored private let client: any SourcesClient
    @ObservationIgnored private let signIn: (any SourceSignIn)?
    @ObservationIgnored private let openURL: @MainActor (URL) -> Void
    @ObservationIgnored private let onChange: @MainActor (SourceStatus?) -> Void
    @ObservationIgnored private let onSignIn: @MainActor () -> Void
    @ObservationIgnored private var browserWait: Task<Void, Never>?
    /// Which wait is the current one: a cancelled one that ends later changes nothing.
    @ObservationIgnored private var waitID = 0

    init(
        source: SourceStatus, space: URL, client: any SourcesClient, signIn: (any SourceSignIn)?, signInAgain: Bool = false,
        openURL: @escaping @MainActor (URL) -> Void, onChange: @escaping @MainActor (SourceStatus?) -> Void,
        onSignIn: @escaping @MainActor () -> Void = {}
    ) {
        mediaLimit = MediaLimit.text(source.setting?.mediaMaxMB)
        self.signInAgain = signInAgain && signIn != nil && source.isConnected
        afterSignIn = source.isConnected ? .containers : .history
        self.source = source
        self.space = space
        self.client = client
        self.signIn = signIn
        self.openURL = openURL
        self.onChange = onChange
        self.onSignIn = onSignIn
        if self.signInAgain {
            step = .checkingSignIn
        } else if source.isConnected {
            step = .containers
        } else if source.needsFullDiskAccess {
            step = .fullDiskAccess
        } else if signIn != nil {
            step = .checkingSignIn
        } else {
            step = .history
        }
    }

    /// Whether the source has its own sign-in steps.
    var hasSignIn: Bool { signIn != nil }

    /// Whether this sheet only changes what a connected source reads.
    var isManaging: Bool { source.isConnected && !pausedForSetup }

    /// What the source calls the things it reads, in the plural: chats, channels, folders.
    var containerNoun: String { SourceText.containers(source.container) }

    /// The steps the person goes through, for the progress at the top.
    var steps: [String] {
        var steps: [String] = []
        if source.needsFullDiskAccess { steps.append("Access") }
        if signIn != nil { steps.append("Sign in") }
        steps.append("History")
        steps.append(containerNoun.prefix(1).uppercased() + containerNoun.dropFirst())
        return steps
    }

    var stepIndex: Int {
        let names = steps
        switch step {
        case .fullDiskAccess: return 0
        case .checkingSignIn, .signIn: return names.firstIndex(of: "Sign in") ?? 0
        case .history: return names.count - 2
        case .containers: return names.count - 1
        }
    }

    /// Runs the first step that needs the service: the sign-in's first step, or the containers of
    /// a connected source.
    func begin() async {
        switch step {
        case .containers: await loadContainers()
        case .checkingSignIn:
            await startSignIn()
            if step == .containers, !containersLoaded { await loadContainers() }
        case .fullDiskAccess, .signIn, .history: break
        }
    }

    func openFullDiskAccess() {
        openURL(Self.fullDiskAccessURL)
    }

    /// After Full Disk Access: the sign-in, or the history.
    func continueFromFullDiskAccess() async {
        guard step == .fullDiskAccess else { return }
        if signIn != nil {
            step = .checkingSignIn
            await startSignIn()
        } else {
            step = .history
        }
    }

    private func startSignIn() async {
        guard let signIn else {
            step = afterSignIn
            return
        }
        await busy {
            let first = try await signIn.start(source: self.source.id, space: self.space)
            self.enter(first)
        }
    }

    /// Goes to the next step of the sign-in, or past it once signed in. A step with a page opens
    /// it in the browser and waits for the person to sign in there.
    private func enter(_ next: SourceSignInStep?) {
        values = [:]
        guard let next else {
            step = afterSignIn
            onSignIn()
            return
        }
        step = .signIn(next)
        if let url = next.browserURL {
            openURL(url)
            startWaiting(for: next)
        }
    }

    private func startWaiting(for current: SourceSignInStep) {
        browserWait?.cancel()
        waitID += 1
        let id = waitID
        isWaitingForBrowser = true
        browserWait = Task { await self.waitForBrowser(current, id: id) }
    }

    private func waitForBrowser(_ current: SourceSignInStep, id: Int) async {
        guard let signIn else { return }
        defer { if id == waitID { isWaitingForBrowser = false } }
        do {
            let next = try await signIn.answer(current, with: [:], source: source.id, space: space)
            guard !Task.isCancelled, id == waitID, step == .signIn(current) else { return }
            errorMessage = nil
            enter(next)
            if step == .containers, !containersLoaded { await loadContainers() }
        } catch {
            guard !Task.isCancelled, id == waitID, !(error is CancellationError) else { return }
            errorMessage = error.localizedDescription
        }
    }

    /// The wait for the browser, for tests: it ends once the person signed in, or it failed.
    func browserWaitEnded() async {
        await browserWait?.value
    }

    /// Opens the sign-in page again, such as after closing its tab by mistake, and waits again.
    func openBrowser() {
        guard case .signIn(let current) = step, let url = current.browserURL else { return }
        openURL(url)
        if !isWaitingForBrowser { startWaiting(for: current) }
    }

    /// Stops waiting for the browser: the sheet was cancelled or closed.
    func cancel() {
        browserWait?.cancel()
        waitID += 1
        isWaitingForBrowser = false
    }

    /// Gives the sign-in what the person entered and goes to its next step, or to the history.
    func submitSignIn() async {
        guard case .signIn(let current) = step, current.browserURL == nil, let signIn, current.isComplete(values) else { return }
        let entered = values
        await busy {
            let next = try await signIn.answer(current, with: entered, source: self.source.id, space: self.space)
            self.enter(next)
        }
        if step == .containers, !containersLoaded { await loadContainers() }
    }

    /// Whether the history step asks for the largest file to download.
    var offersMediaLimit: Bool { source.media }

    /// Whether what was typed for the largest file is a size, `0` or empty.
    var isMediaLimitValid: Bool { !offersMediaLimit || MediaLimit(mediaLimit) != nil }

    /// Connects with the history chosen, paused until the person has chosen what it leaves out,
    /// sets the largest file it downloads when it downloads any, and lists the chats, channels or
    /// folders. Nothing is read before the limit is set.
    func connect() async {
        guard step == .history else { return }
        guard isMediaLimitValid else {
            errorMessage = MediaLimit.invalidMessage
            return
        }
        let limit = offersMediaLimit ? MediaLimit(mediaLimit) : nil
        await busy {
            var status = try await self.client.connect(self.source.id, history: self.history, on: false, in: self.space)
            self.pausedForSetup = true
            self.source = status
            if let limit {
                status = try await self.client.setMedia(limit.megabytes, self.source.id, in: self.space)
                self.source = status
            }
            self.step = .containers
            self.onChange(status)
        }
        if step == .containers { await loadContainers() }
    }

    func loadContainers() async {
        await busy {
            self.containers = try await self.client.containers(of: self.source.id, in: self.space)
                .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
            self.containersLoaded = true
        }
        if !containersLoaded {
            // The connector cannot list them now (it may need a permission): show those excluded.
            containers = source.setting?.exclude.map { SourceContainer(id: $0.id, name: $0.name, excluded: true) } ?? []
        }
    }

    /// Reads a chat, channel or folder, or leaves it out: an excluded one is never read again,
    /// and what came from it stays.
    func setIncluded(_ included: Bool, _ container: SourceContainer) async {
        guard container.excluded == included else { return }
        await busy {
            let status = included
                ? try await self.client.include(container.id, in: self.source.id, in: self.space)
                : try await self.client.exclude(container, from: self.source.id, in: self.space)
            self.source = status
            if let index = self.containers.firstIndex(where: { $0.id == container.id }) {
                self.containers[index].excluded = !included
            }
            self.onChange(status)
        }
    }

    /// Done: turns on a source this flow connected, so it starts reading.
    func finish() async {
        if pausedForSetup {
            await busy {
                let status = try await self.client.setOn(true, self.source.id, in: self.space)
                self.pausedForSetup = false
                self.source = status
                self.onChange(status)
            }
            guard !pausedForSetup else { return }
        }
        isFinished = true
    }

    private func busy(_ work: @escaping () async throws -> Void) async {
        isBusy = true
        defer { isBusy = false }
        do {
            try await work()
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

/// The largest photo or file a connector downloads, as the person types it: megabytes, `0` for
/// none, or nothing for the connector's own default.
struct MediaLimit: Equatable {
    /// `nil` for the connector's default.
    var megabytes: Int64?

    static let invalidMessage = "Enter the largest file to download in MB: a whole number, 0 for none, or leave it empty for the default."

    /// `nil` when the text is not a whole number of megabytes.
    init?(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty {
            megabytes = nil
        } else if let value = Int64(trimmed), value >= 0 {
            megabytes = value
        } else {
            return nil
        }
    }

    static func text(_ megabytes: Int64?) -> String {
        megabytes.map(String.init) ?? ""
    }

    /// The limit in words, for the facts of a source.
    static func words(_ megabytes: Int64?) -> String {
        switch megabytes {
        case nil: "Default"
        case 0: "None"
        case let mb?: "\(mb) MB"
        }
    }
}

/// The words Settings > Sources uses for what the service answers.
enum SourceText {
    /// What a connector reads, in the plural.
    static func containers(_ kind: String?) -> String {
        switch kind {
        case "chat": "chats"
        case "channel": "channels"
        case "folder": "folders"
        case "mailbox": "mailboxes"
        case "calendar": "calendars"
        default: "conversations"
        }
    }

    static func permission(_ permission: SourcePermission) -> String {
        switch permission {
        case .fullDiskAccess: "Full Disk Access"
        case .signIn: "Sign-in"
        case .network: "Network"
        case .calendar: "Calendar"
        case .contacts: "Contacts"
        case .microphone: "Microphone"
        case .photos: "Photos"
        default: permission.rawValue
        }
    }

    static func history(_ history: SourceHistory) -> String {
        switch history {
        case .fromToday: "From today"
        case .everything: "Everything"
        }
    }

    /// The state, as the chip of a source shows it.
    static func state(_ state: SourceState) -> String {
        switch state {
        case .importer: "Importer"
        case .off: "Not connected"
        case .paused: "Paused"
        case .unavailable: "Not on this Mac"
        case .stopped: "Stopped"
        case .running: "Running"
        case .idle: "On"
        case .failing: "Failing"
        case .needsUpdate: "Needs an update"
        case .needsAttention: "Needs attention"
        case .unknown: "Unknown"
        }
    }

    /// How many items, in words.
    static func items(_ count: Int64) -> String {
        count == 1 ? "1 item" : "\(count.formatted()) items"
    }
}
