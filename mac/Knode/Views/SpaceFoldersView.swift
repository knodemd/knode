import AppKit
import SwiftUI

/// The folders that feed a space and whether it captures, as a sheet: from the Space menu and the
/// space's context menu.
struct SpaceFoldersSheet: View {
    @Environment(AppModel.self) private var model
    let url: URL
    @State private var folders: SpaceFoldersModel?

    var body: some View {
        KSheetSurface(width: 580) {
            KSheetTitle(kicker: "Folders and capture", title: folders?.spaceName ?? url.deletingPathExtension().lastPathComponent, close: close)
            if let folders {
                SpaceFoldersForm(folders: folders)
            } else {
                HStack { Spacer(); KSpinner(); Spacer() }.frame(minHeight: 200)
            }
            HStack {
                Spacer()
                Button(action: close) { HStack(spacing: 8) { KIcon(.done, size: .sm); Text("Done") } }
                    .buttonStyle(KButtonStyle(kind: .primary))
                    .keyboardShortcut(.defaultAction)
            }
        }
        .frame(minHeight: 460, alignment: .top)
        .knodeTheme()
        .task {
            folders = await model.foldersModel(at: url)
            if folders == nil { close() }
            await folders?.load()
        }
        .onDisappear { folders?.stop() }
    }

    private func close() {
        if model.sheet != nil { model.sheet = nil }
    }
}

/// Settings > Spaces: the folders and capture of each space in the sidebar.
struct SpacesSettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var selection: URL?
    @State private var folders: SpaceFoldersModel?

    private var spaces: [SpaceItem] {
        model.sidebarSpaces.filter { !$0.isExample && !$0.isMissing }
    }

    var body: some View {
        Group {
            if spaces.isEmpty {
                KEmptyState(icon: .folder, title: "No spaces", message: "Create or open a space to choose the folders that feed it.")
            } else {
                KSettingsPane {
                    KSettingRow("Space", detail: "The folders that feed it, and whether it captures agent conversations.") {
                        KDropdown(selection: $selection, options: spaces.map { item in
                            KOption(Optional(item.url), item.icon.map { "\($0) \(item.name)" } ?? item.name)
                        })
                        .frame(width: 220)
                    }
                    .padding(.horizontal, KSpace.x1)
                    if let folders, folders.spaceID == model.space(at: selection ?? URL(filePath: "/"))?.id {
                        SpaceFoldersForm(folders: folders)
                    } else {
                        HStack { Spacer(); KSpinner(); Spacer() }.frame(minHeight: 200)
                    }
                }
            }
        }
        .task(id: selection) {
            if selection == nil { selection = model.selectedSpaceURL.flatMap { url in spaces.first { $0.url == url }?.url } ?? spaces.first?.url }
            guard let selection else { return }
            folders?.stop()
            folders = await model.foldersModel(at: selection)
            await folders?.load()
        }
        .onDisappear { folders?.stop() }
    }
}

/// What the Folders and Capture screen shows: capture into the space, its folders, and how
/// routing works.
struct SpaceFoldersForm: View {
    @Bindable var folders: SpaceFoldersModel
    @Environment(\.kOverlays) private var overlays
    @State private var projectToAdd: URL?
    @State private var showingProjectSetup = false

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x6) {
            KSettingsSection("Capture") {
                KSettingToggle("Capture agent conversations into this space",
                               detail: "Off, nothing captured is written to \(folders.spaceName), whatever its folders say.",
                               isOn: Binding(get: { folders.captureEnabled }, set: { on in Task { await folders.setCaptureEnabled(on) } }))
                    .disabled(!folders.isLoaded || folders.isBusy)
                KHairline()
                RoutingExplanation()
            }

            KSettingsSection("Folders") {
                AddFolderMenu { kind, url, agent in
                    if kind == .project {
                        projectToAdd = url
                        showingProjectSetup = true
                    } else {
                        Task { await folders.addFolder(kind, at: url, agent: agent) }
                    }
                }
                .disabled(!folders.isLoaded || folders.isBusy)
            } content: {
                if folders.isLoaded, folders.folders.isEmpty {
                    KExplanation("Add a project folder to get started, then repeat for other projects. Importing notes and connecting an agent are separate steps.", size: 13)
                }
                ForEach(Array(folders.folders.enumerated()), id: \.element.id) { offset, folder in
                    if offset > 0 { KHairline() }
                    FolderRow(folder: folder, isBusy: folders.isBusy, setCapture: { on in
                        Task { await folders.setCapture(on, for: folder) }
                    }, remove: { confirmRemove(folder) })
                }
            }

            KSettingsSection("Use this project with an agent") {
                KExplanation("Check the agent's access in Settings > Agents. Adding a project does not connect an agent, change its MCP space, or import the folder's notes. Projects in this space share its access permissions.")
                KExplanation("After connecting, ask the agent to save a project decision in Knode. Start a fresh session and ask it to retrieve that decision and the next action. Keep build and execution rules in your repository instructions.")
            }
            if let error = folders.errorMessage {
                KErrorText(error)
            }
        }
        .sheet(isPresented: $showingProjectSetup) {
            if let projectToAdd {
                AddProjectSheet(url: projectToAdd, spaceName: folders.spaceName, spaceCaptureEnabled: folders.captureEnabled) { capture in
                    showingProjectSetup = false
                    Task { await folders.addFolder(.project, at: projectToAdd, capture: capture) }
                } cancel: {
                    showingProjectSetup = false
                }
            }
        }
    }

    private func confirmRemove(_ folder: SpaceFolder) {
        overlays?.confirmation = KConfirmation(
            title: "Remove \(FolderRow.displayPath(folder))?",
            message: "The space stops being fed by it. Nothing already captured or imported from it is removed.",
            confirmTitle: "Remove folder",
            isDestructive: true
        ) { [folders] in Task { await folders.remove(folder) } }
    }
}

/// How captured conversations find their space, in two sentences (specification, section 15.5).
struct RoutingExplanation: View {
    var body: some View {
        KExplanation("A conversation an agent holds in a project folder is captured into every space that lists the folder with capture on; one held anywhere else goes to the space used for agents and capture. Turn capture off for a project and nothing of it is kept, in any space.")
    }
}

/// One folder: what it is, where, whether it is on this Mac, and capture on or off.
struct FolderRow: View {
    let folder: SpaceFolder
    let isBusy: Bool
    let setCapture: (Bool) -> Void
    let remove: () -> Void
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(alignment: .center, spacing: KSpace.x3) {
            KIcon(Self.glyph(folder.knownKind), size: .sm, color: theme.muted)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 3) {
                Text(Self.displayPath(folder))
                    .font(KFont.mono(12))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .kTooltip(folder.localURL?.path(percentEncoded: false) ?? folder.path)
                HStack(spacing: 6) {
                    Text(kindLabel).font(KFont.ui(12)).foregroundStyle(theme.muted).lineLimit(2)
                    if folder.isMissing {
                        KChip("Not on this Mac", icon: .warning)
                            .fixedSize()
                            .kTooltip("The folder is kept and skipped until it exists here.")
                    }
                }
            }
            Spacer(minLength: KSpace.x2)
            if folder.knownKind != nil {
                Toggle(isOn: Binding(get: { folder.capture }, set: { setCapture($0) })) { EmptyView() }
                    .toggleStyle(KToggleStyle())
                    .fixedSize()
                    .accessibilityLabel(captureLabel)
                    .kTooltip(captureLabel)
                    .disabled(isBusy)
            }
            KIconButton(.delete, title: "Remove this folder from the space", size: .small, action: remove)
                .disabled(isBusy)
        }
        .accessibilityElement(children: .contain)
    }

    private var kindLabel: String {
        switch folder.knownKind {
        case .project: folder.capture ? "Project, captured" : "Project, capture off: nothing of it is kept"
        case .notes: folder.capture ? "Notes" : "Notes, not read"
        case .agent: "Sessions of \(folder.agent.map(CaptureAgent.name(of:)) ?? "an agent")\(folder.capture ? "" : ", not read")"
        case nil: "\(folder.kind.capitalized) folder, from a newer version of Knode"
        }
    }

    private var captureLabel: String {
        folder.knownKind == .project ? "Capture conversations held in this project" : "Read this folder"
    }

    static func glyph(_ kind: SpaceFolder.Kind?) -> KGlyph {
        switch kind {
        case .project: .code
        case .notes: .note
        case .agent: .chat
        case nil: .folder
        }
    }

    /// The path as the space records it, `~/...` inside the home folder.
    static func displayPath(_ folder: SpaceFolder) -> String {
        folder.path
    }
}

/// Adds a project, notes or agent folder, chosen with an open panel.
struct AddFolderMenu: View {
    let add: (SpaceFolder.Kind, URL, String?) -> Void

    var body: some View {
        KMenuButton(style: .secondary) {
            var entries: [KMenuEntry] = [
                .action("Project folder", icon: FolderRow.glyph(.project)) {
                    choose(.project, message: "Agent conversations held in this folder, or in any folder inside it, are captured into this space.")
                },
                .action("Notes folder", icon: FolderRow.glyph(.notes)) {
                    choose(.notes, message: "A folder of Markdown notes for this space.")
                },
                .divider,
                .section("Agent folder"),
            ]
            for agent in CaptureAgent.all {
                entries.append(.action(agent.name, icon: FolderRow.glyph(.agent)) {
                    choose(.agent, agent: agent.id, message: "A folder where \(agent.name) keeps its sessions, read by capture besides its usual folder.")
                })
            }
            return entries
        } label: {
            HStack(spacing: 6) { KIcon(.add, size: .sm); Text("Add folder") }
        }
        .fixedSize()
    }

    private func choose(_ kind: SpaceFolder.Kind, agent: String? = nil, message: String) {
        if let url = SpacePanels.chooseFolder(title: "Add a Folder", message: message) {
            add(kind, url, agent)
        }
    }
}


/// Confirms the capture choice before registering a project. No change occurs on cancellation.
private struct AddProjectSheet: View {
    let url: URL
    let spaceName: String
    let spaceCaptureEnabled: Bool
    let add: (Bool) -> Void
    let cancel: () -> Void
    @State private var capture = false

    var body: some View {
        KSheetSurface(width: 540) {
            KSheetTitle(kicker: "Project setup", title: "Add to \(spaceName)", close: cancel)
            Text(url.path(percentEncoded: false)).font(KFont.mono(12)).textSelection(.enabled)
            KExplanation("This registers a project folder in this space. It does not move files or import your notes. Review existing folder mappings before adding overlapping projects.")
            KToggle("Capture conversations from this project", isOn: $capture)
            KExplanation(capture
                ? "Conversations can be captured into spaces that list this project with capture on. Capture for the space and the service must also be enabled."
                : "Conversations from this project will not be captured into any space. The agent can still save notes intentionally through MCP when it has write access.")
            if !spaceCaptureEnabled {
                KExplanation("Capture is off for \(spaceName). Adding this project does not enable it.")
            }
            HStack {
                KButton("Cancel", action: cancel)
                Spacer()
                KButton("Add project", icon: .add) { add(capture) }
            }
        }
        .knodeTheme()
    }
}
