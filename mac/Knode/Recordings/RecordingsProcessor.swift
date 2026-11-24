import Foundation
import Observation

/// Transcribes and summarises, on this Mac, the recordings the spaces say are still to do: the
/// meetings recorded here, and recordings the app did not make, such as voice memos a connector
/// brought in. What is left to do is kept in the space (`recordings.pending`), so the work goes on
/// after the app restarts.
///
/// It looks for work at launch (`start()`), when Knode Service starts again (`serviceDidStart()`),
/// when the open spaces change and when a space announces a change to a node. One recording at a
/// time, never two at once on the same one; the current one can be cancelled.
@MainActor
@Observable
final class RecordingsProcessor {
    /// The recording being worked on.
    struct Job: Equatable, Sendable {
        enum Step: Equatable, Sendable { case transcribing, summarising }
        var space: Space.ID
        var recording: Node.ID
        var title: String
        var step: Step
        var progress: Double
    }

    /// How a recording's work ended.
    enum Outcome: Equatable, Sendable {
        /// Transcribed, and summarised unless `note` says why not.
        case done(note: String?)
        /// The transcript failed; the space says so and it can be retried.
        case failed(String)
        /// The space could not be read or written now; the work is left pending for later.
        case deferred(String)
    }

    /// A recording whose transcript or minutes failed, which the person can retry.
    struct Failure: Identifiable, Equatable, Sendable {
        var space: Space.ID
        var spaceURL: URL
        var recording: Node.ID
        var title: String
        var stage: StoredRecording.Stage
        var message: String
        var id: Node.ID { recording }
    }

    private(set) var current: Job?
    private(set) var failures: [Failure] = []

    @ObservationIgnored private let store: any RecordingsBackend
    @ObservationIgnored private let transcriber: any Transcriber
    @ObservationIgnored private let summarizer: any Summarizer
    @ObservationIgnored private let spaceURLs: @MainActor () -> [URL]
    @ObservationIgnored private let scratch: URL
    /// The spaces opened so far, to find a change's space by id.
    @ObservationIgnored private var known: [Space.ID: URL] = [:]
    /// Spaces to look for work in, in order.
    @ObservationIgnored private var queued: [URL] = []
    /// Recordings handed over by the recorder, worked on before anything else.
    @ObservationIgnored private var handedOver: [(recording: Node.ID, space: Space.ID)] = []
    @ObservationIgnored private var waiting: [Node.ID: [CheckedContinuation<Outcome, Never>]] = [:]
    @ObservationIgnored private var observers: [Node.ID: @MainActor (Job) -> Void] = [:]
    @ObservationIgnored private var worker: Task<Void, Never>?
    @ObservationIgnored private var job: Task<Outcome, Never>?
    @ObservationIgnored private var changesTask: Task<Void, Never>?
    @ObservationIgnored private var rescanTask: Task<Void, Never>?
    /// How often every space is looked at again, so minutes Apple's model could not write yet
    /// (still downloading, Apple Intelligence off) are written once it can.
    @ObservationIgnored private let rescanInterval: Duration
    @ObservationIgnored private var stopped = false

    init(
        store: any RecordingsBackend,
        transcriber: any Transcriber,
        summarizer: any Summarizer,
        spaceURLs: @escaping @MainActor () -> [URL],
        scratch: URL = FileManager.default.temporaryDirectory.appending(path: "Knode Transcribing", directoryHint: .isDirectory),
        rescanInterval: Duration = .seconds(600)
    ) {
        self.rescanInterval = rescanInterval
        self.store = store
        self.transcriber = transcriber
        self.summarizer = summarizer
        self.spaceURLs = spaceURLs
        self.scratch = scratch
    }

    /// Starts listening to the spaces and looks for work in every one of them.
    func start() {
        stopped = false
        if changesTask == nil {
            let changes = store.changes()
            changesTask = Task { [weak self] in
                for await change in changes {
                    self?.heard(change)
                }
            }
        }
        observeSpaces()
        scanAll()
        if rescanTask == nil {
            let interval = rescanInterval
            rescanTask = Task { [weak self] in
                while !Task.isCancelled {
                    try? await Task.sleep(for: interval)
                    guard !Task.isCancelled else { return }
                    self?.scanAll()
                }
            }
        }
    }

    /// Knode Service started again: what it holds may have work the app has not seen.
    func serviceDidStart() {
        scanAll()
    }

    /// Looks for work in every space the app knows.
    func scanAll() {
        for url in spaceURLs() { schedule(url) }
    }

    /// Looks for work in one space, such as the one a recording was just saved to.
    func scan(spaceAt url: URL) {
        schedule(url)
    }

    /// Stops listening and stops the current work without marking it failed, as when the app
    /// quits: it stays pending in its space.
    func stop() async {
        stopped = true
        changesTask?.cancel()
        changesTask = nil
        rescanTask?.cancel()
        rescanTask = nil
        queued = []
        job?.cancel()
        await worker?.value
    }

    /// Works on a recording the recorder just stored, before anything else, and returns how it
    /// ended. `progress` hears each step while it runs.
    func process(_ recording: Node.ID, in space: Space.ID, spaceURL: URL, progress: @escaping @MainActor (Job) -> Void) async -> Outcome {
        known[space] = spaceURL
        observers[recording] = progress
        handedOver.append((recording, space))
        return await withCheckedContinuation { continuation in
            waiting[recording, default: []].append(continuation)
            wake()
        }
    }

    /// Cancels the work on a recording: the stage running now is marked failed, with "Cancelled",
    /// so the person can retry it.
    func cancel(_ recording: Node.ID) {
        if current?.recording == recording {
            job?.cancel()
        } else if let index = handedOver.firstIndex(where: { $0.recording == recording }) {
            let space = handedOver.remove(at: index).space
            Task {
                _ = try? await store.failRecording(recording, stage: .transcript, failure: .failed, error: Self.cancelled, by: .speech, in: space)
                finish(recording, .failed(Self.cancelled))
            }
        }
    }

    /// Puts a failed stage back to pending, as the person, and works on it again.
    func retry(_ failure: Failure) async throws {
        _ = try await store.retryRecording(failure.recording, stage: failure.stage, in: failure.space)
        failures.removeAll { $0.id == failure.id }
        schedule(failure.spaceURL)
    }

    /// Transcribes a recording again from the start, as the person: the language spoken is listened
    /// for again and minutes already written are made again from the new transcript.
    func transcribeAgain(_ recording: Node.ID, in space: Space.ID) async throws {
        _ = try await store.retryRecording(recording, stage: .transcript, in: space)
        failures.removeAll { $0.id == recording }
        if let url = known[space] { schedule(url) } else { scanAll() }
    }

    static let cancelled = "Cancelled"

    // MARK: Finding work

    private func observeSpaces() {
        guard !stopped else { return }
        let urls = withObservationTracking { spaceURLs() } onChange: { [weak self] in
            Task { @MainActor in
                guard let self else { return }
                self.observeSpaces()
                self.scanAll()
            }
        }
        _ = urls
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
        wake()
    }

    private func wake() {
        guard worker == nil, !stopped else { return }
        worker = Task { await self.work() }
    }

    private func work() async {
        defer { worker = nil }
        while !stopped {
            if !handedOver.isEmpty {
                let next = handedOver.removeFirst()
                await run(next.recording, in: next.space)
                continue
            }
            guard !queued.isEmpty else { return }
            let url = queued.removeFirst()
            guard let space = try? await store.openSpace(at: url) else { continue }
            known[space.id] = url
            await refreshFailures(in: space.id, at: url)
            guard let pending = try? await store.pendingRecordings(in: space.id) else { continue }
            for recording in pending where !stopped {
                // A recording handed over meanwhile goes first.
                while !handedOver.isEmpty, !stopped {
                    let next = handedOver.removeFirst()
                    await run(next.recording, in: next.space)
                }
                await run(recording.id, in: space.id)
            }
        }
    }

    /// The failures of a space as the space keeps them (`recordings.failed`), so a failure from
    /// before the app restarted can be retried too. When they cannot be read, the ones known
    /// already stay.
    private func refreshFailures(in space: Space.ID, at url: URL) async {
        guard let failed = try? await store.failedRecordings(in: space) else { return }
        let known = failed.compactMap { recording in Failure(recording, in: space, at: url) }
        failures = failures.filter { $0.space != space } + known
        await retryMinutesNowAvailable(failed, in: space)
    }

    /// Minutes the model could not write before, put back to pending once it can write in their
    /// language, so they are written without anyone asking.
    private func retryMinutesNowAvailable(_ recordings: [StoredRecording], in space: Space.ID) async {
        for recording in recordings where recording.transcript == .done && recording.summary == .unavailable && !stopped {
            let probe = Transcript(locale: recording.language ?? Locale.current.identifier(.bcp47), segments: [])
            guard await summarizer.availability(for: probe) == .available else { continue }
            _ = try? await store.retryRecording(recording.id, stage: .summary, in: space)
        }
    }

    private func run(_ recording: Node.ID, in space: Space.ID) async {
        let task = Task { await self.handle(recording, in: space) }
        job = task
        let outcome = await task.value
        job = nil
        current = nil
        finish(recording, outcome)
    }

    private func finish(_ recording: Node.ID, _ outcome: Outcome) {
        observers[recording] = nil
        for continuation in waiting.removeValue(forKey: recording) ?? [] {
            continuation.resume(returning: outcome)
        }
    }

    // MARK: One recording

    /// An error of the store, not of the work: the work is left pending.
    private struct StoreFailure: Error {
        var error: any Error
    }

    private func handle(_ id: Node.ID, in space: Space.ID) async -> Outcome {
        // Read again: the list it came from may be old, and the recording done already.
        var recording: StoredRecording
        do {
            recording = try await store.recording(id, in: space)
        } catch {
            return .deferred(error.localizedDescription)
        }
        guard recording.isPending else { return Self.outcome(of: recording) }
        failures.removeAll { $0.id == id }

        var transcript: Transcript?
        if recording.transcript == .pending {
            report(Job(space: space, recording: id, title: recording.title, step: .transcribing, progress: 0))
            do {
                let made = try await transcribe(recording, in: space)
                try Task.checkCancellation()
                transcript = made
            } catch let failure as StoreFailure {
                return .deferred(failure.error.localizedDescription)
            } catch {
                if stopped { return .deferred(Self.cancelled) }
                let message = error is CancellationError ? Self.cancelled : error.localizedDescription
                await fail(recording, in: space, stage: .transcript, failure: .failed, message: message, engine: .speech)
                return .failed(message)
            }
            do {
                recording = try await store.setTranscript(transcript!, by: .speech, of: id, in: space)
            } catch {
                return .deferred(error.localizedDescription)
            }
        }

        guard recording.transcript == .done, recording.summary == .pending else { return Self.outcome(of: recording) }
        if transcript == nil {
            guard let transcriptID = recording.transcriptID else {
                let message = "The transcript of this recording is missing."
                await fail(recording, in: space, stage: .summary, failure: .failed, message: message, engine: .minutes)
                return .done(note: message)
            }
            do {
                let node = try await store.node(transcriptID, in: space)
                transcript = TranscriptNode.transcript(fromBody: node.body, language: recording.language)
            } catch {
                return .deferred(error.localizedDescription)
            }
        }
        return await summarise(recording, transcript: transcript!, in: space)
    }

    private func summarise(_ recording: StoredRecording, transcript: Transcript, in space: Space.ID) async -> Outcome {
        if case .unavailable(let reason) = await summarizer.availability(for: transcript) {
            await fail(recording, in: space, stage: .summary, failure: .unavailable, message: reason.message, engine: .minutes)
            return .done(note: reason.message)
        }
        report(Job(space: space, recording: recording.id, title: recording.title, step: .summarising, progress: 0))
        let summary: MeetingSummary
        do {
            let id = recording.id
            summary = try await summarizer.summarize(transcript) { value in
                Task { @MainActor in self.report(progress: value, of: id, step: .summarising) }
            }
            try Task.checkCancellation()
        } catch {
            if stopped { return .deferred(Self.cancelled) }
            if error is CancellationError {
                await fail(recording, in: space, stage: .summary, failure: .failed, message: Self.cancelled, engine: .minutes)
                return .done(note: "Minutes were cancelled. The transcript is kept.")
            }
            let message = error.localizedDescription
            await fail(recording, in: space, stage: .summary, failure: .failed, message: message, engine: .minutes)
            return .done(note: "Minutes could not be written: \(message) The transcript is kept.")
        }
        do {
            _ = try await store.setSummary(summary, by: .minutes, of: recording.id, in: space)
            return .done(note: nil)
        } catch {
            return .deferred(error.localizedDescription)
        }
    }

    /// Transcribes the recording's audio files, each read under its own name (the space keeps
    /// them by hash, without an extension the audio reader could go by).
    private func transcribe(_ recording: StoredRecording, in space: Space.ID) async throws -> Transcript {
        guard !recording.tracks.isEmpty else { throw RecordingsProcessorError.noAudio }
        let folder = scratch.appending(path: UUID().uuidString, directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: folder) }
        var tracks: [RecordedTrack] = []
        for (index, track) in recording.tracks.enumerated() {
            let blob: URL?
            do {
                blob = try await store.attachmentURL(named: track.name, of: recording.id, in: space)
            } catch {
                throw StoreFailure(error: error)
            }
            guard let blob else { throw RecordingsProcessorError.missingAudio(track.name) }
            let name = (track.name as NSString).lastPathComponent
            let link = folder.appending(path: "\(index)-\(name.isEmpty ? "audio" : name)")
            try FileManager.default.createSymbolicLink(at: link, withDestinationURL: blob)
            tracks.append(RecordedTrack(role: track.role, url: link))
        }
        let id = recording.id
        return try await transcriber.transcribe(tracks, locale: recording.language.map(Locale.init(identifier:))) { value in
            Task { @MainActor in self.report(progress: value, of: id, step: .transcribing) }
        }
    }

    private func fail(_ recording: StoredRecording, in space: Space.ID, stage: StoredRecording.Stage, failure: StoredRecording.Failure, message: String, engine: RecordingEngine) async {
        // In a task of its own: the work may have been cancelled, the write must not be.
        let store = store
        let id = recording.id
        await Task { _ = try? await store.failRecording(id, stage: stage, failure: failure, error: message, by: engine, in: space) }.value
        guard failure == .failed, let url = known[space] else { return }
        failures.removeAll { $0.id == recording.id }
        failures.append(Failure(space: space, spaceURL: url, recording: recording.id, title: recording.title, stage: stage, message: message))
    }

    private func report(_ job: Job) {
        current = job
        observers[job.recording]?(job)
    }

    private func report(progress: Double, of recording: Node.ID, step: Job.Step) {
        guard var job = current, job.recording == recording, job.step == step else { return }
        job.progress = min(max(progress, job.progress), 1)
        report(job)
    }

    private static func outcome(of recording: StoredRecording) -> Outcome {
        switch recording.transcript {
        case .failed: return .failed(recording.error ?? "The transcript failed.")
        case .pending: return .deferred("The transcript is still to be made.")
        case .done:
            switch recording.summary {
            case .unavailable, .failed: return .done(note: recording.error)
            default: return .done(note: nil)
            }
        }
    }
}

extension RecordingsProcessor.Failure {
    /// A failure to retry from a recording the space says failed: its transcript, else its
    /// minutes. Minutes unavailable on this Mac are not a failure to retry by hand.
    init?(_ recording: StoredRecording, in space: Space.ID, at url: URL) {
        let stage: StoredRecording.Stage
        if recording.transcript == .failed {
            stage = .transcript
        } else if recording.summary == .failed {
            stage = .summary
        } else {
            return nil
        }
        self.init(
            space: space, spaceURL: url, recording: recording.id, title: recording.title, stage: stage,
            message: recording.error ?? (stage == .transcript ? "The transcript failed." : "The minutes failed.")
        )
    }
}

enum RecordingsProcessorError: Error, LocalizedError, Equatable {
    case noAudio
    case missingAudio(String)

    var errorDescription: String? {
        switch self {
        case .noAudio: "This recording has no audio to transcribe."
        case .missingAudio(let name): "The audio file \(name) is missing from the space."
        }
    }
}
