import Foundation
import Observation

/// Where a finished recording, its transcript and its minutes go. It will map to Knode Service's
/// `recordings.create`, `recordings.transcript`, `recordings.summary` and `recordings.fail`.
protocol RecordingSink: Sendable {
    /// Saves the recording and its audio files, and returns its id. The files stay where they are
    /// until the sink has copied them.
    func save(_ recording: FinishedRecording) async throws -> String
    /// Saves the whole transcript of a recording.
    func saveTranscript(_ transcript: Transcript, for recording: String) async throws
    /// Saves the minutes of a recording, next to its transcript.
    func saveSummary(_ summary: MeetingSummary, for recording: String) async throws
    /// Marks a recording as failed, so it can be retried; its audio is kept.
    func fail(_ recording: String, reason: String) async
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
    /// Whether minutes are written after the transcript.
    var summarizes: Bool

    @ObservationIgnored private let recorder: any MeetingRecording
    @ObservationIgnored private let transcriber: any Transcriber
    @ObservationIgnored private let summarizer: any Summarizer
    @ObservationIgnored private let sink: any RecordingSink
    @ObservationIgnored private let consent: any RecordingConsentStore
    @ObservationIgnored private let now: () -> Date
    @ObservationIgnored private var startedAt = Date()
    @ObservationIgnored private var processing: Task<Void, Never>?
    @ObservationIgnored private var ticker: Task<Void, Never>?

    init(
        recorder: any MeetingRecording,
        transcriber: any Transcriber,
        summarizer: any Summarizer,
        sink: any RecordingSink,
        consent: any RecordingConsentStore,
        summarizes: Bool = true,
        now: @escaping () -> Date = Date.init
    ) {
        self.recorder = recorder
        self.transcriber = transcriber
        self.summarizer = summarizer
        self.sink = sink
        self.consent = consent
        self.summarizes = summarizes
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
        stopTicking()
        phase = .saving
        isPaused = false
        processing = Task { await self.finish(mode: mode, target: target, duration: duration) }
    }

    /// Waits for the background work started by `stop()`.
    func waitUntilProcessed() async {
        await processing?.value
    }

    /// Stops saving, transcribing or summarising. What was saved stays saved.
    func cancelProcessing() {
        processing?.cancel()
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

    private func finish(mode: RecordingMode, target: RecordingTarget, duration: Duration) async {
        let tracks: [RecordedTrack]
        let id: String
        do {
            tracks = try await recorder.stop()
            id = try await sink.save(FinishedRecording(mode: mode, target: target, startedAt: startedAt, duration: duration, tracks: tracks))
        } catch {
            phase = .failed(error.localizedDescription)
            return
        }

        let transcript: Transcript
        do {
            phase = .transcribing(progress: 0)
            transcript = try await transcriber.transcribe(tracks, locale: nil) { value in
                Task { @MainActor in self.report(transcribing: value) }
            }
            try Task.checkCancellation()
            try await sink.saveTranscript(transcript, for: id)
        } catch {
            await fail(id, error)
            return
        }

        guard summarizes else {
            phase = .done(note: nil)
            return
        }
        if case .unavailable(let reason) = await summarizer.availability(for: transcript) {
            phase = .done(note: reason.message)
            return
        }
        do {
            phase = .summarising(progress: 0)
            let summary = try await summarizer.summarize(transcript) { value in
                Task { @MainActor in self.report(summarising: value) }
            }
            try Task.checkCancellation()
            try await sink.saveSummary(summary, for: id)
            phase = .done(note: nil)
        } catch is CancellationError {
            phase = .done(note: "Minutes were cancelled. The transcript is kept.")
        } catch {
            phase = .done(note: "Minutes could not be written: \(error.localizedDescription) The transcript is kept.")
        }
    }

    private func fail(_ id: String, _ error: any Error) async {
        let reason = error is CancellationError ? "Cancelled" : error.localizedDescription
        await sink.fail(id, reason: reason)
        phase = .failed(reason)
    }

    private func report(transcribing value: Double) {
        guard case .transcribing = phase else { return }
        phase = .transcribing(progress: value)
    }

    private func report(summarising value: Double) {
        guard case .summarising = phase else { return }
        phase = .summarising(progress: value)
    }
}

/// Until Knode Service has `recordings.*`, a sink that keeps the files where they are and
/// remembers nothing: the shell runs end to end without saving to a space.
struct UnwiredRecordingSink: RecordingSink {
    func save(_ recording: FinishedRecording) async throws -> String { UUID().uuidString }
    func saveTranscript(_ transcript: Transcript, for recording: String) async throws {}
    func saveSummary(_ summary: MeetingSummary, for recording: String) async throws {}
    func fail(_ recording: String, reason: String) async {}
}
