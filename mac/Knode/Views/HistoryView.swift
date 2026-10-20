import KnodeBrand
import SwiftUI

/// The versions of the open note, newest first, with who made each change and when. Choosing one
/// compares it with the note as it is now.
struct HistoryPanel: View {
    @Bindable var history: NoteHistoryModel

    var body: some View {
        Group {
            if history.versions.isEmpty {
                if history.isLoading {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    ContentUnavailableView("No History", systemImage: "clock.arrow.circlepath", description: Text("Versions appear here as the note changes."))
                }
            } else {
                List(selection: $history.selectedVersionID) {
                    Section {
                        ForEach(history.versions) { version in
                            VersionRow(version: version, summary: history.changeSummary(version), isLatest: version == history.latest)
                                .tag(version.id)
                        }
                    } header: {
                        Text("HISTORY").font(.knodeLine(11)).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .safeAreaInset(edge: .bottom) {
            if let error = history.errorMessage {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.red)
                    .padding(10)
            }
        }
    }
}

/// One version: who, when, and what the change did.
struct VersionRow: View {
    let version: NoteVersion
    let summary: LineDiff.Summary
    let isLatest: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                AuthorLabel(author: version.author)
                Spacer(minLength: 4)
                if isLatest {
                    Text("Current").font(.caption).foregroundStyle(.secondary)
                }
            }
            Text(version.changedAt, format: .dateTime.day().month(.abbreviated).year().hour().minute())
                .font(.caption)
                .foregroundStyle(.secondary)
            HStack(spacing: 6) {
                Text(VersionRow.action(of: version))
                if summary.added > 0 { Text("+\(summary.added)").foregroundStyle(.green) }
                if summary.removed > 0 { Text("-\(summary.removed)").foregroundStyle(.red) }
            }
            .font(.caption.monospacedDigit())
            .foregroundStyle(.secondary)
        }
        .padding(.vertical, 3)
    }

    static func action(of version: NoteVersion) -> String {
        if version.restoredFrom != nil { return "Put back an earlier version" }
        return switch version.operation {
        case .create: "Created"
        case .update: "Edited"
        case .delete: "Deleted"
        }
    }
}

/// A person, an agent or Knode, with the session an agent wrote in.
struct AuthorLabel: View {
    let author: Author

    var body: some View {
        Label {
            HStack(spacing: 4) {
                Text(author.name).fontWeight(.medium).lineLimit(1)
                if let session = author.session {
                    Text("session \(session.prefix(8))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .help("Session \(session)")
                }
            }
        } icon: {
            Image(systemName: icon)
                .foregroundStyle(author.kind == .agent ? KnodeColor.indigo : .secondary)
        }
        .accessibilityLabel("\(kindName) \(author.name)")
    }

    private var icon: String {
        switch author.kind {
        case .person: "person.fill"
        case .agent: "sparkles"
        case .system: "gearshape"
        }
    }

    private var kindName: String {
        switch author.kind {
        case .person: "Person"
        case .agent: "Agent"
        case .system: "Knode"
        }
    }
}

/// A version compared with the note as it is now, with restoring it and undoing its change.
struct VersionComparisonView: View {
    @Bindable var history: NoteHistoryModel
    let version: NoteVersion

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            switch history.comparison {
            case .changes: changes
            case .sideBySide: sideBySide
            }
        }
        .alert(
            "Undo this change anyway?",
            isPresented: Binding(get: { history.conflict != nil }, set: { if !$0 { history.conflict = nil } }),
            presenting: history.conflict
        ) { conflict in
            // The alert clears `conflict` as it closes: the action keeps its own copy.
            Button("Undo Anyway", role: .destructive) { Task { await history.confirmForcedUndo(conflict) } }
            Button("Cancel", role: .cancel) { history.conflict = nil }
        } message: { conflict in
            Text("\(conflict.message) To go back to this version without undoing a single change, use Restore This Version.")
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(version.title).font(.title2.weight(.semibold)).lineLimit(1)
                    HStack(spacing: 6) {
                        Text(VersionRow.action(of: version))
                        Text("by")
                        AuthorLabel(author: version.author)
                        Text(version.changedAt, format: .relative(presentation: .named))
                    }
                    .font(.callout)
                    .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done", systemImage: "xmark") { history.selectedVersionID = nil }
                    .keyboardShortcut(.cancelAction)
            }
            HStack(spacing: 10) {
                Picker("Compare", selection: $history.comparison) {
                    ForEach(NoteHistoryModel.Comparison.allCases) { Text($0.title).tag($0) }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
                Text(history.isCurrent(version) ? "Same as now" : "Compared with now")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Spacer(minLength: 8)
                Button("Undo This Change", systemImage: "arrow.uturn.backward") {
                    Task { await history.undo(version) }
                }
                .fixedSize()
                .disabled(!history.canUndo(version))
                .help("Undo only what this change did, keeping later changes when they do not overlap")
                Button("Restore This Version", systemImage: "clock.arrow.circlepath") {
                    Task { await history.restore(version) }
                }
                .buttonStyle(.borderedProminent)
                .fixedSize()
                .disabled(history.isCurrent(version))
                .help("Put the whole note back as it was in this version, as a new version")
            }
            if let error = history.errorMessage {
                Label(error, systemImage: "exclamationmark.triangle").font(.caption).foregroundStyle(.red)
            }
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 16)
    }

    private var changes: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(history.diff(version)) { line in
                    DiffLineView(line: line)
                }
            }
            .padding(.vertical, 12)
        }
        .accessibilityLabel("Changes between this version and the note now")
    }

    private var sideBySide: some View {
        HSplitView {
            column("This version", text: version.body)
            column("Now", text: history.currentBody())
        }
    }

    private func column(_ title: String, text: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title.uppercased())
                .font(.knodeLine(11))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 18)
                .padding(.vertical, 8)
            Divider()
            ScrollView {
                Text(text)
                    .font(.system(size: 13, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(18)
            }
        }
        .frame(minWidth: 240)
    }
}

/// One line of a comparison: removed lines are only in the version, added lines only in the note
/// now.
struct DiffLineView: View {
    let line: LineDiff.Line

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(marker)
                .foregroundStyle(color)
                .frame(width: 12)
            Text(line.text.isEmpty ? " " : line.text)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(.system(size: 13, design: .monospaced))
        .padding(.horizontal, 18)
        .padding(.vertical, 1)
        .background(color.opacity(line.kind == .same ? 0 : 0.12))
        .accessibilityLabel(accessibility)
    }

    private var marker: String {
        switch line.kind {
        case .same: " "
        case .removed: "-"
        case .added: "+"
        }
    }

    private var color: Color {
        switch line.kind {
        case .same: .secondary
        case .removed: .red
        case .added: .green
        }
    }

    private var accessibility: String {
        switch line.kind {
        case .same: line.text
        case .removed: "Only in this version: \(line.text)"
        case .added: "Only now: \(line.text)"
        }
    }
}
