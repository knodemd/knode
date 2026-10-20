import KnodeBrand
import SwiftUI

/// How the note is shown: the Markdown source, the preview, or both side by side.
enum EditorLayout: String, CaseIterable, Identifiable {
    case edit, split, preview
    var id: Self { self }

    var title: String {
        switch self {
        case .edit: "Edit"
        case .split: "Edit and Preview"
        case .preview: "Preview"
        }
    }

    var systemImage: String {
        switch self {
        case .edit: "text.alignleft"
        case .split: "rectangle.split.2x1"
        case .preview: "eye"
        }
    }
}

struct NoteView: View {
    @Bindable var editor: NoteEditorModel
    @Environment(AppModel.self) private var model
    @AppStorage("editorLayout") private var layout: EditorLayout = .split

    var body: some View {
        @Bindable var model = model
        Group {
            if let history = model.history, let version = history.selectedVersion {
                VersionComparisonView(history: history, version: version)
            } else {
                editorBody
            }
        }
        .background(PaperBackground())
        .background {
            if let history = model.history {
                Color.clear.modifier(UndoConflictAlert(history: history))
            }
        }
        .inspector(isPresented: $model.isShowingHistory) {
            if let history = model.history {
                HistoryPanel(history: history)
                    .inspectorColumnWidth(min: 240, ideal: 280, max: 380)
            }
        }
        .toolbar {
            ToolbarItem {
                Picker("Layout", selection: $layout) {
                    ForEach(EditorLayout.allCases) { layout in
                        Label(layout.title, systemImage: layout.systemImage).tag(layout)
                    }
                }
                .pickerStyle(.segmented)
                .help("Edit, preview, or both")
                .disabled(model.history?.selectedVersion != nil)
            }
            ToolbarItem {
                Toggle(isOn: $model.isShowingHistory) {
                    Label("History", systemImage: "clock.arrow.circlepath")
                }
                .help("Show who changed this note, and when (Command-Y)")
            }
        }
        .onDisappear { Task { await editor.save() } }
    }

    private var editorBody: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            switch layout {
            case .edit: source
            case .preview: preview
            case .split:
                HSplitView {
                    source.frame(minWidth: 240)
                    preview.frame(minWidth: 240)
                }
            }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            TextField("Title", text: $editor.title)
                .textFieldStyle(.plain)
                .font(.title.weight(.semibold))
            HStack(spacing: 6) {
                Text("Edited \(editor.updatedAt, format: .relative(presentation: .named))")
                if editor.hasUnsavedChanges {
                    Text("Saving").foregroundStyle(.tertiary)
                }
                if let error = editor.errorMessage ?? model.history?.errorMessage {
                    Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.red)
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 16)
    }

    private var source: some View {
        TextEditor(text: $editor.body)
            .font(.system(size: 14, design: .monospaced))
            .scrollContentBackground(.hidden)
            .padding(.horizontal, 18)
            .padding(.vertical, 12)
            .accessibilityLabel("Markdown")
    }

    private var preview: some View {
        ScrollView {
            MarkdownPreview(markdown: editor.body)
                .padding(.horizontal, 24)
                .padding(.vertical, 18)
                .frame(maxWidth: 720, alignment: .leading)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// Draws a Markdown body as it reads. `[[links]]` open the note they name.
struct MarkdownPreview: View {
    let markdown: String
    @Environment(AppModel.self) private var model
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(MarkdownBlock.parse(markdown).enumerated()), id: \.offset) { _, block in
                view(for: block)
            }
        }
        .textSelection(.enabled)
        .tint(colorScheme == .dark ? KnodeColor.indigo : KnodeColor.ink)
        .environment(\.openURL, OpenURLAction { url in
            if let title = InlineMarkdown.linkedTitle(url) {
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
            Text(InlineMarkdown.render(text))
                .font(headingFont(level))
                .padding(.top, level <= 2 ? 8 : 4)
        case .paragraph(let text):
            Text(InlineMarkdown.render(text))
                .lineSpacing(3)
        case .quote(let text):
            HStack(alignment: .top, spacing: 10) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(KnodeColor.indigo)
                    .frame(width: 3)
                Text(InlineMarkdown.render(text))
                    .foregroundStyle(.secondary)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .bullet(let text, let checked):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                if let checked {
                    Image(systemName: checked ? "checkmark.square.fill" : "square")
                        .foregroundStyle(checked ? KnodeColor.indigo : .secondary)
                } else {
                    Text("•").foregroundStyle(.secondary)
                }
                Text(InlineMarkdown.render(text))
                    .strikethrough(checked == true, color: .secondary)
            }
            .padding(.leading, 4)
        case .numbered(let number, let text):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("\(number).").monospacedDigit().foregroundStyle(.secondary)
                Text(InlineMarkdown.render(text))
            }
            .padding(.leading, 4)
        case .code(_, let text):
            Text(text)
                .font(.system(size: 13, design: .monospaced))
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
                .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        case .rule:
            Divider().padding(.vertical, 6)
        }
    }

    private func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: .title.weight(.bold)
        case 2: .title2.weight(.semibold)
        case 3: .title3.weight(.semibold)
        default: .headline
        }
    }
}
