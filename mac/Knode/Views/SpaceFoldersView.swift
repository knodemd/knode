import AppKit
import SwiftUI

/// The folders that feed a space and whether it captures, as a sheet: from the Space menu and the
/// space's context menu.
struct SpaceFoldersSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let url: URL
    @State private var folders: SpaceFoldersModel?

    var body: some View {
        Group {
            if let folders {
                SpaceFoldersForm(folders: folders)
            } else {
                ProgressView().frame(maxWidth: .infinity, minHeight: 200)
            }
        }
        .frame(width: 540)
        .frame(minHeight: 420)
        .navigationTitle("Folders and Capture")
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done", systemImage: "checkmark") { dismiss() }
            }
        }
        .task {
            folders = await model.foldersModel(at: url)
            if folders == nil { dismiss() }
            await folders?.load()
        }
        .onDisappear { folders?.stop() }
    }
}

/// The Spaces tab of Settings: the folders and capture of each space in the sidebar.
struct SpacesSettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var selection: URL?
    @State private var folders: SpaceFoldersModel?

    private var spaces: [SpaceItem] {
        model.sidebarSpaces.filter { !$0.isExample && !$0.isMissing }
    }

    var body: some View {
        VStack(spacing: 0) {
            if spaces.isEmpty {
                ContentUnavailableView("No Spaces", systemImage: "square.stack", description: Text("Create or open a space to choose the folders that feed it."))
            } else {
                Picker("Space", selection: $selection) {
                    ForEach(spaces) { item in
                        Text(item.icon.map { "\($0) \(item.name)" } ?? item.name).tag(Optional(item.url))
                    }
                }
                .padding([.horizontal, .top], 20)
                if let folders, folders.spaceID == model.space(at: selection ?? URL(filePath: "/"))?.id {
                    SpaceFoldersForm(folders: folders)
                } else {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
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
    @State private var removing: SpaceFolder?

    var body: some View {
        Form {
            Section {
                Toggle(isOn: Binding(get: { folders.captureEnabled }, set: { on in Task { await folders.setCaptureEnabled(on) } })) {
                    Text("Capture agent conversations into this space")
                    Text("Off, nothing captured is written to \(folders.spaceName), whatever its folders say.")
                }
                .disabled(!folders.isLoaded || folders.isBusy)
            } footer: {
                RoutingExplanation()
            }

            Section {
                if folders.isLoaded, folders.folders.isEmpty {
                    Text("No folders yet. Add a project to capture the conversations agents hold in it.")
                        .foregroundStyle(.secondary)
                }
                ForEach(folders.folders) { folder in
                    FolderRow(folder: folder, isBusy: folders.isBusy, setCapture: { on in
                        Task { await folders.setCapture(on, for: folder) }
                    }, remove: { removing = folder })
                }
            } header: {
                HStack {
                    Text("Folders")
                    Spacer()
                    AddFolderMenu { kind, url, agent in
                        Task { await folders.addFolder(kind, at: url, agent: agent) }
                    }
                    .disabled(!folders.isLoaded || folders.isBusy)
                }
            }

            if let error = folders.errorMessage {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.red)
                }
            }
        }
        .formStyle(.grouped)
        .confirmationDialog(
            "Remove \(removing.map { FolderRow.displayPath($0) } ?? "this folder")?",
            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
            presenting: removing
        ) { folder in
            Button("Remove Folder", role: .destructive) { Task { await folders.remove(folder) } }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("The space stops being fed by it. Nothing already captured or imported from it is removed.")
        }
    }
}

/// How captured conversations find their space, in two sentences (specification, section 15.5).
struct RoutingExplanation: View {
    var body: some View {
        Text("A conversation an agent holds in a project folder is captured into every space that lists the folder with capture on; one held anywhere else goes to the space used for agents and capture. Turn capture off for a project and nothing of it is kept, in any space.")
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// One folder: what it is, where, whether it is on this Mac, and capture on or off.
struct FolderRow: View {
    let folder: SpaceFolder
    let isBusy: Bool
    let setCapture: (Bool) -> Void
    let remove: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: Self.symbol(folder.knownKind))
                .foregroundStyle(.secondary)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                Text(Self.displayPath(folder))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(folder.localURL?.path(percentEncoded: false) ?? folder.path)
                HStack(spacing: 6) {
                    Text(kindLabel)
                    if folder.isMissing {
                        Label("Not on this Mac", systemImage: "exclamationmark.triangle")
                            .help("The folder is kept and skipped until it exists here.")
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            if folder.knownKind != nil {
                Toggle(captureLabel, isOn: Binding(get: { folder.capture }, set: setCapture))
                    .toggleStyle(.switch)
                    .labelsHidden()
                    .help(captureLabel)
                    .disabled(isBusy)
            }
            Button("Remove", systemImage: "trash", action: remove)
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                .help("Remove this folder from the space")
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

    static func symbol(_ kind: SpaceFolder.Kind?) -> String {
        switch kind {
        case .project: "hammer"
        case .notes: "doc.text"
        case .agent: "sparkles"
        case nil: "folder"
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
        Menu("Add Folder", systemImage: "plus") {
            Button("Project Folder…", systemImage: FolderRow.symbol(.project)) {
                choose(.project, message: "Agent conversations held in this folder, or in any folder inside it, are captured into this space.")
            }
            Button("Notes Folder…", systemImage: FolderRow.symbol(.notes)) {
                choose(.notes, message: "A folder of Markdown notes for this space.")
            }
            Menu("Agent Folder", systemImage: FolderRow.symbol(.agent)) {
                ForEach(CaptureAgent.all) { agent in
                    Button("\(agent.name)…") {
                        choose(.agent, agent: agent.id, message: "A folder where \(agent.name) keeps its sessions, read by capture besides its usual folder.")
                    }
                }
            }
        }
        .fixedSize()
    }

    private func choose(_ kind: SpaceFolder.Kind, agent: String? = nil, message: String) {
        if let url = SpacePanels.chooseFolder(title: "Add a Folder", message: message) {
            add(kind, url, agent)
        }
    }
}
