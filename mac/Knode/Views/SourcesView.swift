import SwiftUI

/// Settings > Sources: what brings things into a space. Connectors keep running in Knode Service
/// and bring in what is new (a chat app, a mail store); importers run once on an export. Each
/// space has its own: choose one, then connect, pause, run or disconnect each connector.
struct SourcesSettingsView: View {
    @Environment(SourcesModel.self) private var sources
    @Environment(AppModel.self) private var model
    @State private var selection: URL?

    private var spaces: [SpaceItem] {
        model.sidebarSpaces.filter { !$0.isExample && !$0.isMissing }
    }

    var body: some View {
        Group {
            if spaces.isEmpty {
                KEmptyState(icon: .import, title: "No spaces", message: "Create or open a space to connect sources to it.")
            } else {
                KSettingsPane {
                    KSettingRow("Space", detail: "Sources bring things into one space. Each space has its own.") {
                        KDropdown(selection: $selection, options: spaces.map { item in
                            KOption(Optional(item.url), item.icon.map { "\($0) \(item.name)" } ?? item.name)
                        })
                        .frame(width: 220)
                    }
                    .padding(.horizontal, KSpace.x1)
                    switch sources.state {
                    case .loading:
                        HStack(spacing: KSpace.x2) { KSpinner(); KExplanation("Asking Knode Service") }
                    case .serviceStopped:
                        SourcesServiceStoppedNotice()
                    case .loaded:
                        SourcesSections(spaceName: spaceName)
                    }
                    if let error = sources.errorMessage {
                        KErrorText(error)
                    }
                }
            }
        }
        .task(id: selection) {
            if selection == nil {
                selection = model.selectedSpaceURL.flatMap { url in spaces.first { $0.url == url }?.url } ?? spaces.first?.url
            }
            await sources.select(selection)
            await sources.watch()
        }
    }

    private var spaceName: String {
        spaces.first { $0.url == selection }?.name ?? selection?.deletingPathExtension().lastPathComponent ?? ""
    }
}

/// The connectors, each in its own card, then the importers.
private struct SourcesSections: View {
    @Environment(SourcesModel.self) private var sources
    let spaceName: String

    var body: some View {
        KSettingsSection("Connectors") {
            KExplanation("A connector runs in Knode Service, on this Mac, and brings in what is new as it happens. Everything comes in except what you leave out. Nothing is sent anywhere.")
            if sources.connectors.isEmpty {
                KHairline()
                KExplanation("This version of Knode has no connectors yet.")
            }
        }
        ForEach(sources.connectors) { source in
            SourceCard(source: source, spaceName: spaceName)
        }
        if !sources.importers.isEmpty {
            KSettingsSection("Importers") {
                KExplanation("An importer runs once, on an export you give it, with the knode import command. Importing again brings in only what is new.")
                ForEach(sources.importers) { source in
                    KHairline()
                    KSettingRow(source.name, detail: source.reads) {
                        KChip(SourceText.state(.importer), icon: .import)
                    }
                }
            }
        }
    }
}

/// One connector: what it reads and needs, what it did, and its actions.
struct SourceCard: View {
    @Environment(SourcesModel.self) private var sources
    @Environment(\.kOverlays) private var overlays
    let source: SourceStatus
    let spaceName: String

    var body: some View {
        KSettingsSection(source.name, highlighted: source.state == .needsUpdate) {
            SourceStateChip(state: source.state)
        } content: {
            KExplanation(source.reads, size: 13)
            SourceFacts(source: source)
            SourceNotice(source: source, spaceName: spaceName)
            if source.needsSignIn, let account = sources.account(of: source) {
                KHairline()
                SourceAccountRow(source: source, account: account)
            }
            KHairline()
            if source.isConnected {
                KSettingToggle("On in \(spaceName)",
                               detail: source.isOn ? "Off pauses it: nothing new comes in until you turn it on again." : "Paused: nothing new comes in.",
                               isOn: Binding(get: { source.isOn }, set: { on in Task { await sources.setOn(on, for: source) } }))
                    .disabled(sources.isBusy || source.state == .unavailable)
                if source.id == PhotosLibraryWire.source, let space = sources.space {
                    KHairline()
                    PhotosLibraryOptionsRows(space: space)
                }
                KHairline()
                HStack(spacing: KSpace.x2) {
                    KButton("Run now", icon: .refresh, size: .small) { Task { await sources.run(source) } }
                        .disabled(sources.isBusy || !source.isOn || source.state == .running || source.state == .unavailable)
                        .kTooltip("Bring in what is new now, without waiting")
                    KButton("Choose \(SourceText.containers(source.container))", icon: .filter, size: .small) { open() }
                        .disabled(sources.isBusy || source.state == .unavailable)
                    Spacer()
                    KButton("Disconnect", icon: .close, kind: .destructive, size: .small) { confirmDisconnect() }
                        .disabled(sources.isBusy)
                }
            } else {
                HStack {
                    if source.state == .unavailable {
                        KExplanation("This Mac has no such connector.")
                    }
                    Spacer()
                    KButton("Connect", icon: .connect, kind: .primary, size: .small) { open() }
                        .disabled(sources.isBusy || source.state == .unavailable)
                }
            }
        }
    }

    private func open(signInAgain: Bool = false) {
        overlays?.connectingSource = sources.connectFlow(for: source, signInAgain: signInAgain)
    }

    private func confirmDisconnect() {
        let secrets = source.needsSignIn ? " Its sign-in is removed from your Keychain." : ""
        overlays?.confirmation = KConfirmation(
            title: "Disconnect \(source.name)?",
            message: "It stops bringing things into \(spaceName). Everything it brought in stays in the space, and so do your notes about it.\(secrets) Connecting it again asks for the history again.",
            confirmTitle: "Disconnect",
            isDestructive: true,
            confirmIcon: .close
        ) { [sources] in Task { await sources.disconnect(source) } }
    }
}

/// What a connector needs and what it did, in small labelled columns.
private struct SourceFacts: View {
    let source: SourceStatus
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(alignment: .top, spacing: KSpace.x6) {
            fact("Needs", source.permissions.isEmpty ? "Nothing" : source.permissions.map(SourceText.permission).joined(separator: ", "))
            if let setting = source.setting {
                fact("History", setting.history == .fromToday
                    ? "From \(Date(unixMilliseconds: setting.since).formatted(.dateTime.day().month(.abbreviated)))"
                    : SourceText.history(.everything))
                fact("Last run", source.lastRun.map { Date(unixMilliseconds: $0).formatted(.relative(presentation: .named)) } ?? "Not yet")
                fact("Items", SourceText.items(source.items), detail: source.lastRun == nil ? nil : "\(source.lastItems) last time")
            }
            Spacer(minLength: 0)
        }
    }

    private func fact(_ label: String, _ value: String, detail: String? = nil) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label).kLineStyle(10).foregroundStyle(theme.muted)
            Text(value).font(KFont.ui(13)).foregroundStyle(theme.fg).fixedSize(horizontal: false, vertical: true)
            if let detail { Text(detail).font(KFont.ui(11)).foregroundStyle(theme.muted) }
        }
    }
}

/// What the person should know about a connector's state: it needs an update, a permission or a
/// sign-in, it failed, or it cannot run here.
private struct SourceNotice: View {
    @Environment(SourcesModel.self) private var sources
    @Environment(\.kOverlays) private var overlays
    let source: SourceStatus
    let spaceName: String

    var body: some View {
        switch source.state {
        case .needsUpdate:
            KBanner(.error, title: "Needs an update",
                    message: "\(source.name) changed how it keeps its data. Knode brings in nothing from it until Knode is updated; what came in before stays.\(detail)")
        case .needsAttention:
            KBanner(.warning, title: "Needs your attention",
                    message: (source.lastError?.message).map(Self.sentence) ?? "A permission or a sign-in is missing.", actionsBelow: true) {
                HStack(spacing: KSpace.x2) {
                    if source.needsFullDiskAccess {
                        KButton("Open Full Disk Access", icon: .settings, size: .small) { sources.openURL(SourceConnectFlow.fullDiskAccessURL) }
                    }
                    if source.needsSignIn, sources.signIns.flow(for: source) != nil {
                        KButton("Sign in again", icon: .connect, size: .small) {
                            overlays?.connectingSource = sources.connectFlow(for: source, signInAgain: true)
                        }
                    }
                }
            }
        case .failing:
            if let error = source.lastError {
                KErrorText("\(Self.sentence(error.message)) It tries again later.")
            }
        case .stopped:
            KBanner(.info, title: "Stopped on this Mac",
                    message: "Connectors are off: sources or capture is off in Knode's config.toml, or Knode Service is not running them.")
        case .unavailable:
            KBanner(.info, title: "Not on this Mac",
                    message: "It was connected to \(spaceName) on another device. This Mac has no such connector, so it does not run here.")
        default:
            EmptyView()
        }
    }

    private var detail: String {
        guard let message = source.lastError?.message, !message.isEmpty else { return "" }
        return " " + Self.sentence(message)
    }

    static func sentence(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let first = trimmed.first else { return trimmed }
        let capital = first.uppercased() + trimmed.dropFirst()
        return capital.hasSuffix(".") ? capital : capital + "."
    }
}

/// The state of a source, as a chip.
struct SourceStateChip: View {
    var state: SourceState

    var body: some View {
        switch state {
        case .idle: KChip(SourceText.state(state), icon: .done, kind: .accent)
        case .running: HStack(spacing: KSpace.x2) { KSpinner(size: 12); KChip(SourceText.state(state), kind: .accent) }
        case .needsUpdate, .needsAttention, .failing: KChip(SourceText.state(state), icon: .warning, kind: .solid)
        default: KChip(SourceText.state(state))
        }
    }
}

/// Connectors run in Knode Service.
struct SourcesServiceStoppedNotice: View {
    @Environment(SourcesModel.self) private var sources

    var body: some View {
        KBanner(.warning, title: "Knode Service is not running",
                message: "Connectors run in Knode Service, and are managed through it. Start it to see and change them.", actions: {
            KButton("Start", icon: .play, kind: .primary, size: .small) {
                Task { await sources.startServiceAndLoad() }
            }
            .disabled(sources.isBusy)
        })
    }
}

// MARK: - Connect sheet

/// Connects a source step by step, or changes what a connected one reads.
struct ConnectSourceSheet: View {
    @Bindable var flow: SourceConnectFlow
    let close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x5) {
            KSheetTitle(kicker: flow.isManaging ? "What it reads" : "Connect a source", title: flow.source.name,
                        message: flow.source.reads, close: dismiss)
            if !flow.isManaging {
                SourceStepsBar(steps: flow.steps, current: flow.stepIndex)
            }
            switch flow.step {
            case .fullDiskAccess: FullDiskAccessStep(flow: flow)
            case .checkingSignIn:
                HStack(spacing: KSpace.x2) { KSpinner(); KExplanation("Checking whether \(flow.source.name) is signed in") }
            case .signIn(let step): SignInStepView(flow: flow, step: step)
            case .history: HistoryStep(flow: flow)
            case .containers: ContainersStep(flow: flow)
            }
            if let error = flow.errorMessage {
                KErrorText(error)
            }
            footer
        }
        .task { await flow.begin() }
        .onChange(of: flow.isFinished) { _, finished in if finished { close() } }
        .onDisappear { flow.cancel() }
    }

    /// Closing after connecting turns the source on, as Done does.
    private func cancel() {
        flow.cancel()
        close()
    }

    private func dismiss() {
        if flow.step == .containers {
            Task { await flow.finish() }
        } else {
            close()
        }
    }

    @ViewBuilder private var footer: some View {
        HStack(spacing: KSpace.x2) {
            if case .signIn(let step) = flow.step, step.browserURL != nil {
                KButton("Open the page again", icon: .web, kind: .ghost) { flow.openBrowser() }
            }
            Spacer()
            if flow.isBusy { KSpinner() }
            switch flow.step {
            case .fullDiskAccess:
                KButton("Cancel", icon: .close, action: cancel)
                primary("Continue", icon: .forward, disabled: flow.isBusy) { Task { await flow.continueFromFullDiskAccess() } }
            case .checkingSignIn:
                KButton("Cancel", icon: .close, action: cancel)
            case .signIn(let step) where step.browserURL != nil:
                KButton("Cancel", icon: .close, action: cancel)
            case .signIn(let step):
                KButton("Cancel", icon: .close, action: cancel)
                primary(step.continueTitle, icon: .forward, disabled: flow.isBusy || !step.isComplete(flow.values)) {
                    Task { await flow.submitSignIn() }
                }
            case .history:
                KButton("Cancel", icon: .close, action: cancel)
                primary("Connect", icon: .connect, disabled: flow.isBusy) { Task { await flow.connect() } }
            case .containers:
                primary(flow.isManaging ? "Done" : "Start", icon: flow.isManaging ? .done : .play, disabled: flow.isBusy) {
                    Task { await flow.finish() }
                }
            }
        }
    }

    private func primary(_ title: String, icon: KGlyph, disabled: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) { HStack(spacing: 8) { KIcon(icon, size: .sm); Text(title) } }
            .buttonStyle(KButtonStyle(kind: .primary))
            .keyboardShortcut(.defaultAction)
            .disabled(disabled)
    }
}

/// Where the person is in the steps of connecting.
private struct SourceStepsBar: View {
    let steps: [String]
    let current: Int
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(spacing: KSpace.x3) {
            ForEach(Array(steps.enumerated()), id: \.offset) { index, name in
                HStack(spacing: KSpace.x2) {
                    Text("\(index + 1)").kLineStyle(10)
                        .foregroundStyle(index <= current ? theme.onAccent : theme.muted)
                        .frame(width: 18, height: 18)
                        .background(Circle().fill(index <= current ? theme.accent : theme.panel))
                        .overlay(Circle().strokeBorder(index <= current ? .clear : theme.line))
                    Text(name).kLineStyle(10).foregroundStyle(index == current ? theme.fg : theme.muted)
                }
                if index < steps.count - 1 {
                    Rectangle().fill(theme.line).frame(height: 1).frame(maxWidth: 32)
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Step \(current + 1) of \(steps.count), \(steps.indices.contains(current) ? steps[current] : "")")
    }
}

/// Full Disk Access: why it is needed, and the pane of System Settings where it is turned on.
private struct FullDiskAccessStep: View {
    let flow: SourceConnectFlow
    @Environment(\.kTheme) private var theme

    var body: some View {
        KCard(padding: KSpace.x4) {
            VStack(alignment: .leading, spacing: KSpace.x3) {
                HStack(spacing: KSpace.x2) {
                    KIcon(.private, size: .sm, color: theme.isDark ? theme.indigo : theme.fg)
                    Text("Give Knode Full Disk Access").font(KFont.ui(13, .semibold))
                }
                KExplanation("\(flow.source.name) keeps its data where macOS lets an app read only with Full Disk Access. Knode reads a copy of it, never changes it, and keeps what it reads on this Mac.")
                VStack(alignment: .leading, spacing: KSpace.x1) {
                    KExplanation("1. Open Full Disk Access in System Settings.")
                    KExplanation("2. Turn on Knode. If it is not in the list, add it with the + button, from Applications.")
                    KExplanation("3. Come back here and continue.")
                }
                KButton("Open Full Disk Access", icon: .settings, size: .small) { flow.openFullDiskAccess() }
            }
        }
    }
}

/// One step of a connector's own sign-in: its fields, or a page to open in the browser.
private struct SignInStepView: View {
    @Bindable var flow: SourceConnectFlow
    let step: SourceSignInStep

    var body: some View {
        KCard(padding: KSpace.x4) {
            VStack(alignment: .leading, spacing: KSpace.x3) {
                Text(step.title).font(KFont.ui(13, .semibold))
                if let message = step.message { KExplanation(message) }
                if step.browserURL != nil {
                    HStack(spacing: KSpace.x2) {
                        if flow.isWaitingForBrowser { KSpinner(size: 12) }
                        KExplanation(flow.isWaitingForBrowser ? "Waiting for you to sign in in the browser" : "Open the page again to try once more.")
                    }
                }
                ForEach(step.fields) { field in
                    VStack(alignment: .leading, spacing: KSpace.x1) {
                        Text(field.label).kLineStyle(10)
                        let text = Binding(get: { flow.values[field.id] ?? "" }, set: { flow.values[field.id] = $0 })
                        if field.isSecret {
                            KSecureField(field.placeholder, text: text) { Task { await flow.submitSignIn() } }
                        } else {
                            KTextField(field.placeholder, text: text, mono: field.isCode) { Task { await flow.submitSignIn() } }
                        }
                    }
                }
                if !step.fields.isEmpty {
                    KExplanation("What you enter goes to Knode Service on this Mac, which keeps the sign-in in your Keychain.", size: 11)
                }
            }
        }
    }
}

/// A field whose text is hidden as it is typed, such as a password.
struct KSecureField: View {
    var placeholder: String
    @Binding var text: String
    var onSubmit: () -> Void = {}
    @Environment(\.kTheme) private var theme
    @FocusState private var focused: Bool

    init(_ placeholder: String, text: Binding<String>, onSubmit: @escaping () -> Void = {}) {
        self.placeholder = placeholder
        self._text = text
        self.onSubmit = onSubmit
    }

    var body: some View {
        KFieldBox(focused: focused) {
            KIcon(.private, size: .sm, color: theme.muted)
            SecureField("", text: $text, prompt: Text(placeholder).foregroundStyle(theme.muted))
                .textFieldStyle(.plain)
                .font(KFont.ui(13))
                .foregroundStyle(theme.fg)
                .focused($focused)
                .onSubmit(onSubmit)
                .accessibilityLabel(placeholder)
        }
        .onTapGesture { focused = true }
    }
}

/// From today or everything, chosen once.
private struct HistoryStep: View {
    @Bindable var flow: SourceConnectFlow

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x3) {
            Text("What should it bring in?").font(KFont.ui(13, .semibold))
            KCard(padding: KSpace.x2) {
                VStack(spacing: KSpace.x1) {
                    choice(.fromToday, title: "From today", detail: "What is new from the start of today on. Recommended.")
                    choice(.everything, title: "Everything", detail: "Everything \(flow.source.name) has, then what is new. A long history can take a while.")
                }
            }
            KExplanation("The history is chosen once. To change it, disconnect and connect again; what came in stays.")
        }
    }

    private func choice(_ history: SourceHistory, title: String, detail: String) -> some View {
        KRow(isSelected: flow.history == history, action: { flow.history = history }) {
            KRadioMark(isOn: flow.history == history)
            KSettingText(title: title, detail: detail)
        }
        .accessibilityLabel(title)
    }
}

/// A round mark for one choice among a few.
private struct KRadioMark: View {
    var isOn: Bool
    @Environment(\.kTheme) private var theme

    var body: some View {
        ZStack {
            Circle().strokeBorder(isOn ? theme.indigo : theme.line, lineWidth: 1.5)
            if isOn { Circle().fill(theme.indigo).padding(4) }
        }
        .frame(width: 16, height: 16)
    }
}

/// Every chat, channel or folder, each with a switch: on is read, off is left out.
private struct ContainersStep: View {
    let flow: SourceConnectFlow
    @State private var filter = ""

    private var shown: [SourceContainer] {
        let words = filter.trimmingCharacters(in: .whitespaces)
        return words.isEmpty ? flow.containers : flow.containers.filter { $0.name.localizedStandardContains(words) }
    }

    var body: some View {
        let noun = flow.containerNoun
        VStack(alignment: .leading, spacing: KSpace.x3) {
            if flow.source.picks {
                KExplanation("Only the \(noun) you switch on are read, none until you pick one. One switched off is not read again; what came from it stays, marked as no longer read.")
            } else {
                KExplanation("Every one of your \(noun) comes in, and new ones too, except the ones you switch off. One switched off is never read again; what came from it stays.")
            }
            if flow.containers.count > 8 {
                KSearchField(placeholder: "Find", text: $filter, size: .small)
            }
            KCard(padding: KSpace.x2) {
                if !flow.containersLoaded, flow.isBusy {
                    HStack(spacing: KSpace.x2) { KSpinner(); KExplanation("Asking \(flow.source.name)") }
                        .padding(KSpace.x2)
                } else if flow.containers.isEmpty {
                    KExplanation(flow.containersLoaded ? "No \(noun) yet. New ones come in as they appear." : "Knode can list them once \(flow.source.name) can be read.")
                        .padding(KSpace.x2)
                } else {
                    KScrollView {
                        VStack(alignment: .leading, spacing: 0) {
                            ForEach(shown) { container in
                                Toggle(isOn: Binding(get: { !container.excluded }, set: { on in Task { await flow.setIncluded(on, container) } })) {
                                    Text(container.name).font(KFont.ui(13)).lineLimit(1)
                                }
                                .toggleStyle(KToggleStyle())
                                .disabled(flow.isBusy)
                                .padding(.horizontal, KSpace.x2)
                                .padding(.vertical, 6)
                            }
                        }
                    }
                    .frame(height: min(CGFloat(shown.count) * 36 + 8, 260))
                }
            }
            if !flow.isManaging {
                KExplanation("\(flow.source.name) is paused until you start it, so nothing is read before you choose.")
            }
        }
    }
}

/// The account a connector reads, on this Mac: who it is signed in as, with Sign out, or Sign in.
private struct SourceAccountRow: View {
    @Environment(SourcesModel.self) private var sources
    @Environment(\.kOverlays) private var overlays
    let source: SourceStatus
    let account: SourceAccountState

    var body: some View {
        switch account {
        case .signedIn(let name):
            KSettingRow("Signed in", detail: name.isEmpty ? "The sign-in is kept in your Keychain." : "As \(name). The sign-in is kept in your Keychain.") {
                KButton("Sign out", icon: .key, kind: .ghost, size: .small) { confirmSignOut() }
                    .disabled(sources.isBusy)
            }
        default:
            KSettingRow("Not signed in", detail: "\(source.name) reads nothing until you sign in.") {
                if source.isConnected, sources.signIns.flow(for: source) != nil {
                    KButton("Sign in", icon: .connect, size: .small) {
                        overlays?.connectingSource = sources.connectFlow(for: source, signInAgain: true)
                    }
                    .disabled(sources.isBusy)
                }
            }
        }
    }

    private func confirmSignOut() {
        overlays?.confirmation = KConfirmation(
            title: "Sign out of \(source.name)?",
            message: "Knode ends its session and removes the sign-in from your Keychain. \(source.name) brings in nothing more, in any space, until you sign in again. What it brought in stays.",
            confirmTitle: "Sign out",
            isDestructive: true,
            confirmIcon: .key
        ) { [sources] in Task { await sources.signOut(source) } }
    }
}

#if DEBUG
/// A sources client for previews: answers at once, signs in after one step.
private struct PreviewSourcesClient: SourcesClient {
    var account: SourceAccountState

    func list(in space: URL) async throws -> [SourceStatus] { [] }
    func containers(of source: String, in space: URL) async throws -> [SourceContainer] {
        [SourceContainer(id: "c1", name: "general"), SourceContainer(id: "c2", name: "random", excluded: true)]
    }
    func connect(_ source: String, history: SourceHistory, on: Bool, in space: URL) async throws -> SourceStatus { SourcesPreviews.slack }
    func setOn(_ on: Bool, _ source: String, in space: URL) async throws -> SourceStatus { SourcesPreviews.slack }
    func exclude(_ container: SourceContainer, from source: String, in space: URL) async throws -> SourceStatus { SourcesPreviews.slack }
    func include(_ container: String, in source: String, in space: URL) async throws -> SourceStatus { SourcesPreviews.slack }
    func run(_ source: String, in space: URL) async throws -> SourceStatus { SourcesPreviews.slack }
    func disconnect(_ source: String, in space: URL) async throws {}
    func account(of source: String) async throws -> SourceAccountState { account }
    func signIn(_ source: String, _ step: SourceSignInRequest) async throws -> SourceAccountState {
        if case .browser = step { try await Task.sleep(for: .seconds(3600)) }
        return account
    }
    func signOut(_ source: String) async throws -> Bool { true }
}

enum SourcesPreviews {
    static let slack = SourceStatus(id: "slack", name: "Slack", kind: "connector", reads: "The channels and messages you are in on Slack",
                                    permissions: [.signIn, .network], container: "channel", state: .off)

    @MainActor static func sheet(_ account: SourceAccountState) -> some View {
        let client = PreviewSourcesClient(account: account)
        let flow = SourceConnectFlow(source: slack, space: URL(filePath: "/tmp/Preview.knode"), client: client,
                                     signIn: ServiceSourceSignIn(client: client), openURL: { _ in }, onChange: { _ in })
        return ConnectSourceSheet(flow: flow) {}
            .padding(KSpace.x6)
            .frame(width: 560)
            .knodeTheme(background: true)
    }
}

#Preview("Slack, signed out: waiting for the browser") {
    SourcesPreviews.sheet(.openBrowser(url: URL(string: "https://slack.com/oauth/v2/authorize")!))
}

#Preview("Slack, signed in: history") {
    SourcesPreviews.sheet(.signedIn(account: "Ada at Example"))
}

#Preview("Telegram code") {
    let client = PreviewSourcesClient(account: .codeSent(via: "the Telegram app"))
    let telegram = SourceStatus(id: "telegram", name: "Telegram", kind: "connector", reads: "Your Telegram chats",
                                permissions: [.signIn, .network], container: "chat", state: .off)
    return ConnectSourceSheet(flow: SourceConnectFlow(source: telegram, space: URL(filePath: "/tmp/Preview.knode"), client: client,
                                                      signIn: ServiceSourceSignIn(client: client), openURL: { _ in }, onChange: { _ in })) {}
        .padding(KSpace.x6)
        .frame(width: 560)
        .knodeTheme(background: true)
}
#endif
