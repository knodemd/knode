import KnodeBrand
import SwiftUI

// MARK: - Toolbar search

/// The menu next to the toolbar search: search by words and meaning, by words, or by meaning.
struct SearchModeMenu: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        Menu {
            Picker("Search By", selection: $model.searchMode) {
                ForEach(SearchMode.allCases) { mode in
                    Label(mode.title, systemImage: mode.systemImage).tag(mode)
                }
            }
            .pickerStyle(.inline)
        } label: {
            Label("Search By \(model.searchMode.title)", systemImage: model.searchMode.systemImage)
        }
        .help("Search by \(model.searchMode.title.lowercased()): \(model.searchMode.explanation)")
        .disabled(model.selectedSpace == nil)
    }
}

/// Why the search ran by words, or that some notes were found by words only, as the store said.
struct SearchNoteBanner: View {
    let note: String

    var body: some View {
        Label {
            Text(note).fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: "info.circle")
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }
}

/// One search result: the title, the snippet with the words found in bold, and how it was found.
struct SearchResultRow: View {
    let hit: SearchHit

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 5) {
                if hit.summary.kind == NodeKind.canvas {
                    Image(systemName: "rectangle.on.rectangle.angled")
                        .foregroundStyle(KnodeColor.indigo)
                        .accessibilityLabel("Canvas")
                }
                Text(hit.summary.title.isEmpty ? "Untitled" : hit.summary.title)
                    .font(.headline)
                    .lineLimit(1)
            }
            if !hit.snippet.isEmpty {
                Text(snippet)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
            }
            HStack(spacing: 6) {
                if let rank = hit.wordRank {
                    MatchBadge(title: "Words", rank: rank, systemImage: SearchMode.words.systemImage)
                        .help("Found by words: place \(rank) among the notes with the words you typed")
                }
                if let rank = hit.meaningRank {
                    MatchBadge(title: "Meaning", rank: rank, systemImage: SearchMode.meaning.systemImage)
                        .help("Found by meaning: place \(rank) among the notes nearest to what you typed")
                }
                Spacer(minLength: 4)
                Text(hit.summary.updatedAt, format: .relative(presentation: .named))
                    .foregroundStyle(.tertiary)
            }
            .font(.caption)
        }
        .padding(.vertical, 4)
    }

    /// The snippet with each match in bold.
    private var snippet: AttributedString {
        var text = AttributedString()
        for run in hit.snippetRuns {
            var part = AttributedString(run.text.replacing("\n", with: " "))
            if run.bold {
                part.inlinePresentationIntent = .stronglyEmphasized
                part.foregroundColor = .primary
            }
            text += part
        }
        return text
    }
}

/// How a note was found, and its place in that list.
struct MatchBadge: View {
    let title: String
    let rank: Int
    let systemImage: String

    var body: some View {
        Label("\(title) \(rank)", systemImage: systemImage)
            .labelStyle(.titleAndIcon)
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(KnodeColor.indigo.opacity(0.12), in: Capsule())
            .foregroundStyle(KnodeColor.indigo)
            .accessibilityLabel("\(title), place \(rank)")
    }
}

// MARK: - Settings

/// Settings > Search: search by meaning on or off, the search model and how far each open space
/// is indexed.
struct SearchSettingsView: View {
    @Environment(SearchIndexModel.self) private var index

    var body: some View {
        Form {
            Section {
                Text("Knode finds notes by the words they contain and by what they mean. Search by meaning runs on this Mac, in Knode Service, with a small model: nothing is sent anywhere.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Toggle(isOn: Binding(get: { index.isEnabled }, set: { on in Task { await index.setEnabled(on) } })) {
                    Text("Search by Meaning")
                    Text("The first time, Knode downloads its search model once, about 490 MB. Off, it downloads and indexes nothing, and search is by words.")
                }
                .disabled(index.isBusy)
            }
            switch index.state {
            case .loading:
                Section { ProgressView().frame(maxWidth: .infinity) }
            case .serviceStopped:
                Section { SearchServiceStoppedNotice() }
            case .loaded:
                if let status = index.status {
                    SearchIndexSections(status: status)
                }
            }
            if let error = index.errorMessage {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.red)
                }
            }
        }
        .formStyle(.grouped)
        .task { await index.watch() }
    }
}

/// The model and the open spaces, while Knode Service runs.
struct SearchIndexSections: View {
    @Environment(SearchIndexModel.self) private var index
    let status: IndexStatus

    var body: some View {
        Section {
            Label(status.summary, systemImage: status.isActive ? "sparkle.magnifyingglass" : "textformat")
            if status.isActive {
                LabeledContent("Model", value: status.model.name)
                VStack(alignment: .leading, spacing: 6) {
                    LabeledContent("State", value: status.model.summary)
                    if let progress = status.model.progress {
                        ProgressView(value: progress)
                            .accessibilityLabel("Search model download")
                    }
                }
                if let paused = status.paused {
                    LabeledContent("Paused", value: paused.prefix(1).uppercased() + paused.dropFirst())
                }
            }
        } header: {
            Text("Search Model")
        }
        if status.isActive {
            Section {
                if status.spaces.isEmpty {
                    Text("No spaces open in Knode Service.").foregroundStyle(.secondary)
                }
                ForEach(status.spaces) { space in
                    SpaceIndexRow(space: space)
                }
            } header: {
                Text("Open Spaces")
            } footer: {
                HStack {
                    Text("Knode indexes each open space in the background, and waits while this Mac runs on battery.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Index Now", systemImage: "arrow.clockwise") {
                        Task { await index.indexNow() }
                    }
                    .disabled(index.isBusy)
                    .help("Index every open space now, even on battery, and try a failed download again")
                }
            }
        }
    }
}

/// How far one open space is indexed.
struct SpaceIndexRow: View {
    let space: IndexStatus.SpaceIndex

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            LabeledContent(space.name, value: space.summary)
            if space.waiting > 0, space.nodes > 0 {
                ProgressView(value: Double(space.indexed), total: Double(space.nodes))
                    .accessibilityLabel("\(space.name) indexed")
            }
        }
        .help(space.space)
    }
}

/// Search by meaning needs Knode Service.
struct SearchServiceStoppedNotice: View {
    @Environment(SearchIndexModel.self) private var index

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Knode Service is not running", systemImage: "pause.circle")
                .font(.headline)
            Text("Search by meaning runs in Knode Service. Until it runs, the app searches by words.")
                .font(.callout)
                .foregroundStyle(.secondary)
            Button("Start Knode Service", systemImage: "play.circle") {
                Task { await index.startServiceAndLoad() }
            }
            .disabled(index.isBusy)
        }
    }
}

// MARK: - Menu bar

/// Search by meaning in the menu bar menu: the model, each open space, on or off, and Index Now.
struct SearchIndexMenu: View {
    let status: IndexStatus
    let index: SearchIndexModel

    var body: some View {
        Section("Search") {
            Text(status.summary)
            if status.isActive {
                Text("Model: \(status.model.summary)")
                ForEach(status.spaces) { space in
                    Text("\(space.name): \(space.summary)")
                }
            }
            Toggle("Search by Meaning", systemImage: "sparkle.magnifyingglass", isOn: Binding(
                get: { index.isEnabled },
                set: { on in Task { await index.setEnabled(on) } }
            ))
            .disabled(index.isBusy || !status.available)
            Button("Index Now", systemImage: "arrow.clockwise") {
                Task { await index.indexNow() }
            }
            .disabled(index.isBusy || !status.isActive)
        }
        .onAppear { index.refreshEnabled() }
    }
}
