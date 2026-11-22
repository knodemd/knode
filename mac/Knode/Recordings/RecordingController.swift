import Foundation
import Observation

/// Where a finished recording goes: its space, through `recordings.create`.
protocol RecordingSink: Sendable {
    /// Stores the recording and its audio files, then removes the files it was given.
    func save(_ recording: FinishedRecording) async throws -> SavedRecording
}

/// A recording stored in its space.
struct SavedRecording: Hashable, Sendable {
    var space: Space.ID
    var spaceURL: URL
    var recording: StoredRecording
}

/// Stores finished recordings through the app's store: Knode Service when it runs, the core in
/// the app when it does not. The recording is the person's; whether minutes are written comes
/// from the space (`recordings.summaries`).
struct StoreRecordingSink: RecordingSink {
    let store: any RecordingsBackend
    /// The most bytes one audio file may have: what the space keeps in one attachment.
    var limit: Int64 = AttachmentInfo.maxSize
    var locale: Locale = .current

    func save(_ recording: FinishedRecording) async throws -> SavedRecording {
        try RecordingAudio.check(recording.tracks, limit: limit)
        let space = try await store.openSpace(at: recording.target.spaceURL)
        let summaries = try await store.recordingSummariesEnabled(in: space.id)
        let app: String? = if case .online(.apps(let ids)) = recording.mode { ids.first } else { nil }
        let draft = NewRecordingDraft(
            kind: .meeting,
            title: RecordingTitle.of(recording.mode, startedAt: recording.startedAt, locale: locale),
            startedAt: recording.startedAt,
            endedAt: recording.endedAt,
            project: recording.target.projectID,
            audio: recording.tracks,
            key: recording.key,
            app: app,
            summaries: summaries
        )
        let stored = try await store.createRecording(draft, in: space.id)
        RecordingAudio.remove(recording.tracks)
        return SavedRecording(space: space.id, spaceURL: space.url ?? recording.target.spaceURL, recording: stored)
    }
}

/// The audio files of a recording before they are stored.
enum RecordingAudio {
    /// Fails, before anything is stored, when a file is larger than a space keeps.
    static func check(_ tracks: [RecordedTrack], limit: Int64) throws {
        for track in tracks {
            let size = (try? track.url.resourceValues(forKeys: [.fileSizeKey]).fileSize).flatMap { Int64($0) } ?? 0
            if size > limit {
                throw RecordingError.tooLarge(
                    file: track.url.lastPathComponent, size: size, limit: limit,
                    folder: track.url.deletingLastPathComponent().path(percentEncoded: false)
                )
            }
        }
    }

    /// Removes the private folder the files were recorded into.
    static func remove(_ tracks: [RecordedTrack]) {
        for folder in Set(tracks.map { $0.url.deletingLastPathComponent() }) {
            try? FileManager.default.removeItem(at: folder)
        }
    }
}

/// Whether the person has read the first-run explanation about consent.
@MainActor
protocol RecordingConsentStore: AnyObject {
    var hasAcknowledged: Bool { get set }
}

/// The acknowledgement kept in the app's preferences.
@MainActor
final class DefaultsRecordingConsentStore: RecordingConsentStore {
    static let key = "KnodeRecordingConsentAcknowledged"
    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    var hasAcknowledged: Bool {
        get { defaults.bool(forKey: Self.key) }
        set { defaults.set(newValue, forKey: Self.key) }
    }
}

/// The acknowledgement kept in memory, for tests and previews.
@MainActor
final class MemoryRecordingConsentStore: RecordingConsentStore {
    var hasAcknowledged: Bool

    init(hasAcknowledged: Bool = false) {
        self.hasAcknowledged = hasAcknowledged
    }
}

/// One meeting recording from start to minutes: nothing is recorded until the person has read
/// the consent explanation once and starts this meeting by hand.
///
/// idle → consentNeeded (first time only) → choosingTarget → recording → saving → transcribing →
/// summarising → done, or failed from any step after it starts.
@MainActor
@Observable
final class RecordingController {
    enum Phase: Equatable {
        case idle
        case consentNeeded(RecordingMode)
        case choosingTarget(RecordingMode)
        case recording(RecordingMode, RecordingTarget)
        case saving
        case transcribing(progress: Double)
        case summarising(progress: Double)
        /// Finished; `note` says why there are no minutes when there are none.
        case done(note: String?)
        case failed(String)
    }

    static let notice = "This meeting is being recorded and transcribed on my device by Knode. The audio and the transcript stay on my computer."

    private(set) var phase: Phase = .idle
    private(set) var isPaused = false
    /// Whole seconds recorded, pauses left out, updated every second for the indicator.
    private(set) var elapsedSeconds = 0

    @ObservationIgnored private let recorder: any MeetingRecording
    @ObservationIgnored private let sink: any RecordingSink
    @ObservationIgnored private let processor: RecordingsProcessor
    @ObservationIgnored private let consent: any RecordingConsentStore
    @ObservationIgnored private let now: () -> Date
    @ObservationIgnored private var startedAt = Date()
    @ObservationIgnored private var key = UUID()
    /// The recording being transcribed or summarised, once stored.
    @ObservationIgnored private var stored: Node.ID?
    @ObservationIgnored private var processing: Task<Void, Never>?
    @ObservationIgnored private var ticker: Task<Void, Never>?

    init(
        recorder: any MeetingRecording,
        sink: any RecordingSink,
        processor: RecordingsProcessor,
        consent: any RecordingConsentStore,
        now: @escaping () -> Date = Date.init
    ) {
        self.recorder = recorder
        self.sink = sink
        self.processor = processor
        self.consent = consent
        self.now = now
    }

    var isRecording: Bool {
        if case .recording = phase { true } else { false }
    }

    /// Whether a recording is being saved, transcribed or summarised.
    var isProcessing: Bool {
        switch phase {
        case .saving, .transcribing, .summarising: true
        default: false
        }
    }

    /// Whether a new recording can be asked for now.
    var canRequest: Bool {
        switch phase {
        case .idle, .done, .failed: true
        default: false
        }
    }

    var elapsed: Duration { isRecording ? recorder.elapsed : .zero }
    var level: Float { isRecording ? recorder.level : 0 }

    /// Asks to record a meeting: the consent explanation the first time, then where it goes.
    func request(_ mode: RecordingMode) {
        guard canRequest else { return }
        phase = consent.hasAcknowledged ? .choosingTarget(mode) : .consentNeeded(mode)
    }

    func acknowledgeConsent() {
        guard case .consentNeeded(let mode) = phase else { return }
        consent.hasAcknowledged = true
        phase = .choosingTarget(mode)
    }

    /// Goes back without recording.
    func cancelRequest() {
        switch phase {
        case .consentNeeded, .choosingTarget: phase = .idle
        default: break
        }
    }

    /// Starts this meeting's recording, by hand.
    func start(target: RecordingTarget) async {
        guard case .choosingTarget(let mode) = phase else { return }
        do {
            try await recorder.start(mode)
            startedAt = now()
            key = UUID()
            isPaused = false
            elapsedSeconds = 0
            phase = .recording(mode, target)
            startTicking()
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    func pause() {
        guard isRecording, !isPaused else { return }
        recorder.pause()
        isPaused = true
    }

    func resume() {
        guard isRecording, isPaused else { return }
        recorder.resume()
        isPaused = false
    }

    /// Stops recording and throws the audio away.
    func discard() {
        guard isRecording else { return }
        recorder.cancel()
        stopTicking()
        isPaused = false
        phase = .idle
    }

    /// Stops recording, then saves, transcribes and summarises in the background.
    func stop() {
        guard case .recording(let mode, let target) = phase else { return }
        let duration = recorder.elapsed
        let endedAt = now()
        stopTicking()
        phase = .saving
        isPaused = false
        processing = Task { await self.finish(mode: mode, target: target, duration: duration, endedAt: endedAt) }
    }

    /// Waits for the background work started by `stop()`.
    func waitUntilProcessed() async {
        await processing?.value
    }

    /// Stops saving, transcribing or summarising. What was saved stays saved, and the stage
    /// cancelled can be retried.
    func cancelProcessing() {
        if let stored {
            processor.cancel(stored)
        } else {
            processing?.cancel()
        }
    }

    /// Clears a finished or failed recording.
    func dismiss() {
        switch phase {
        case .done, .failed: phase = .idle
        default: break
        }
    }

    private func startTicking() {
        ticker?.cancel()
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(250))
                guard let self, self.isRecording else { return }
                let seconds = Int(self.recorder.elapsed.components.seconds)
                if seconds != self.elapsedSeconds { self.elapsedSeconds = seconds }
            }
        }
    }

    private func stopTicking() {
        ticker?.cancel()
        ticker = nil
    }

    /// Stores the recording, then hands it to the processor, the one pipeline that transcribes
    /// and summarises every recording, and follows its progress.
    private func finish(mode: RecordingMode, target: RecordingTarget, duration: Duration, endedAt: Date) async {
        let saved: SavedRecording
        do {
            let tracks = try await recorder.stop()
            saved = try await sink.save(FinishedRecording(
                mode: mode, target: target, startedAt: startedAt, endedAt: endedAt, duration: duration,
                tracks: tracks, key: "recorder:\(key.uuidString)"
            ))
        } catch {
            phase = .failed(error is CancellationError ? RecordingsProcessor.cancelled : error.localizedDescription)
            return
        }
        stored = saved.recording.id
        defer { stored = nil }
        phase = .transcribing(progress: 0)
        let outcome = await processor.process(saved.recording.id, in: saved.space, spaceURL: saved.spaceURL) { [weak self] job in
            guard let self else { return }
            switch job.step {
            case .transcribing: self.phase = .transcribing(progress: job.progress)
            case .summarising: self.phase = .summarising(progress: job.progress)
            }
        }
        switch outcome {
        case .done(let note): phase = .done(note: note)
        case .failed(let message): phase = .failed(message)
        case .deferred: phase = .done(note: "The recording is saved. Knode will transcribe it when the space can be written again.")
        }
    }
}
