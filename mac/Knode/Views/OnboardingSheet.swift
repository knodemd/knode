import AppKit
import SwiftUI

/// The tutorial edits a core-generated proposal; all import/archive validation stays in Rust.
private struct ReviewRow: Identifiable {
    var section: String
    var index: Int
    var id: String { "\(section):\(index)" }
}
private struct ImportGroup: Codable, Equatable, Identifiable {
    var id: String
    var name: String
}
private struct ImportDocument: Codable, Equatable, Identifiable {
    var path: String
    var title: String
    var project: String
    var include: Bool
    var digest: String
    /// The repository inside the source that tracks this document, when it starts unchecked for that reason.
    var repository: String?
    var id: String { path }
}
private struct ImportSourceFile: Codable, Equatable {
    var path: String
    var digest: String
}
private struct ImportPlan: Codable, Equatable {
    var version = 1
    var mode = "vault"
    var source: String?
    var groups: [ImportGroup] = []
    var documents: [ImportDocument] = []
    var files: [ImportSourceFile] = []
    var warnings: [String] = []
}
/// The conversations onboarding would bring in, listed before anything is written.
private struct SessionsPreview: Decodable {
    var projects: [String: [Session]]
    var total: Int
    var secrets: Int
    struct Session: Decodable { var agent: String; var session: String; var title: String; var messages: Int; var secrets: Int }
}
/// What bringing conversations into the new space did.
private struct BroughtSessions: Decodable {
    var brought: Int
    var projects: [String: Int]
    var redacted: Int
}
private struct ImportResult: Decodable {
    var destination: String
    var contexts: [String: String]
    var unresolved: [String: [String]]
    /// Links to documents left out in review: expected, shown as a count.
    var leftOut: [String: [String]]?
    var report: Report
    var plan: ImportPlan
    var conversations: BroughtSessions?
    var conversationsError: String?
    struct Report: Decodable {
        var attachments: [String]
        var attached: [String]
        var unlinked: [Skipped]
    }
    struct Skipped: Decodable { var path: String; var reason: String }
    enum CodingKeys: String, CodingKey { case destination, contexts, unresolved, plan, conversations; case leftOut = "left_out"; case report = "import"; case conversationsError = "conversations_error" }
}

struct OnboardingSheet: View {
    private let existing: URL?
    init(existing: URL? = nil) { self.existing = existing }
    @Environment(AppModel.self) private var model
    @State private var step = 0
    /// Whether an agent does the import, the default, or the person does it in this sheet.
    @State private var byAgent = true
    @State private var agentPrompt: String?
    @State private var copied = false
    @State private var mode = "vault"
    @State private var source: URL?
    /// The suggested name before a source is chosen, and for an empty start.
    static let defaultName = "Imported memory"
    @State private var name = OnboardingSheet.defaultName
    @State private var destinationFolder: URL?
    @State private var plan = ImportPlan()
    @State private var result: ImportResult?
    @State private var busy = false
    @State private var error: String?
    @State private var acknowledged = false
    /// Whether to bring the agent conversations held in the source's project folders. Off
    /// until the person turns it on, after seeing which ones would come.
    @State private var bringsSessions = false
    /// The conversations that would be brought in, for the plan as it is now.
    @State private var sessionsPreview: SessionsPreview?
    @State private var connected = false
    @State private var workingFolder: URL?
    @State private var instructionPreview: String?
    @State private var instructionsInstalled = false
    @State private var archive: URL?
    @State private var confirmArchive = false
    @State private var recoveryNote: String?
    @State private var documentFilter = ""
    @State private var bulkProject = ""

    private var command: KnodeCommand {
        KnodeCommand(executable: Bundle.main.bundleURL.appending(path: "Contents/Helpers/knode"))
    }
    private var destination: URL {
        (destinationFolder ?? model.settings.spacesFolder)
            .appending(path: name.replacing("/", with: "-").replacing(":", with: "-") + ".knode")
    }
    private var included: Int { plan.documents.filter(\.include).count }
    private var matchingDocuments: [Int] {
        plan.documents.indices.filter { documentFilter.isEmpty || plan.documents[$0].path.localizedCaseInsensitiveContains(documentFilter) }
    }

    var body: some View {
        KSheetSurface(width: 760) {
            KSheetTitle(kicker: step == 0 && byAgent ? "Get started" : "Get started · Step \(step + 1) of 3", title: step == 0 ? "Bring your projects" : step == 1 ? "Review your new memory" : "Try your imported copy", close: { if !busy { model.sheet = nil } })
            if step == 0 { startingPoint }
            if step == 1 { review }
            if step == 2 {
                ScrollView { completion }.frame(height: 500)
                if let result {
                    HStack {
                        Spacer()
                        KButton("Open imported space", icon: .folder, kind: .primary) {
                            Task {
                                let url = URL(filePath: result.destination, directoryHint: .isDirectory)
                                await model.openSpace(at: url)
                                if model.defaultSpaceURL == nil { model.setDefaultSpace(url) }
                                model.sheet = nil
                            }
                        }
                    }
                }
            }
            if let error { KErrorText(error).textSelection(.enabled) }
            if busy { HStack { KSpinner(); Text("Working locally…").font(KFont.ui(12)) } }
        }
        .knodeTheme()
        .disabled(busy)
        .task {
            if let existing {
                await perform {
                    let text = try await command.run(["onboard", "inspect", "--target", existing.path(percentEncoded: false)])
                    let loaded = try JSONDecoder().decode(ImportResult.self, from: Data(text.utf8))
                    result = loaded; plan = loaded.plan; mode = loaded.plan.mode
                    if let path = loaded.plan.source { source = URL(filePath: path) }
                    step = 2
                }
            }
        }
    }

    private var startingPoint: some View {
        VStack(alignment: .leading, spacing: 14) {
            Picker("Who brings your notes in?", selection: $byAgent) {
                Text("Let your agent do it").tag(true)
                Text("Do it yourself").tag(false)
            }
            .pickerStyle(.segmented)
            if byAgent { agentStart } else { manualStart }
        }
    }

    /// The agent reads the guide that comes with Knode (`knode onboard guide`), decides what needs
    /// judgment with the person and runs the same commands this sheet runs.
    private var agentStart: some View {
        VStack(alignment: .leading, spacing: 14) {
            KExplanation("Your agent reads a guide that comes with Knode, looks at your folders and asks you only what needs your judgment: which notes are yours, which stay with their code, which projects keep their files. Then it imports them, connects itself and moves your instruction files to Knode, showing you each change first. Your files are never changed: Knode imports a copy.")
            HStack {
                Text(source?.path(percentEncoded: false) ?? "No folder chosen: your agent will ask").font(KFont.mono(12)).lineLimit(2)
                Spacer()
                KButton("Choose folder", icon: .folder) {
                    source = SpacePanels.chooseFolder(title: "Where are your notes?", message: "Read only: your agent imports a copy into a separate space.")
                }
            }
            Text("Paste this into Claude Code, Codex or any agent that can run commands on this Mac:").font(KFont.ui(13))
            ScrollView {
                Text(agentPrompt ?? "…").font(KFont.mono(12)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(height: 76)
            HStack {
                KButton("Not now") { model.sheet = nil }
                Spacer()
                KButton(copied ? "Copied" : "Copy prompt", icon: copied ? .done : .copy, kind: .primary) {
                    guard let agentPrompt else { return }
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(agentPrompt, forType: .string)
                    copied = true
                }
                .disabled(agentPrompt == nil)
            }
        }
        .task(id: source) {
            copied = false
            var args = ["onboard", "prompt"]
            if let source { args += ["--source", source.path(percentEncoded: false)] }
            agentPrompt = try? await command.run(args).trimmingCharacters(in: .whitespacesAndNewlines)
        }
    }

    private var manualStart: some View {
        VStack(alignment: .leading, spacing: 14) {
            KExplanation("Knode creates a separate copy. Your current notes, project files and Obsidian settings stay where they are. You can review the organization, archive the copy and start again.")
            Picker("How do you work today?", selection: $mode) {
                Text("Obsidian / Markdown memory").tag("vault")
                Text("A project with agents").tag("project")
                Text("Start from scratch").tag("empty")
            }
            .pickerStyle(.segmented)
            if mode != "empty" {
                KExplanation(mode == "vault" ? "Choose your vault or notes folder. Folder names suggest projects; you can rename, merge, exclude or reassign notes before importing." : "Choose a project folder. Markdown documentation and agent instruction files provide its starting context. Code, build rules and your repository stay in place.")
                HStack {
                    Text(source?.path(percentEncoded: false) ?? "No folder selected").font(KFont.mono(12)).lineLimit(2)
                    Spacer()
                    KButton("Choose folder", icon: .folder) {
                        source = SpacePanels.chooseFolder(title: "Choose your source", message: "Read only: Knode will import a copy into a separate space.")
                    }
                }
            }
            if let recoveryNote { KExplanation(recoveryNote) }
            if let archive {
                KExplanation("Previous copy archived at \(archive.path(percentEncoded: false)). Its notes and edits remain recoverable with Open space.")
                KButton("Recover archived copy") { Task { await model.openSpace(at: archive); model.sheet = nil } }
            }
            HStack {
                KButton("Not now") { model.sheet = nil }
                Spacer()
                KButton("Analyze locally", icon: .search, kind: .primary) { Task { await analyze() } }
                    .disabled(mode != "empty" && source == nil)
            }
        }
    }

    private var review: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("New space").font(KFont.ui(13))
                KTextField("Space name", text: $name)
                KButton("Location", icon: .folder, size: .small) {
                    if let folder = SpacePanels.chooseFolder(title: "Choose destination", message: "Choose a location outside the source. Existing spaces are never overwritten.") { destinationFolder = folder }
                }
            }
            Text(destination.path(percentEncoded: false)).font(KFont.mono(11)).textSelection(.enabled)
            KExplanation("Rename projects below. Give groups the same name to merge them, or change each note's assignment. Original notes stay intact; Knode adds editable context indexes and part-of links. It does not infer decisions or invent a project status.")
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 9) {
                    ForEach(plan.groups.indices.map { ReviewRow(section: "group", index: $0) }) { row in
                        let i = row.index
                        KTextField("Project name", text: $plan.groups[i].name)
                    }
                    KButton("Add project", icon: .add, size: .small) { plan.groups.append(ImportGroup(id: UUID().uuidString, name: "New project")) }
                    Divider()
                    if !trackedRepositories.isEmpty {
                        repositoryDocuments
                        Divider()
                    }
                    Text("\(included) of \(plan.documents.count) Markdown documents selected").font(KFont.ui(12))
                    bulkAssignment
                    Text("\(matchingDocuments.count) documents match this path filter").font(KFont.ui(11))
                    ForEach(matchingDocuments.map { ReviewRow(section: "document", index: $0) }) { row in
                        let i = row.index
                        HStack {
                            Toggle(isOn: $plan.documents[i].include) { Text(plan.documents[i].path).font(KFont.mono(11)).lineLimit(2) }
                            Spacer()
                            Picker("Project", selection: $plan.documents[i].project) {
                                ForEach(plan.groups) { group in Text(group.name).tag(group.id) }
                            }
                            .labelsHidden().frame(width: 190)
                        }
                    }
                    Divider()
                    Text("Review before continuing").font(KFont.ui(13))
                    ForEach(plan.warnings.indices.map { ReviewRow(section: "warning", index: $0) }) { row in
                        Text("• " + plan.warnings[row.index]).font(KFont.ui(11)).textSelection(.enabled)
                    }
                }
            }.frame(height: 300)
            if mode != "empty" {
                Toggle("Also bring the conversations your agents (Claude Code, Codex…) held in these project folders, each in its project.", isOn: $bringsSessions)
                    .font(KFont.ui(12))
                    .onChange(of: bringsSessions) { _, on in
                        sessionsPreview = nil
                        if on { Task { await previewSessions() } }
                    }
                if bringsSessions, let sessionsPreview { sessionsSummary(sessionsPreview) }
            }
            Toggle("I reviewed the assignments and compatibility notes. Create a separate copy with capture off.", isOn: $acknowledged)
                .font(KFont.ui(12))
            HStack {
                KButton("Back") { step = 0; acknowledged = false }
                Spacer()
                KButton("Create imported copy", icon: .add, kind: .primary) { Task { await createCopy() } }
                    .disabled(!acknowledged || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || plan.groups.isEmpty || (mode == "vault" && included == 0) || (bringsSessions && sessionsPreview == nil))
            }
        }
        .onChange(of: plan) { _, _ in
            // The projects kept decide which conversations come: list them again.
            sessionsPreview = nil
            if bringsSessions { Task { await previewSessions() } }
        }
    }

    private func sessionsSummary(_ preview: SessionsPreview) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if preview.total == 0 {
                Text("No agent conversations were held in the projects you kept.").font(KFont.ui(11))
            } else {
                Text("\(preview.total) conversations will be brought in:").font(KFont.ui(11))
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 3) {
                        ForEach(preview.projects.keys.sorted(), id: \.self) { project in
                            let sessions = preview.projects[project, default: []]
                            Text("\(project): \(sessions.count)").font(KFont.ui(11))
                            ForEach(sessions, id: \.session) { session in
                                Text("  \(session.agent)  \(session.title)").font(KFont.mono(10)).lineLimit(1)
                            }
                        }
                    }
                }.frame(maxHeight: 80)
                if preview.secrets > 0 {
                    Text("\(preview.secrets) likely secrets (keys, tokens, passwords) will be replaced with [redacted]. Other sensitive text is kept as written.").font(KFont.ui(11))
                }
            }
        }
    }

    /// Repositories whose tracked documents start unchecked, with how many and how many are brought.
    private var trackedRepositories: [(name: String, total: Int, brought: Int)] {
        var counts: [String: (total: Int, brought: Int)] = [:]
        for document in plan.documents {
            guard let repository = document.repository else { continue }
            counts[repository, default: (0, 0)].total += 1
            if document.include { counts[repository, default: (0, 0)].brought += 1 }
        }
        return counts.keys.sorted().map { (name: $0, total: counts[$0]!.total, brought: counts[$0]!.brought) }
    }

    private var repositoryDocuments: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Documentation tracked in your repositories").font(KFont.ui(13))
            KExplanation("These start unchecked: they live with their code, where agents read them, and a copy here would not follow their changes. Turn a repository on if it keeps your own notes, such as a status note or session logs.")
            ForEach(trackedRepositories, id: \.name) { repository in
                Toggle(isOn: Binding(
                    get: { repository.brought == repository.total },
                    set: { on in
                        for index in plan.documents.indices where plan.documents[index].repository == repository.name {
                            plan.documents[index].include = on
                        }
                        acknowledged = false
                    }
                )) {
                    let partly = repository.brought > 0 && repository.brought < repository.total ? ", \(repository.brought) brought" : ""
                    Text("\(repository.name), \(repository.total) \(repository.total == 1 ? "document" : "documents")\(partly)").font(KFont.mono(11))
                }
            }
        }
    }

    private var bulkAssignment: some View {
        HStack {
            KTextField("Filter paths, e.g. Projects/Knode/", text: $documentFilter)
            Picker("Assign to", selection: $bulkProject) {
                Text("Choose project").tag("")
                ForEach(plan.groups) { group in Text(group.name).tag(group.id) }
            }.frame(width: 180)
            KButton("Assign matches", size: .small) {
                for index in matchingDocuments { plan.documents[index].project = bulkProject }
                acknowledged = false
            }.disabled(documentFilter.isEmpty || bulkProject.isEmpty || matchingDocuments.isEmpty)
        }
    }

    private var completion: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let result {
                KExplanation("Created \(result.contexts.count) projects from your folders, each with its notes, the projects inside it and related projects. The import left your original source unchanged; edits here are independent. Optional instruction installation is a separate file edit.")
                Text(result.contexts.keys.sorted().joined(separator: " · ")).font(KFont.ui(13))
                if let brought = result.conversations {
                    Text("Brought \(brought.brought) agent conversations into their projects\(brought.redacted > 0 ? ", with \(brought.redacted) likely secrets replaced with [redacted]" : "").").font(KFont.ui(12))
                }
                if let message = result.conversationsError {
                    KErrorText(message).textSelection(.enabled)
                    KButton("Try bringing conversations again", icon: .chat, size: .small) { Task { await retrySessions() } }
                }
                if let leftOut = result.leftOut, !leftOut.isEmpty {
                    let count = leftOut.values.reduce(0) { $0 + $1.count }
                    KExplanation("\(count) \(count == 1 ? "link points" : "links point") to documents you left out. That is expected: they stay as written.")
                }
                if !result.unresolved.isEmpty || !result.report.attachments.isEmpty || !result.report.unlinked.isEmpty {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 5) {
                            Text("Items to review").font(KFont.ui(13))
                            ForEach(result.unresolved.keys.sorted(), id: \.self) { path in
                                Text("\(path): unresolved \(result.unresolved[path, default: []].joined(separator: ", "))").font(KFont.mono(11))
                            }
                            ForEach(Array(result.report.unlinked.enumerated()), id: \.offset) { _, item in
                                Text("\(item.path): \(item.reason)").font(KFont.mono(11))
                            }
                            ForEach(Array(result.report.attachments.prefix(30)), id: \.self) { path in Text("Not imported as an attachment: \(path)").font(KFont.mono(11)) }
                            if result.report.attachments.count > 30 {
                                Text("\(result.report.attachments.count) unused/source files in total. The full report is saved as onboarding.json in this copy.").font(KFont.ui(11))
                            }
                        }
                    }.frame(maxHeight: 110)
                }
                Divider()
                KExplanation("Optional: connect Codex to this copy with read/write access. This replaces its Knode MCP connection and saves a private recovery snapshot. Other MCP servers and existing space grants remain unchanged. No content is sent to a model by this tutorial. Content Codex retrieves later goes to its model provider.")
                KButton(connected ? "Codex connected" : "Connect Codex", icon: .link) { Task { await connectCodex() } }.disabled(connected)
                KExplanation("Optional project rules: choose a working repository separately from your source vault. Preview before installing; only that selected instruction file is edited and a numbered backup is kept. Your source vault cannot be chosen here. For a project import, choosing the source repository is an explicit instruction-file edit, with a backup.")
                HStack {
                    KButton("Choose working folder", icon: .folder, size: .small) { Task { await previewInstructions() } }
                    if workingFolder != nil, instructionPreview != nil {
                        KButton(instructionsInstalled ? "Instructions installed" : "Install previewed instructions", size: .small) { Task { await installInstructions() } }.disabled(instructionsInstalled)
                    }
                }
                if let instructionPreview {
                    ScrollView { Text(instructionPreview).font(KFont.mono(10)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }.frame(height: 90)
                }
                KExplanation("After connecting, start a fresh Codex session. Ask: ‘Find my project context in Knode. Tell me what is documented, what is unverified and what we should do next.’ Save a small checkpoint, then retrieve it from another session. Claude can be connected later.")
                HStack {
                    KButton("Archive copy and start again", kind: .destructive, size: .small) { confirmArchive = true }
                    Spacer()

                }
                if confirmArchive {
                    KExplanation("Archive keeps all notes and edits in a recoverable .knode copy. It removes agent access to this space and briefly reconnects the memory service. The previous Codex configuration is restored if it has not changed since setup; otherwise it is left untouched for review. Instruction-file edits are separate; their backups remain in the working folder. Your original source is never deleted.")
                    HStack {
                        KButton("Keep this copy") { confirmArchive = false }
                        KButton("Confirm archive", kind: .destructive) { Task { await archiveCopy() } }
                    }
                }
            }
        }
    }

    private func perform(_ action: () async throws -> Void) async {
        busy = true; error = nil
        defer { busy = false }
        do { try await action() } catch { self.error = error.localizedDescription }
    }
    private func analyze() async {
        await perform {
            var args = ["onboard", "analyze", "--mode", mode]
            if mode != "empty", let source { args += ["--source", source.path(percentEncoded: false)] }
            let text = try await command.run(args)
            plan = try JSONDecoder().decode(ImportPlan.self, from: Data(text.utf8))
            // Name the new space after its source, unless the person already typed a name.
            if name == Self.defaultName, mode != "empty", let source { name = source.lastPathComponent }
            acknowledged = false; step = 1
        }
    }
    /// Writes the plan as it is now to a temporary file for the CLI, which removes it after.
    private func withPlanFile<T>(_ body: (URL) async throws -> T) async throws -> T {
        let file = FileManager.default.temporaryDirectory.appending(path: "knode-plan-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: file) }
        try JSONEncoder().encode(plan).write(to: file, options: .atomic)
        return try await body(file)
    }
    private func previewSessions() async {
        await perform {
            let text = try await withPlanFile { file in
                try await command.run(["onboard", "sessions", "--plan", file.path(percentEncoded: false)])
            }
            sessionsPreview = try JSONDecoder().decode(SessionsPreview.self, from: Data(text.utf8))
        }
    }
    private func retrySessions() async {
        guard let current = result else { return }
        await perform {
            let text = try await command.run(["onboard", "sessions", "--target", current.destination, "--apply"])
            result?.conversations = try JSONDecoder().decode(BroughtSessions.self, from: Data(text.utf8))
            result?.conversationsError = nil
        }
    }
    private func createCopy() async {
        await perform {
            let folder = destinationFolder ?? model.settings.spacesFolder
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            let text = try await withPlanFile { file in
                var args = ["onboard", "apply", "--plan", file.path(percentEncoded: false), "--destination", destination.path(percentEncoded: false), "--name", name]
                if bringsSessions, mode != "empty" { args.append("--sessions") }
                return try await command.run(args)
            }
            result = try JSONDecoder().decode(ImportResult.self, from: Data(text.utf8))
            connected = false; confirmArchive = false; step = 2
        }
    }
    private func connectCodex() async {
        guard let result else { return }
        await perform { try await command.run(["onboard", "connect-codex", "--target", result.destination]); connected = true }
    }
    private func previewInstructions() async {
        guard let folder = SpacePanels.chooseFolder(title: "Choose working repository", message: "Preview instructions for a working repository. Installing is a separate file edit with a backup; a source vault cannot be selected.") else { return }
        await perform {
            if mode == "vault", let source = plan.source {
                let root = URL(filePath: source).resolvingSymlinksInPath().pathComponents
                let chosen = folder.resolvingSymlinksInPath().pathComponents
                guard !chosen.starts(with: root) else { throw NSError(domain: "Knode", code: 1, userInfo: [NSLocalizedDescriptionKey: "Choose a separate working folder so the source stays unchanged."]) }
            }
            instructionPreview = try await command.run(["instructions", "codex", "--project", folder.path(percentEncoded: false)])
            workingFolder = folder; instructionsInstalled = false
        }
    }
    private func installInstructions() async {
        guard let workingFolder else { return }
        await perform { try await command.run(["instructions", "codex", "--project", workingFolder.path(percentEncoded: false), "--apply"]); instructionsInstalled = true }
    }
    private func archiveCopy() async {
        guard let result else { return }
        await perform {
            let target = URL(filePath: result.destination, directoryHint: .isDirectory)
            try await model.prepareToArchiveImport(target)
            let text = try await command.run(["onboard", "archive", "--target", result.destination])
            let decoded = try JSONDecoder().decode([String: String].self, from: Data(text.utf8))
            if let path = decoded["archived"] { archive = URL(filePath: path, directoryHint: .isDirectory) }
            recoveryNote = decoded["codex_configuration"]
            self.result = nil; connected = false; confirmArchive = false; step = 0
        }
    }
}
