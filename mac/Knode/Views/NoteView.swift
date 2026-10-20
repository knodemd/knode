import AppKit
import KnodeBrand
import SwiftUI

/// How the note is shown: the Markdown source, the preview, or both side by side.
enum EditorLayout: String, CaseIterable, Identifiable {
    case edit, split, preview
    var id: Self { self }

    var title: String {
        switch self {
        case .edit: "Source"
        case .split: "Split"
        case .preview: "Preview"
        }
    }

    var glyph: KGlyph {
        switch self {
        case .edit: .code
        case .split: .split
        case .preview: .show
        }
    }
}

/// Which panel the inspector of the note shows.
enum InspectorPanel: Hashable {
    case links, attachments, history
}

struct NoteView: View {
    @Bindable var editor: NoteEditorModel
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @AppStorage("editorLayout") private var layout: EditorLayout = .split
    @AppStorage("editorSplit") private var split = 0.5
    @State private var isDropTargeted = false

    var body: some View {
        HStack(spacing: 0) {
            Group {
                if let history = model.history, let version = history.selectedVersion {
                    VersionComparisonView(history: history, version: version)
                } else {
                    editorBody
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            if let panel = inspector {
                KDivider(.vertical)
                InspectorSlot(panel: panel)
                    .frame(width: 300)
                    .transition(reduceMotion ? .opacity : .move(edge: .trailing))
            }
        }
        .animation(KMotion.animation(reduceMotion: reduceMotion), value: inspector)
        .background(theme.bg)
        .background {
            if let history = model.history {
                Color.clear.modifier(UndoConflictAlert(history: history))
            }
        }
        .onDisappear { Task { await editor.save() } }
    }

    /// The one panel shown beside the note, if any.
    private var inspector: InspectorPanel? {
        if model.isShowingAttachments, model.attachments != nil { return .attachments }
        if model.isShowingLinks, model.links != nil { return .links }
        if model.isShowingHistory, model.history != nil { return .history }
        return nil
    }

    private var editorBody: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            KDivider()
            switch layout {
            case .edit: source
            case .preview: preview
            case .split:
                KSplit(fraction: $split) { source } trailing: { preview }
            }
        }
        // Files dropped on the note, outside the text, are attached and shown where it ends.
        .overlay {
            if isDropTargeted { DropHighlight() }
        }
        .onDrop(of: [.fileURL], isTargeted: $isDropTargeted) { providers in
            guard model.canAttach else { return false }
            Task { await model.attachFilesAtEnd(await DroppedItems.fileURLs(providers)) }
            return true
        }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: KSpace.x4) {
            VStack(alignment: .leading, spacing: KSpace.x2) {
                TextField("", text: $editor.title, prompt: Text("Untitled").foregroundStyle(theme.muted))
                    .textFieldStyle(.plain)
                    .font(KFont.ui(24, .semibold))
                    .foregroundStyle(theme.fg)
                    .accessibilityLabel("Title")
                meta
            }
            Spacer(minLength: 0)
            KSegmented(selection: $layout, options: EditorLayout.allCases.map { KOption($0, $0.title, icon: $0.glyph) })
                .disabled(model.history?.selectedVersion != nil)
                .kTooltip("Source, preview, or both")
        }
        .padding(.horizontal, 28)
        .padding(.top, KSpace.x5)
        .padding(.bottom, KSpace.x4)
    }

    private var meta: some View {
        HStack(spacing: KSpace.x3) {
            Text("Edited \(editor.updatedAt, format: .relative(presentation: .named))")
                .kLineStyle(10.5)
                .foregroundStyle(theme.muted)
            if editor.hasUnsavedChanges {
                Text("Saving").kLineStyle(10.5).foregroundStyle(theme.muted.opacity(0.7))
            }
            if let writer = editor.otherWriter {
                WriterChip(name: writer.editorName)
                    .transition(.opacity)
            }
            if let error = editor.errorMessage ?? model.history?.errorMessage {
                HStack(spacing: 6) {
                    KIcon(.warning, size: .xs, color: theme.danger)
                    Text(error).font(KFont.ui(12)).foregroundStyle(theme.danger).lineLimit(1)
                }
            }
        }
        .animation(.easeOut(duration: KMotion.standard), value: editor.otherWriter)
    }

    private var source: some View {
        MarkdownSourceEditor(editor: editor, attachments: attachmentDrop)
            .id(editor.nodeID)
    }

    /// Files dropped in the text and images pasted into it are attached to the note and written
    /// where they went.
    private var attachmentDrop: AttachmentDrop? {
        guard model.canAttach else { return nil }
        let model = model
        return AttachmentDrop(
            files: { await model.attachFiles($0) },
            image: { await model.attachPastedImage($0, named: $1) }
        )
    }

    private var preview: some View {
        KScrollView {
            MarkdownPreview(markdown: editor.body, attachments: model.attachments)
                .padding(.horizontal, 32)
                .padding(.vertical, KSpace.x6)
                .frame(maxWidth: 760, alignment: .leading)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// Who else is writing the note now: an indigo chip that breathes.
private struct WriterChip: View {
    let name: String
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var dim = false

    var body: some View {
        KChip("\(name) is editing", icon: .edit, kind: .accent)
            .opacity(dim ? 0.55 : 1)
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true)) { dim = true }
            }
            .accessibilityLabel("\(name) is editing this note")
    }
}

/// Two panes side by side with a line between them that drags.
struct KSplit<Leading: View, Trailing: View>: View {
    @Binding var fraction: Double
    var minimum: CGFloat = 240
    @ViewBuilder var leading: Leading
    @ViewBuilder var trailing: Trailing
    @Environment(\.kTheme) private var theme
    @State private var hovering = false
    @State private var start: Double?

    var body: some View {
        GeometryReader { geo in
            let width = geo.size.width
            let low = min(minimum / max(width, 1), 0.5), high = 1 - low
            let f = min(max(fraction, low), high)
            HStack(spacing: 0) {
                leading.frame(width: (width * f).rounded())
                Rectangle()
                    .fill(hovering || start != nil ? theme.indigo : theme.line)
                    .frame(width: 1)
                    .overlay {
                        Color.clear
                            .frame(width: 9)
                            .contentShape(Rectangle())
                            .onHover { inside in
                                hovering = inside
                                if inside { NSCursor.resizeLeftRight.push() } else { NSCursor.pop() }
                            }
                            .gesture(DragGesture(minimumDistance: 1, coordinateSpace: .global)
                                .onChanged { value in
                                    let base = start ?? f
                                    if start == nil { start = f }
                                    fraction = min(max(base + value.translation.width / width, low), high)
                                }
                                .onEnded { _ in start = nil })
                    }
                    .accessibilityHidden(true)
                trailing.frame(maxWidth: .infinity)
            }
        }
    }
}

/// The panel on the right of the note: links, attachments or history, one at a time.
private struct InspectorSlot: View {
    let panel: InspectorPanel
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .center) {
                KTabs(selection: selection, options: options, spacing: KSpace.x4)
                Spacer(minLength: KSpace.x2)
                KIconButton(.close, title: "Close panel", size: .small) {
                    model.isShowingAttachments = false
                    model.isShowingLinks = false
                    model.isShowingHistory = false
                }
            }
            .padding(.leading, KSpace.x4)
            .padding(.trailing, KSpace.x2)
            .padding(.top, KSpace.x3)
            KDivider()
            Group {
                switch panel {
                case .attachments:
                    if let attachments = model.attachments { NoteAttachmentsPanel(attachments: attachments) }
                case .links:
                    if let links = model.links { NoteLinksPanel(links: links) }
                case .history:
                    if let history = model.history { HistoryPanel(history: history) }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // Lists and forms inside the panels draw on the panel, not on the system background.
            .scrollContentBackground(.hidden)
            .background(theme.panel)
        }
        .background(theme.panel)
    }

    private var options: [KOption<InspectorPanel>] {
        var options: [KOption<InspectorPanel>] = []
        if model.links != nil { options.append(KOption(.links, "Links", icon: .link)) }
        if model.canAttach { options.append(KOption(.attachments, "Files", icon: .attach)) }
        options.append(KOption(.history, "History", icon: .history))
        return options
    }

    private var selection: Binding<InspectorPanel> {
        Binding(get: { panel }, set: { panel in
            switch panel {
            case .links: model.isShowingLinks = true
            case .attachments: model.isShowingAttachments = true
            case .history: model.isShowingHistory = true
            }
        })
    }
}

/// Draws a Markdown body as it reads, in the prose of knode.md. `[[links]]` open the note they
/// name.
struct MarkdownPreview: View {
    let markdown: String
    /// The note's attachments, shown where the body says `![[name]]`.
    var attachments: NoteAttachmentsModel?
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme

    static let bodySize: CGFloat = 15

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(Array(MarkdownBlock.parse(markdown).enumerated()), id: \.offset) { _, block in
                view(for: block)
            }
        }
        .textSelection(.enabled)
        .tint(theme.isDark ? theme.indigo : theme.fg)
        .environment(\.openURL, OpenURLAction { url in
            if let title = InlineMarkdown.linkedTitle(url) {
                // `[[name]]` opens the note's attachment of that name, else the note so titled.
                if model.openAttachment(named: title) { return .handled }
                Task { await model.openLinkedNode(titled: title) }
                return .handled
            }
            return .systemAction
        })
    }

    @ViewBuilder
    private func view(for block: MarkdownBlock) -> some View {
        switch block {
        case .heading(let level, let text):
            heading(level, text)
        case .paragraph(let text):
            Text(prose(text))
                .lineSpacing(6)
                .fixedSize(horizontal: false, vertical: true)
        case .quote(let text):
            HStack(alignment: .top, spacing: KSpace.x3) {
                Capsule().fill(theme.indigo).frame(width: 3)
                Text(prose(text))
                    .lineSpacing(6)
                    .italic()
            }
            .fixedSize(horizontal: false, vertical: true)
        case .bullet(let text, let checked):
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                if let checked {
                    KCheckmark(isOn: checked, side: 15)
                        .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 2 }
                } else {
                    Circle().fill(theme.indigo).frame(width: 5, height: 5)
                        .alignmentGuide(.firstTextBaseline) { $0[.bottom] + 4 }
                }
                Text(prose(text, done: checked == true))
                    .lineSpacing(6)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.leading, 6)
        case .numbered(let number, let text):
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text("\(number).")
                    .font(KFont.ui(Self.bodySize, .medium))
                    .monospacedDigit()
                    .foregroundStyle(theme.isDark ? theme.indigo : theme.fg)
                Text(prose(text))
                    .lineSpacing(6)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.leading, 6)
        case .code(_, let text):
            ScrollView(.horizontal) {
                Text(text)
                    .font(KFont.mono(13))
                    .foregroundStyle(theme.fg)
                    .lineSpacing(4)
                    .padding(.horizontal, 18)
                    .padding(.vertical, 16)
            }
            .scrollIndicators(.never)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: KRadius.control, style: .continuous).fill(theme.panel))
            .overlay(RoundedRectangle(cornerRadius: KRadius.control, style: .continuous).strokeBorder(theme.line))
        case .rule:
            KDivider().padding(.vertical, KSpace.x2)
        case .embed(let name):
            AttachmentEmbed(name: name, attachments: attachments)
        }
    }

    @ViewBuilder
    private func heading(_ level: Int, _ text: String) -> some View {
        switch level {
        case 1:
            Text(prose(text, size: 26, weight: .bold, color: theme.fg))
                .padding(.top, KSpace.x2)
        case 2:
            Text(prose(text, size: 20, weight: .semibold, color: theme.fg))
                .padding(.top, KSpace.x3)
        case 3:
            Text(prose(text, size: 17, weight: .semibold, color: theme.fg))
                .padding(.top, KSpace.x2)
        default:
            Text(text.uppercased())
                .kLineStyle(12)
                .foregroundStyle(theme.muted)
                .padding(.top, KSpace.x2)
        }
    }

    /// Inline Markdown in the type of the preview: text in the muted ink like the web's prose,
    /// bold in the foreground, code in Geist Mono, `[[links]]` in indigo (ink with an indigo
    /// underline on paper).
    private func prose(_ text: String, size: CGFloat = bodySize, weight: KFont.Weight = .regular,
                       color: Color? = nil, done: Bool = false) -> AttributedString {
        var string = InlineMarkdown.render(text)
        let base = color ?? (done ? theme.muted.opacity(0.7) : theme.muted)
        for run in string.runs {
            let intent = run.inlinePresentationIntent ?? []
            var font = KFont.ui(size, weight)
            var foreground = base
            if intent.contains(.stronglyEmphasized) {
                font = KFont.ui(size, weight == .regular ? .semibold : .bold)
                foreground = color ?? theme.fg
            }
            if intent.contains(.emphasized) { font = font.italic() }
            if intent.contains(.code) {
                font = KFont.mono(size * 0.88)
                foreground = theme.fg
                string[run.range].backgroundColor = theme.fg.opacity(theme.isDark ? 0.08 : 0.06)
            }
            if let link = run.link {
                font = KFont.ui(size, weight == .regular ? .medium : weight)
                if InlineMarkdown.linkedTitle(link) != nil {
                    foreground = theme.isDark ? theme.indigo : theme.fg
                    if !theme.isDark {
                        string[run.range].underlineStyle = Text.LineStyle(pattern: .solid, color: theme.indigo)
                    }
                } else {
                    foreground = theme.fg
                    string[run.range].underlineStyle = Text.LineStyle(pattern: .solid, color: theme.indigo.opacity(0.6))
                }
            }
            if done { string[run.range].strikethroughStyle = Text.LineStyle(pattern: .solid, color: theme.muted) }
            string[run.range].font = font
            string[run.range].foregroundColor = foreground
        }
        return string
    }
}

extension Author {
    /// The name shown for whoever is editing: the agent's product name when Knode knows it.
    var editorName: String {
        kind == .agent ? ConnectableAgent(agent: name)?.title ?? name : name
    }
}
