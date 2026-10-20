import KnodeBrand
import SwiftUI

// MARK: - Search in the window

extension SearchMode {
    /// The Knode icon of the mode.
    var glyph: KGlyph {
        switch self {
        case .hybrid: .search
        case .words: .text
        case .meaning: .knode
        }
    }
}

/// The choice next to the search field: search by words and meaning, by words, or by meaning.
struct SearchModeMenu: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        KDropdown(selection: $model.searchMode,
                  options: SearchMode.allCases.map { KOption($0, $0.title, icon: $0.glyph) },
                  size: .small)
            .fixedSize()
            .kTooltip("Search by \(model.searchMode.title.lowercased()): \(model.searchMode.explanation)")
            .accessibilityLabel("Search by")
            .disabled(model.selectedSpace == nil)
    }
}

/// Why the search ran by words, or that some notes were found by words only, as the store said.
struct SearchNoteBanner: View {
    let note: String
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: KSpace.x2) {
            KIcon(.info, size: .xs, color: theme.muted).alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
            Text(note).font(KFont.ui(12)).foregroundStyle(theme.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, KSpace.x3)
        .padding(.vertical, KSpace.x2)
        .background(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous).fill(theme.hoverFill))
        .padding(.horizontal, KSpace.x2)
        .padding(.vertical, KSpace.x1)
    }
}

/// One search result: the title, the snippet with the words found in indigo, and how it was found.
struct SearchResultRow: View {
    let hit: SearchHit
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                if hit.summary.kind == NodeKind.canvas {
                    KIcon(.canvas, size: .xs, color: theme.isDark ? theme.indigo : theme.fg)
                }
                Text(hit.summary.title.isEmpty ? "Untitled" : hit.summary.title)
                    .font(KFont.ui(13, .semibold))
                    .foregroundStyle(theme.fg)
                    .lineLimit(1)
            }
            if !hit.snippet.isEmpty {
                Text(Self.snippet(hit.snippetRuns, theme: theme))
                    .font(KFont.ui(12))
                    .foregroundStyle(theme.muted)
                    .lineLimit(3)
            }
            HStack(spacing: 6) {
                if let rank = hit.wordRank {
                    MatchBadge(title: "Words", rank: rank, glyph: SearchMode.words.glyph)
                        .kTooltip("Found by words: place \(rank) among the notes with the words you typed")
                }
                if let rank = hit.meaningRank {
                    MatchBadge(title: "Meaning", rank: rank, glyph: SearchMode.meaning.glyph)
                        .kTooltip("Found by meaning: place \(rank) among the notes nearest to what you typed")
                }
                Spacer(minLength: 4)
                Text(hit.summary.updatedAt, format: .relative(presentation: .named))
                    .kLineStyle(10)
                    .foregroundStyle(theme.muted)
            }
        }
        .padding(.vertical, 2)
    }

    /// The snippet with each match marked: indigo on ink; ink on an indigo wash on paper, where
    /// indigo is never small text.
    static func snippet(_ runs: [(text: String, bold: Bool)], theme: KTheme) -> AttributedString {
        var text = AttributedString()
        for run in runs {
            var part = AttributedString(run.text.replacing("\n", with: " "))
            if run.bold {
                part.font = KFont.ui(12, .semibold)
                if theme.isDark {
                    part.foregroundColor = theme.indigo
                } else {
                    part.foregroundColor = theme.fg
                    part.backgroundColor = theme.indigo.opacity(0.22)
                }
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
    let glyph: KGlyph

    var body: some View {
        KChip("\(title) \(rank)", icon: glyph, kind: .accent)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(title), place \(rank)")
    }
}

// MARK: - Settings

/// Settings > Search: search by meaning on or off, the search model and how far each open space
/// is indexed.
struct SearchSettingsView: View {
    @Environment(SearchIndexModel.self) private var index

    var body: some View {
        KSettingsPane {
            KSettingsSection("Search") {
                KExplanation("Knode finds notes by the words they contain and by what they mean. Search by meaning runs on this Mac, in Knode Service, with a small model: nothing is sent anywhere.")
                KHairline()
                KSettingToggle("Search by meaning",
                               detail: "The first time, Knode downloads its search model once, about 490 MB. Off, it downloads and indexes nothing, and search is by words.",
                               isOn: Binding(get: { index.isEnabled }, set: { on in Task { await index.setEnabled(on) } }))
                    .disabled(index.isBusy)
            }
            switch index.state {
            case .loading:
                HStack(spacing: KSpace.x2) { KSpinner(); KExplanation("Asking Knode Service") }
            case .serviceStopped:
                SearchServiceStoppedNotice()
            case .loaded:
                if let status = index.status {
                    SearchIndexSections(status: status)
                }
            }
            if let error = index.errorMessage {
                KErrorText(error)
            }
        }
        .task { await index.watch() }
    }
}

/// The model and the open spaces, while Knode Service runs.
struct SearchIndexSections: View {
    @Environment(SearchIndexModel.self) private var index
    @Environment(\.kTheme) private var theme
    let status: IndexStatus

    var body: some View {
        KSettingsSection("Search model") {
            HStack(spacing: KSpace.x2) {
                KIcon(status.isActive ? SearchMode.meaning.glyph : SearchMode.words.glyph, size: .sm,
                      color: status.isActive ? (theme.isDark ? theme.indigo : theme.fg) : theme.muted)
                Text(status.summary).font(KFont.ui(13, .medium)).fixedSize(horizontal: false, vertical: true)
            }
            if status.isActive {
                KHairline()
                KSettingRow("Model") { Text(status.model.name).font(KFont.mono(12)).foregroundStyle(theme.muted) }
                VStack(alignment: .leading, spacing: KSpace.x2) {
                    KSettingRow("State") {
                        Text(status.model.summary).font(KFont.ui(12)).foregroundStyle(theme.muted)
                            .multilineTextAlignment(.trailing)
                    }
                    if let progress = status.model.progress {
                        KProgress(value: progress).accessibilityLabel("Search model download")
                    }
                }
                if let paused = status.paused {
                    KSettingRow("Paused") {
                        Text(paused.prefix(1).uppercased() + paused.dropFirst()).font(KFont.ui(12)).foregroundStyle(theme.muted)
                    }
                }
            }
        }
        if status.isActive {
            KSettingsSection("Open spaces") {
                KButton("Index now", icon: .refresh, size: .small) { Task { await index.indexNow() } }
                    .disabled(index.isBusy)
                    .kTooltip("Index every open space now, even on battery, and try a failed download again")
            } content: {
                if status.spaces.isEmpty {
                    KExplanation("No spaces open in Knode Service.")
                }
                ForEach(Array(status.spaces.enumerated()), id: \.element.id) { offset, space in
                    if offset > 0 { KHairline() }
                    SpaceIndexRow(space: space)
                }
                KHairline()
                KExplanation("Knode indexes each open space in the background, and waits while this Mac runs on battery.")
            }
        }
    }
}

/// How far one open space is indexed.
struct SpaceIndexRow: View {
    let space: IndexStatus.SpaceIndex

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSettingRow(space.name, detail: space.summary) {
                if space.indexing { KSpinner(size: 12) }
            }
            if space.waiting > 0, space.nodes > 0 {
                KProgress(value: Double(space.indexed) / Double(space.nodes))
                    .accessibilityLabel("\(space.name) indexed")
            }
        }
        .kTooltip(space.space)
    }
}

/// Search by meaning needs Knode Service.
struct SearchServiceStoppedNotice: View {
    @Environment(SearchIndexModel.self) private var index

    var body: some View {
        KBanner(.warning, title: "Knode Service is not running",
                message: "Search by meaning runs in Knode Service. Until it runs, the app searches by words.", actions: {
            KButton("Start", icon: .play, kind: .primary, size: .small) {
                Task { await index.startServiceAndLoad() }
            }
            .disabled(index.isBusy)
        })
    }
}

// MARK: - Menu bar

/// Search by meaning in the menu bar panel: the model, each open space, on or off, and Index Now.
struct SearchIndexMenu: View {
    let status: IndexStatus
    let index: SearchIndexModel
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSectionLabel("Search") {
                KButton("Index now", icon: .refresh, kind: .ghost, size: .small) { Task { await index.indexNow() } }
                    .disabled(index.isBusy || !status.isActive)
            }
            VStack(alignment: .leading, spacing: KSpace.x2) {
                KExplanation(status.summary)
                if status.isActive {
                    Text("Model: \(status.model.summary)").font(KFont.ui(12)).foregroundStyle(theme.muted)
                    if let progress = status.model.progress { KProgress(value: progress) }
                    ForEach(status.spaces) { space in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(spacing: KSpace.x2) {
                                Text(space.name).font(KFont.ui(12, .medium)).lineLimit(1)
                                Spacer(minLength: KSpace.x2)
                                Text(space.summary).font(KFont.ui(11)).foregroundStyle(theme.muted).lineLimit(1)
                            }
                            if space.waiting > 0, space.nodes > 0 {
                                KProgress(value: Double(space.indexed) / Double(space.nodes))
                            }
                        }
                    }
                }
                KToggle("Search by meaning", isOn: Binding(
                    get: { index.isEnabled },
                    set: { on in Task { await index.setEnabled(on) } }
                ))
                .font(KFont.ui(13))
                .disabled(index.isBusy || !status.available)
            }
            .padding(.horizontal, KSpace.x1)
        }
        .onAppear { index.refreshEnabled() }
    }
}
