import AppKit
import SwiftUI

/// The versions of the open note, newest first, with who made each change and when. Choosing one
/// compares it with the note as it is now.
struct HistoryPanel: View {
    @Bindable var history: NoteHistoryModel
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(spacing: 0) {
            if history.versions.isEmpty {
                if history.isLoading {
                    KSpinner().frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    VStack(spacing: KSpace.x3) {
                        KIcon(.history, size: .lg, color: theme.muted)
                        Text("No history").font(KFont.ui(13, .semibold)).foregroundStyle(theme.fg)
                        Text("Versions appear here as the note changes.")
                            .font(KFont.ui(12)).foregroundStyle(theme.muted).multilineTextAlignment(.center)
                    }
                    .padding(KSpace.x6)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                KScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        KSectionLabel("Versions") {
                            Text("\(history.versions.count)").kLineStyle(10).foregroundStyle(theme.muted).monospacedDigit()
                        }
                        .padding(.horizontal, KSpace.x3)
                        .padding(.bottom, KSpace.x2)
                        ForEach(history.versions) { version in
                            KRow(isSelected: history.selectedVersionID == version.id, action: {
                                history.selectedVersionID = history.selectedVersionID == version.id ? nil : version.id
                            }) {
                                VersionRow(version: version, summary: history.changeSummary(version), isLatest: version == history.latest)
                            }
                        }
                    }
                    .padding(KSpace.x2)
                    .padding(.top, KSpace.x2)
                }
            }
            if let error = history.errorMessage {
                InspectorFooter(error: error) { EmptyView() }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.panel)
        .knodeTheme()
    }
}

/// One version: who, when, and what the change did.
struct VersionRow: View {
    let version: NoteVersion
    let summary: LineDiff.Summary
    let isLatest: Bool
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                AuthorLabel(author: version.author)
                Spacer(minLength: 4)
                if isLatest { KChip("Current", kind: .accent) }
            }
            HStack(spacing: KSpace.x2) {
                Text(VersionRow.action(of: version)).font(KFont.ui(12, .medium)).foregroundStyle(theme.fg)
                if summary.added > 0 {
                    Text("+\(summary.added)").font(KFont.mono(11, .medium)).foregroundStyle(theme.indigo)
                }
                if summary.removed > 0 {
                    Text("-\(summary.removed)").font(KFont.mono(11)).foregroundStyle(theme.muted).strikethrough(color: theme.muted)
                }
                Spacer(minLength: 0)
            }
            Text(version.changedAt, format: .dateTime.day().month(.abbreviated).year().hour().minute())
                .kLineStyle(10)
                .foregroundStyle(theme.muted)
        }
        .padding(.vertical, 2)
        .frame(maxWidth: .infinity, alignment: .leading)
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

/// A person, an agent or Knode as a chip, with the session an agent wrote in.
struct AuthorLabel: View {
    let author: Author
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(spacing: 6) {
            KChip(author.name, icon: icon, kind: author.kind == .agent ? .accent : .neutral)
                .lineLimit(1)
            if let session = author.session {
                Text(session.prefix(8))
                    .font(KFont.mono(10))
                    .foregroundStyle(theme.muted)
                    .lineLimit(1)
                    .kTooltip("Session \(session)")
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(kindName) \(author.name)")
    }

    private var icon: KGlyph {
        switch author.kind {
        case .person: .profile
        case .agent: .knode
        case .system: .settings
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
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Rectangle().fill(theme.line).frame(height: 1)
            switch history.comparison {
            case .changes: changes
            case .sideBySide: sideBySide
            }
        }
        .background(theme.bg)
        .knodeTheme()
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: KSpace.x4) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Version").kLineStyle(11).foregroundStyle(theme.isDark ? theme.indigo : theme.muted)
                    Text(version.title).font(KFont.ui(20, .semibold)).foregroundStyle(theme.fg).lineLimit(1)
                    HStack(spacing: 6) {
                        Text(VersionRow.action(of: version))
                        Text("by")
                        AuthorLabel(author: version.author)
                        Text(version.changedAt, format: .relative(presentation: .named))
                    }
                    .font(KFont.ui(12))
                    .foregroundStyle(theme.muted)
                }
                Spacer()
                Button { history.selectedVersionID = nil } label: {
                    HStack(spacing: 6) {
                        KIcon(.close, size: .sm)
                        Text("Done")
                    }
                }
                .buttonStyle(KButtonStyle(kind: .ghost, size: .small))
                .keyboardShortcut(.cancelAction)
            }
            HStack(spacing: KSpace.x3) {
                KSegmented(selection: $history.comparison, options: NoteHistoryModel.Comparison.allCases.map { KOption($0, $0.title) })
                    .fixedSize()
                Text(history.isCurrent(version) ? "Same as now" : "Compared with now")
                    .kLineStyle(10)
                    .foregroundStyle(theme.muted)
                    .lineLimit(1)
                Spacer(minLength: KSpace.x2)
                KButton("Undo this change", icon: .undo, size: .small) {
                    Task { await history.undo(version) }
                }
                .fixedSize()
                .disabled(!history.canUndo(version))
                .kTooltip("Undo only what this change did, keeping later changes when they do not overlap")
                KButton("Restore this version", icon: .history, kind: .primary, size: .small) {
                    Task { await history.restore(version) }
                }
                .fixedSize()
                .disabled(history.isCurrent(version))
                .kTooltip("Put the whole note back as it was in this version, as a new version")
            }
            if let error = history.errorMessage {
                KBanner(.error, title: error, onClose: { history.errorMessage = nil })
            }
        }
        .padding(.horizontal, KSpace.x6)
        .padding(.vertical, KSpace.x4)
    }

    private var changes: some View {
        KScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(history.diff(version)) { line in
                    DiffLineView(line: line)
                }
            }
            .padding(.vertical, KSpace.x3)
        }
        .accessibilityLabel("Changes between this version and the note now")
    }

    private var sideBySide: some View {
        HStack(spacing: 0) {
            column("This version", text: version.body)
            Rectangle().fill(theme.line).frame(width: 1)
            column("Now", text: history.currentBody())
        }
    }

    private func column(_ title: String, text: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            KSectionLabel(title)
                .padding(.horizontal, 18)
                .padding(.vertical, 10)
            Rectangle().fill(theme.line).frame(height: 1)
            KScrollView {
                Text(text)
                    .font(KFont.mono(13))
                    .foregroundStyle(theme.fg)
                    .lineSpacing(3)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(18)
            }
        }
        .frame(minWidth: 240, maxWidth: .infinity)
    }
}

/// Asks before an undo or a redo that would overwrite later changes to the note. The question
/// floats above the window, so it shows wherever the modifier sits.
struct UndoConflictAlert: ViewModifier {
    @Bindable var history: NoteHistoryModel

    func body(content: Content) -> some View {
        content.background(UndoConflictPresenter(history: history, conflict: history.conflict))
    }
}

private struct UndoConflictPresenter: NSViewRepresentable {
    let history: NoteHistoryModel
    let conflict: NoteHistoryModel.Conflict?

    func makeCoordinator() -> KFloatingPresenter { KFloatingPresenter() }

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        context.coordinator.anchor = view
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        let presenter = context.coordinator
        presenter.anchor = view
        let history = history
        presenter.onDismiss = { history.conflict = nil }
        guard let conflict else {
            if presenter.isShowing { presenter.dismiss(notify: false) }
            return
        }
        guard !presenter.isShowing else { return }
        let card = KFloatingCard(padding: KSpace.x6) {
            UndoConflictCard(conflict: conflict) {
                presenter.dismiss()
            } confirm: {
                presenter.dismiss()
                Task { await history.confirmForcedUndo(conflict) }
            }
        }
        let content = AnyView(card.environment(\.self, context.environment).knodeTheme())
        DispatchQueue.main.async {
            guard history.conflict == conflict, !presenter.isShowing, let window = view.window else { return }
            // Centred on the window, from the card's width and about its height.
            let m = KFloatingPresenter.margin
            let size = CGSize(width: UndoConflictCard.width + 2 * KSpace.x6 + 2 * m, height: 200)
            let frame = window.frame
            presenter.present(content, edge: .below, at: NSPoint(x: frame.midX - size.width / 2 + m, y: frame.midY + size.height / 2 - m))
        }
    }

    static func dismantleNSView(_ view: NSView, coordinator: KFloatingPresenter) {
        coordinator.dismiss(notify: false)
    }
}

private struct UndoConflictCard: View {
    static let width: CGFloat = 380
    let conflict: NoteHistoryModel.Conflict
    var cancel: () -> Void
    var confirm: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x5) {
            KSheetHeader(
                kicker: "History",
                title: conflict.isRedo ? "Redo this change anyway?" : "Undo this change anyway?",
                message: "\(conflict.message) To go back to a version without undoing a single change, use Restore this version in the history."
            )
            HStack(spacing: KSpace.x2) {
                Spacer()
                KButton("Cancel", kind: .secondary, action: cancel)
                Button(action: confirm) { Text(conflict.isRedo ? "Redo anyway" : "Undo anyway") }
                    .buttonStyle(KButtonStyle(kind: .destructive))
                    .keyboardShortcut(.defaultAction)
            }
        }
        .frame(width: Self.width)
    }
}

/// One line of a comparison: removed lines are only in the version, added lines only in the note
/// now. Added lines are tinted indigo, removed lines muted and struck through.
struct DiffLineView: View {
    let line: LineDiff.Line
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: KSpace.x2) {
            Text(marker)
                .foregroundStyle(markerColor)
                .frame(width: 12)
            Text(line.text.isEmpty ? " " : line.text)
                .foregroundStyle(line.kind == .removed ? theme.muted : theme.fg)
                .strikethrough(line.kind == .removed && !line.text.isEmpty, color: theme.muted.opacity(0.7))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(KFont.mono(13))
        .padding(.horizontal, 18)
        .padding(.vertical, 2)
        .background(background)
        .overlay(alignment: .leading) {
            if line.kind == .added { Rectangle().fill(theme.indigo).frame(width: 2) }
        }
        .accessibilityLabel(accessibility)
    }

    private var marker: String {
        switch line.kind {
        case .same: " "
        case .removed: "-"
        case .added: "+"
        }
    }

    private var markerColor: Color {
        switch line.kind {
        case .same: theme.muted
        case .removed: theme.muted
        case .added: theme.indigo
        }
    }

    private var background: Color {
        switch line.kind {
        case .same: .clear
        case .removed: theme.fg.opacity(theme.isDark ? 0.03 : 0.035)
        case .added: theme.indigo.opacity(theme.isDark ? 0.12 : 0.14)
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
