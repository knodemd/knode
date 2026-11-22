import AppKit
import SwiftUI
import UniformTypeIdentifiers

extension MediaImporter {
    /// Adds the files to the space open in the app, in its selected project, then refreshes the
    /// list and opens the first node. What was left out is said in the app's error message.
    func addToSelectedSpace(_ urls: [URL], model: AppModel) async {
        guard let space = model.selectedSpace, let spaceURL = space.url else {
            model.errorMessage = "Open a space first to add photos, recordings or videos to it."
            return
        }
        let report = await add(urls, to: space.id, at: spaceURL, project: model.selectedProjectID)
        await model.refreshSpace(space.id)
        if let first = report.nodes.first { await model.openNode(first) }
        if !report.refused.isEmpty { model.errorMessage = report.refused.joined(separator: "\n") }
    }
}

/// The open panels for media.
@MainActor
enum MediaPanels {
    static func choosePhotos() -> [URL] {
        choose(title: "Add Photos", types: [.heic, .heif, .jpeg, .png])
    }

    static func chooseRecordings() -> [URL] {
        choose(title: "Add Recordings or Videos", types: [.audio, .movie])
    }

    private static func choose(title: String, types: [UTType]) -> [URL] {
        let panel = NSOpenPanel()
        panel.title = title
        panel.prompt = "Add"
        panel.message = "The originals are kept unchanged in the space and read on this Mac. Nothing is uploaded."
        panel.allowedContentTypes = types
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        return panel.runModal() == .OK ? panel.urls : []
    }
}

/// The File menu items that add media to the open space.
struct MediaMenuItems: View {
    let model: AppModel
    let media: MediaImporter

    var body: some View {
        Button("Add Photos…") {
            let urls = MediaPanels.choosePhotos()
            guard !urls.isEmpty else { return }
            Task { await media.addToSelectedSpace(urls, model: model) }
        }
        .disabled(model.selectedSpace?.url == nil || media.isAdding)

        Button("Add Recording or Video…") {
            let urls = MediaPanels.chooseRecordings()
            guard !urls.isEmpty else { return }
            Task { await media.addToSelectedSpace(urls, model: model) }
        }
        .disabled(model.selectedSpace?.url == nil || media.isAdding)
    }
}

/// Photos, audio files and videos dropped on the note list become nodes of the open space.
struct MediaDropTarget: ViewModifier {
    @Environment(AppModel.self) private var model
    @Environment(MediaImporter.self) private var media
    @State private var isTargeted = false

    func body(content: Content) -> some View {
        content
            .overlay {
                if isTargeted { DropHighlight() }
            }
            .onDrop(of: [.fileURL], isTargeted: $isTargeted) { providers in
                guard model.selectedSpace?.url != nil, DroppedItems.hasFiles(providers) else { return false }
                Task {
                    let urls = await DroppedItems.fileURLs(providers)
                    guard MediaImporter.accepts(urls) else { return }
                    await media.addToSelectedSpace(urls, model: model)
                }
                return true
            }
    }
}

extension View {
    /// Takes photos, audio files and videos dropped here into the open space.
    func mediaDropTarget() -> some View { modifier(MediaDropTarget()) }
}

/// The photos part of the menu bar panel: the photo being read and the ones that could not be
/// read, with Retry. Nothing while there is nothing to say.
struct PhotosMenuSection: View {
    @Environment(PhotosProcessor.self) private var processor
    @Environment(\.kTheme) private var theme

    var body: some View {
        if processor.current != nil || !processor.failures.isEmpty {
            VStack(alignment: .leading, spacing: KSpace.x2) {
                KSectionLabel("Photos")
                if let current = processor.current {
                    HStack(spacing: KSpace.x2) {
                        KSpinner()
                        Text("Reading \(current.title)").font(KFont.ui(12)).foregroundStyle(theme.muted).lineLimit(1)
                    }
                    .padding(.horizontal, KSpace.x1)
                }
                ForEach(processor.failures) { failure in
                    PhotoFailureRow(failure: failure)
                }
            }
        }
    }
}

private struct PhotoFailureRow: View {
    let failure: PhotosProcessor.Failure
    @Environment(PhotosProcessor.self) private var processor
    @State private var retrying = false
    @State private var error: String?
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x1) {
            HStack(spacing: KSpace.x2) {
                KIcon(.warning, size: .xs, color: theme.danger)
                VStack(alignment: .leading, spacing: 2) {
                    Text(failure.title).font(KFont.ui(12, .semibold)).lineLimit(1)
                    Text(failure.message).font(KFont.ui(11)).foregroundStyle(theme.muted).lineLimit(2)
                }
                Spacer(minLength: KSpace.x2)
                KButton("Retry", size: .small) {
                    retrying = true
                    Task {
                        do {
                            try await processor.retry(failure)
                            error = nil
                        } catch {
                            self.error = error.localizedDescription
                        }
                        retrying = false
                    }
                }
                .disabled(retrying)
            }
            if let error { KErrorText(error) }
        }
        .padding(.horizontal, KSpace.x1)
    }
}
