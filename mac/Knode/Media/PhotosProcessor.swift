import Foundation
import Observation

/// Reads, on this Mac, the photos the spaces say are still to read (`photos.pending`): the text
/// in each one and what it shows, written into its node. What is left to read is kept in the
/// space, so the work goes on after the app restarts.
///
/// It looks for work at launch (`start()`), when Knode Service starts again, when the open spaces
/// change and when a space announces a change. One photo at a time.
@MainActor
@Observable
final class PhotosProcessor {
    /// A photo whose reading failed, which the person can retry.
    struct Failure: Identifiable, Equatable, Sendable {
        var space: Space.ID
        var spaceURL: URL
        var photo: Node.ID
        var title: String
        var message: String
        var id: Node.ID { photo }
    }

    /// The photo being read.
    private(set) var current: (space: Space.ID, photo: Node.ID, title: String)?
    private(set) var failures: [Failure] = []
    /// How many photos were read since launch.
    private(set) var readCount = 0

    @ObservationIgnored private let store: any PhotosBackend
    @ObservationIgnored private let reader: any PhotoReader
    @ObservationIgnored private let spaceURLs: @MainActor () -> [URL]
    @ObservationIgnored private let scratch: URL
    @ObservationIgnored private var known: [Space.ID: URL] = [:]
    @ObservationIgnored private var queued: [URL] = []
    @ObservationIgnored private var worker: Task<Void, Never>?
    @ObservationIgnored private var changesTask: Task<Void, Never>?
    @ObservationIgnored private var stopped = false

    init(
        store: any PhotosBackend,
        reader: any PhotoReader,
        spaceURLs: @escaping @MainActor () -> [URL],
        scratch: URL = FileManager.default.temporaryDirectory.appending(path: "Knode Reading Photos", directoryHint: .isDirectory)
    ) {
        self.store = store
        self.reader = reader
        self.spaceURLs = spaceURLs
        self.scratch = scratch
    }

    func start() {
        stopped = false
        if changesTask == nil {
            let changes = store.changes()
            changesTask = Task { [weak self] in
                for await change in changes { self?.heard(change) }
            }
        }
        observeSpaces()
        scanAll()
    }

    func serviceDidStart() { scanAll() }

    func scanAll() {
        for url in spaceURLs() { schedule(url) }
    }

    func scan(spaceAt url: URL) { schedule(url) }

    /// Stops listening and lets the photo being read finish; what is left stays pending.
    func stop() async {
        stopped = true
        changesTask?.cancel()
        changesTask = nil
        queued = []
        await worker?.value
    }

    /// Puts a failed reading back to pending, as the person, and reads it again.
    func retry(_ failure: Failure) async throws {
        _ = try await store.retryPhoto(failure.photo, in: failure.space)
        failures.removeAll { $0.id == failure.id }
        schedule(failure.spaceURL)
    }

    /// Waits until nothing is queued or being read; for tests.
    func idle() async {
        while let worker { await worker.value }
    }

    private func observeSpaces() {
        guard !stopped else { return }
        _ = withObservationTracking { spaceURLs() } onChange: { [weak self] in
            Task { @MainActor in
                guard let self else { return }
                self.observeSpaces()
                self.scanAll()
            }
        }
    }

    private func heard(_ change: SpaceChange) {
        switch change.kind {
        case .create, .update, .reload:
            if let url = known[change.space] { schedule(url) } else if change.kind == .reload { scanAll() }
        default:
            break
        }
    }

    private func schedule(_ url: URL) {
        guard !stopped else { return }
        let url = url.standardizedFileURL
        if !queued.contains(url) { queued.append(url) }
        guard worker == nil else { return }
        worker = Task { await self.work() }
    }

    private func work() async {
        defer { worker = nil }
        while !stopped, !queued.isEmpty {
            let url = queued.removeFirst()
            guard let space = try? await store.openSpace(at: url) else { continue }
            known[space.id] = url
            if let failed = try? await store.failedPhotos(in: space.id) {
                failures = failures.filter { $0.space != space.id }
                    + failed.map { Failure(space: space.id, spaceURL: url, photo: $0.id, title: $0.title, message: $0.error ?? "The photo could not be read.") }
            }
            guard let pending = try? await store.pendingPhotos(in: space.id) else { continue }
            for photo in pending where !stopped {
                await read(photo, in: space.id, at: url)
            }
        }
    }

    private func read(_ photo: StoredPhoto, in space: Space.ID, at url: URL) async {
        current = (space, photo.id, photo.title)
        defer { current = nil }
        let blob: URL?
        do {
            blob = try await store.attachmentURL(named: photo.file, of: photo.id, in: space)
        } catch {
            return // The space could not be read now: left pending.
        }
        let reading: PhotoReadingDraft
        do {
            guard let blob else { throw PhotosProcessorError.missingFile(photo.file) }
            reading = try await readFile(blob, named: photo.file)
        } catch {
            let message = error.localizedDescription
            _ = try? await store.failPhoto(photo.id, error: message, in: space)
            failures.removeAll { $0.id == photo.id }
            failures.append(Failure(space: space, spaceURL: url, photo: photo.id, title: photo.title, message: message))
            return
        }
        if (try? await store.setPhotoReading(reading, of: photo.id, in: space)) != nil {
            readCount += 1
        }
    }

    /// Reads the photo under its own name: the space keeps it by hash, without an extension.
    private func readFile(_ blob: URL, named name: String) async throws -> PhotoReadingDraft {
        let folder = scratch.appending(path: UUID().uuidString, directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = (name as NSString).lastPathComponent
        let link = folder.appending(path: file.isEmpty ? "photo" : file)
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: blob)
        return try await reader.read(link)
    }
}

enum PhotosProcessorError: Error, LocalizedError, Equatable {
    case missingFile(String)

    var errorDescription: String? {
        switch self {
        case .missingFile(let name): "The photo \(name) is missing from the space."
        }
    }
}
