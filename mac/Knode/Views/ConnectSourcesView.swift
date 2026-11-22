import AppKit
import SwiftUI

/// Connect your things: every connector of this Mac as a big tile, one click from its Connect
/// sheet. Shown once after the first run, and from the welcome screen, the sidebar's Sources
/// button, Knode > Connect Your Things… and Settings > Sources.
struct ConnectSourcesSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(SourcesModel.self) private var sources
    @Environment(\.kOverlays) private var overlays
    @Environment(\.openSettings) private var openSettingsAction
    @State private var space: URL?
    @State private var showsGmailWays = false

    private var spaces: [SpaceItem] {
        model.sidebarSpaces.filter { !$0.isExample && !$0.isMissing }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x5) {
            KSheetTitle(kicker: "Sources", title: showsGmailWays ? "Connect Gmail" : "Connect your things",
                        message: showsGmailWays ? nil : "Knode brings in what is new from your apps as it happens, on this Mac. Nothing is sent anywhere. Pick one to connect it; you can change everything later in Settings > Sources.",
                        close: { model.sheet = nil })
            if spaces.isEmpty {
                KEmptyState(icon: .import, title: "No spaces yet", message: "Create or open a space first: sources bring things into a space.")
            } else if showsGmailWays, let ways = ConnectSources.gmailWays(in: sources.sources) {
                GmailWaysView(ways: ways, connect: { connect($0, preset: $1) }, back: { showsGmailWays = false })
            } else {
                spaceRow
                content
            }
            if let error = sources.errorMessage {
                KErrorText(error)
            }
            HStack(spacing: KSpace.x2) {
                Button("Manage in Settings") {
                    model.sheet = nil
                    SettingsTab.open(.sources, with: openSettingsAction)
                }
                .buttonStyle(KLinkButtonStyle(muted: true))
                Spacer()
                KButton("Done", icon: .done, kind: .primary) { model.sheet = nil }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .frame(width: 640)
        .task(id: space) {
            if space == nil { space = ConnectSources.initialSpace(spaces, selected: model.selectedSpaceURL) }
            await sources.select(space)
            await sources.watch()
        }
    }

    private var spaceRow: some View {
        KSettingRow("Into", detail: "What you connect comes into this space. Each space has its own sources.") {
            KDropdown(selection: $space, options: spaces.map { item in
                KOption(Optional(item.url), item.icon.map { "\($0) \(item.name)" } ?? item.name)
            })
            .frame(width: 220)
        }
    }

    @ViewBuilder private var content: some View {
        switch sources.state {
        case .loading:
            HStack(spacing: KSpace.x2) { KSpinner(); KExplanation("Asking Knode Service") }
                .frame(maxWidth: .infinity, minHeight: 120)
        case .serviceStopped:
            SourcesServiceStoppedNotice()
        case .loaded:
            let tiles = ConnectSources.tiles(sources.sources)
            if tiles.isEmpty {
                KExplanation("This version of Knode has no connectors yet.")
            } else {
                KScrollView {
                    SourceTilesGrid(tiles: tiles, open: open)
                        .disabled(sources.isBusy)
                        .padding(2)
                }
                .frame(maxHeight: 420)
            }
        }
    }

    private func open(_ source: SourceStatus) {
        switch ConnectSources.state(of: source) {
        case .comingSoon:
            return
        case .otherWays:
            showsGmailWays = true
        case .needsAccess where source.isConnected:
            if source.needsFullDiskAccess {
                sources.openURL(SourceConnectFlow.fullDiskAccessURL)
            } else {
                connect(source, signInAgain: source.needsSignIn)
            }
        default:
            connect(source)
        }
    }

    /// Opens a source's Connect sheet in place of this one, and comes back here once it closes.
    private func connect(_ source: SourceStatus, signInAgain: Bool = false, preset: SourceConnectPreset? = nil) {
        guard let flow = sources.connectFlow(for: source, signInAgain: signInAgain, preset: preset) else { return }
        overlays?.afterConnectingSource = { [model] in model.showConnectSources() }
        model.sheet = nil
        overlays?.connectingSource = flow
    }
}

/// The tiles of Connect your things, as many in a row as fit.
struct SourceTilesGrid: View {
    let tiles: [SourceStatus]
    let open: (SourceStatus) -> Void

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 190), spacing: KSpace.x3)], spacing: KSpace.x3) {
            ForEach(tiles) { source in
                SourceTile(source: source, state: ConnectSources.state(of: source)) { open(source) }
            }
        }
    }
}

/// One source in Connect your things: its app's icon, its name, what it brings and its state.
private struct SourceTile: View {
    let source: SourceStatus
    let state: SourceTileState
    let action: () -> Void
    @Environment(\.kTheme) private var theme
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            VStack(alignment: .leading, spacing: KSpace.x2) {
                HStack(alignment: .top) {
                    SourceIcon(sourceID: source.id, size: 44)
                    Spacer(minLength: KSpace.x2)
                    chip.fixedSize().layoutPriority(1)
                }
                Text(source.name).font(KFont.ui(15, .semibold)).foregroundStyle(theme.fg).lineLimit(1)
                Text(detail).font(KFont.ui(12)).foregroundStyle(theme.muted)
                    .lineLimit(3, reservesSpace: true)
                    .multilineTextAlignment(.leading)
            }
            .padding(KSpace.x4)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: KRadius.card, style: .continuous)
                .fill(hovering && state.isEnabled && isEnabled ? theme.highlight : theme.panel))
            .overlay(RoundedRectangle(cornerRadius: KRadius.card, style: .continuous)
                .strokeBorder(hovering && state.isEnabled ? theme.indigo.opacity(0.6) : theme.line))
            .opacity(state.isEnabled ? 1 : 0.6)
            .contentShape(RoundedRectangle(cornerRadius: KRadius.card, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(!state.isEnabled)
        .onHover { hovering = $0 }
        .kTooltip(tooltip)
        .accessibilityLabel("\(source.name), \(state.label)")
        .accessibilityHint(detail)
    }

    private var detail: String {
        switch state {
        case .comingSoon: "Coming soon. Not available in this build of Knode."
        case .needsAccess where !source.isConnected: "\(source.reads). Knode asks for access first."
        default: source.reads
        }
    }

    private var tooltip: String {
        switch state {
        case .comingSoon(let reason): SourceText.sentence(reason)
        case .otherWays: "Read Gmail through Apple Mail, or with an app password"
        case .connected: "Choose what it reads"
        default: "Connect \(source.name)"
        }
    }

    @ViewBuilder private var chip: some View {
        switch state {
        case .connected: KChip(state.label, icon: .done, kind: .accent)
        case .needsAccess: KChip(state.label, kind: .solid)
        case .comingSoon: KChip(state.label)
        case .connect, .otherWays: KChip(state.label, icon: .connect)
        }
    }
}

/// Gmail in a build without Sign in with Google: Apple Mail first, then an app password.
private struct GmailWaysView: View {
    let ways: ConnectSources.GmailWays
    let connect: (SourceStatus, SourceConnectPreset?) -> Void
    let back: () -> Void
    @Environment(SourcesModel.self) private var sources
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x4) {
            HStack(spacing: KSpace.x3) {
                SourceIcon(sourceID: ConnectSources.gmail, size: 36)
                KExplanation("This build of Knode cannot sign in with Google yet. These read the same mail, on this Mac:")
            }
            if let appleMail = ways.appleMail {
                way(number: 1, title: "Use Apple Mail", glyph: .mail) {
                    switch appleMail {
                    case .connect:
                        KExplanation("If your Gmail account is in Mail, Knode reads it from there. Recommended.")
                        KButton("Connect Apple Mail", icon: .connect, kind: .primary, size: .small) { connectSource(ConnectSources.appleMail) }
                    case .addAccount:
                        KExplanation("Mail has no account yet. In Mail, choose Mail > Add Account > Google and sign in, then come back and connect Apple Mail.")
                        HStack(spacing: KSpace.x2) {
                            KButton("Open Mail", icon: .mail, kind: .primary, size: .small) { openMail() }
                            KButton("Connect Apple Mail", icon: .connect, size: .small) { connectSource(ConnectSources.appleMail) }
                        }
                    }
                }
            }
            if ways.appPassword {
                way(number: ways.appleMail == nil ? 1 : 2, title: "Use an app password", glyph: .key) {
                    KExplanation("Knode signs in to Gmail over IMAP with a password Google makes for Knode alone. It takes a minute; the next sheet walks you through it.")
                    KButton("Use an app password", icon: .key, size: .small) {
                        connectSource(ConnectSources.imap, preset: .gmailAppPassword)
                    }
                }
            }
            Button(action: back) {
                HStack(spacing: KSpace.x1) { KIcon(.back, size: .xs); Text("All sources") }
            }
            .buttonStyle(KLinkButtonStyle(muted: true))
        }
    }

    private func way<Content: View>(number: Int, title: String, glyph: KGlyph, @ViewBuilder content: () -> Content) -> some View {
        KCard(padding: KSpace.x4) {
            VStack(alignment: .leading, spacing: KSpace.x2) {
                HStack(spacing: KSpace.x2) {
                    Text("\(number)").kLineStyle(10)
                        .foregroundStyle(theme.onAccent)
                        .frame(width: 18, height: 18)
                        .background(Circle().fill(theme.accent))
                    KIcon(glyph, size: .sm, color: theme.isDark ? theme.indigo : theme.fg)
                    Text(title).font(KFont.ui(13, .semibold))
                }
                content()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func connectSource(_ id: String, preset: SourceConnectPreset? = nil) {
        guard let source = sources.sources.first(where: { $0.id == id }) else { return }
        connect(source, preset)
    }

    private func openMail() {
        if let mail = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.mail") {
            sources.openURL(mail)
        }
    }
}

/// Connect…, in Settings > Sources: Connect your things, in the main window.
struct ConnectSourcesButton: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        KButton("Connect…", icon: .connect, kind: .primary, size: .small) {
            model.showConnectSources()
            openWindow(id: "main")
            NSApp.activate()
        }
        .disabled(!model.canShowConnectSources)
        .kTooltip("Connect your mail, chats and other apps, as big tiles")
    }
}
