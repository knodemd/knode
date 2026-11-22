import Foundation
import Observation

/// Where a finished recording goes: its space, through `recordings.create`.
protocol RecordingSink: Sendable {
    /// Stores the recording and its audio files, then removes the files it was given, only once
    /// the stored recording lists every one of them.
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
    /// Links a recording to the calendar event it was made at, when the person chose one.
    var calendar: (any CalendarLinking)? = nil

    func save(_ recording: FinishedRecording) async throws -> SavedRecording {
        try RecordingAudio.check(recording.tracks, limit: limit)
        let space = try await store.openSpace(at: recording.target.spaceURL)
        let summaries = try await store.recordingSummariesEnabled(in: space.id)
        let app: String? = if case .online(.apps(let ids)) = recording.mode { ids.first } else { nil }
        let draft = NewRecordingDraft(
            kind: .meeting,
            title: recording.target.eventTitle ?? RecordingTitle.of(recording.mode, startedAt: recording.startedAt, locale: locale),
            startedAt: recording.startedAt,
            endedAt: recording.endedAt,
            project: recording.target.projectID,
            audio: recording.tracks,
            key: recording.key,
            app: app,
            summaries: summaries
        )
        let stored = try await store.createRecording(draft, in: space.id)
        // The audio is removed only once the space holds every file of it.
        let missing = RecordingAudio.missing(recording.tracks, from: stored)
        guard missing.isEmpty else {
            throw RecordingError.notStored(
                files: missing.map(\.url.lastPathComponent),
                folder: recording.tracks[0].url.deletingLastPathComponent().path(percentEncoded: false)
            )
        }
        RecordingAudio.remove(recording.tracks)
        let spaceURL = space.url ?? recording.target.spaceURL
        if let event = recording.target.eventID, let calendar {
            // The recording is kept either way; a link that fails can be made by hand.
            do {
                try await calendar.link(stored.id, to: event, project: recording.target.projectID, in: spaceURL)
            } catch {
                print("Knode: the recording could not be linked to its event: \(error.localizedDescription)")
            }
        }
        return SavedRecording(space: space.id, spaceURL: spaceURL, recording: stored)
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

    /// The files the stored recording does not list among its tracks, by role and name.
    static func missing(_ tracks: [RecordedTrack], from stored: StoredRecording) -> [RecordedTrack] {
        tracks.filter { track in
            !stored.tracks.contains { $0.role == track.role && $0.name == track.url.lastPathComponent }
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
///
/// As soon as a recording stops, its files move to the unsaved recordings
/// (`UnsavedRecordingStore`), and leave them once the space holds them; one that could not be
/// saved stays there to save again, show in Finder or delete.
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

    /// Where and how the last recording went, when it stopped by itself at the limit, so a new
    /// one can go on with the meeting.
    struct Continuation: Hashable, Sendable {
        var mode: RecordingMode
        var target: RecordingTarget
    }

    static let notice = "This meeting is being recorded and transcribed on my device by Knode. The audio and the transcript stay on my computer."
    static let inputChangedNotice = "The microphone changed. The recording goes on with the new one."
    static let limitReachedNote = "The recording reached the longest a space keeps in one file and was saved. Start a new recording to go on with the meeting."

    private(set) var phase: Phase = .idle
    private(set) var isPaused = false
    /// Whole seconds recorded, pauses left out, updated every second for the indicator.
    private(set) var elapsedSeconds = 0
    /// A short message about the recording running now, such as the microphone changing.
    private(set) var notice: String?
    /// Whether the recording running now is close to the longest a space keeps.
    private(set) var isNearLimit = false
    /// Set when the last recording stopped by itself at the limit.
    private(set) var continuation: Continuation?
    /// Finished recordings not in their space yet.
    private(set) var unsaved: [UnsavedRecording] = []
    /// The unsaved recordings being saved again now.
    private(set) var resaving: Set<UnsavedRecording.ID> = []

    @ObservationIgnored private let recorder: any MeetingRecording
    @ObservationIgnored private let sink: any RecordingSink
    @ObservationIgnored private let processor: RecordingsProcessor
    @ObservationIgnored private let consent: any RecordingConsentStore
    @ObservationIgnored private let unsavedStore: UnsavedRecordingStore
    @ObservationIgnored private let limits: RecordingLimits
    @ObservationIgnored private let now: () -> Date
    @ObservationIgnored private var startedAt = Date()
    @ObservationIgnored private var key = UUID()
    /// The recording being transcribed or summarised, once stored.
    @ObservationIgnored private var stored: Node.ID?
    /// The unsaved recording the controller is saving now, which a retry leaves alone.
    @ObservationIgnored private var saving: UnsavedRecording.ID?
    @ObservationIgnored private var processing: Task<Void, Never>?
    @ObservationIgnored private var ticker: Task<Void, Never>?
    @ObservationIgnored private var noticeTask: Task<Void, Never>?
    @ObservationIgnored private var inputChanges = 0
    @ObservationIgnored private var retryingAll = false

    init(
        recorder: any MeetingRecording,
        sink: any RecordingSink,
        processor: RecordingsProcessor,
        consent: any RecordingConsentStore,
        unsaved: UnsavedRecordingStore,
        limits: RecordingLimits = RecordingLimits(),
        now: @escaping () -> Date = Date.init
    ) {
        self.recorder = recorder
        self.sink = sink
        self.processor = processor
        self.consent = consent
        self.unsavedStore = unsaved
        self.limits = limits
        self.now = now
    }

    var isRecording: Bool {
        if case .recording = phase { true } else { false }
    }

    var isSaving: Bool { phase == .saving }

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

    /// Whether the processing running now can be cancelled: not while saving, which would leave
    /// the recording half stored.
    var canCancelProcessing: Bool {
        switch phase {
        case .transcribing, .summarising: true
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
            isNearLimit = false
            continuation = nil
            notice = nil
            inputChanges = recorder.inputChanges
            phase = .recording(mode, target)
            startTicking()
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    /// Starts a new recording of the meeting whose recording stopped at the limit, to the same
    /// space and project. The consent was given for this meeting already.
    func recordAgain() async {
        guard let continuation, canRequest else { return }
        phase = .choosingTarget(continuation.mode)
        await start(target: continuation.target)
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
        stop(atLimit: false)
    }

    private func stop(atLimit: Bool) {
        guard case .recording(let mode, let target) = phase else { return }
        let duration = recorder.elapsed
        let endedAt = now()
        stopTicking()
        phase = .saving
        isPaused = false
        isNearLimit = false
        notice = nil
        processing = Task {
            await self.finish(mode: mode, target: target, duration: duration, endedAt: endedAt, atLimit: atLimit)
        }
    }

    /// Waits for the background work started by `stop()`.
    func waitUntilProcessed() async {
        await processing?.value
    }

    /// Stops transcribing or summarising. What was saved stays saved, and the stage cancelled
    /// can be retried. Saving is never interrupted.
    func cancelProcessing() {
        guard canCancelProcessing, let stored else { return }
        processor.cancel(stored)
    }

    /// Clears a finished or failed recording.
    func dismiss() {
        switch phase {
        case .done, .failed: phase = .idle
        default: break
        }
    }

    // MARK: Quitting

    /// Whether quitting now has to wait: a recording runs or is being saved.
    var shouldDelayTermination: Bool { isRecording || isSaving }

    /// Before the app quits: stops a running recording, so its files are closed and complete,
    /// and waits for it to be saved, at most `timeout`. Whatever is not saved by then stays
    /// among the unsaved recordings, to be saved at the next launch.
    func prepareForTermination(timeout: Duration) async {
        if isRecording { stop() }
        let deadline = ContinuousClock.now + timeout
        while isSaving, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    // MARK: Unsaved recordings

    /// Reads the unsaved recordings again.
    func refreshUnsaved() {
        unsaved = unsavedStore.list()
    }

    /// Tries to save every unsaved recording again, as at launch and when Knode Service starts.
    func saveUnsaved() async {
        guard !retryingAll else { return }
        retryingAll = true
        defer { retryingAll = false }
        refreshUnsaved()
        for item in unsaved where item.id != saving {
            await saveAgain(item)
        }
    }

    /// Saves one unsaved recording again; once its space holds it, it is transcribed like any
    /// other. Returns why it failed, if it did.
    @discardableResult
    func saveAgain(_ item: UnsavedRecording) async -> String? {
        guard item.id != saving, !resaving.contains(item.id) else { return nil }
        resaving.insert(item.id)
        defer {
            resaving.remove(item.id)
            refreshUnsaved()
        }
        do {
            let saved = try await sink.save(item.recording)
            try? unsavedStore.delete(item.id)
            processor.scan(spaceAt: saved.spaceURL)
            return nil
        } catch {
            unsavedStore.setError(error.localizedDescription, of: item.id)
            return error.localizedDescription
        }
    }

    /// Deletes an unsaved recording and its audio for good.
    func deleteUnsaved(_ item: UnsavedRecording) throws {
        guard item.id != saving, !resaving.contains(item.id) else { return }
        defer { refreshUnsaved() }
        try unsavedStore.delete(item.id)
    }

    // MARK: Running

    private func startTicking() {
        ticker?.cancel()
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, self.isRecording else { return }
                self.tick()
                try? await Task.sleep(for: .milliseconds(250))
            }
        }
    }

    private func tick() {
        let elapsed = recorder.elapsed
        let seconds = Int(elapsed.components.seconds)
        if seconds != elapsedSeconds { elapsedSeconds = seconds }
        let changes = recorder.inputChanges
        if changes != inputChanges {
            inputChanges = changes
            show(Self.inputChangedNotice)
        }
        if elapsed >= limits.stopAfter {
            stop(atLimit: true)
        } else if elapsed >= limits.warnAfter, !isNearLimit {
            isNearLimit = true
        }
    }

    /// The minutes left before the recording stops by itself.
    var minutesToLimit: Int {
        let left = limits.stopAfter - (isRecording ? recorder.elapsed : .zero)
        return max(Int((Double(left.components.seconds) / 60).rounded(.up)), 0)
    }

    private func show(_ message: String) {
        notice = message
        noticeTask?.cancel()
        noticeTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(6))
            guard !Task.isCancelled else { return }
            self?.notice = nil
        }
    }

    private func stopTicking() {
        ticker?.cancel()
        ticker = nil
    }

    /// Stops the recorder, moves the files to the unsaved recordings, stores the recording, then
    /// hands it to the processor, the one pipeline that transcribes and summarises every
    /// recording, and follows its progress.
    private func finish(mode: RecordingMode, target: RecordingTarget, duration: Duration, endedAt: Date, atLimit: Bool) async {
        var recording: FinishedRecording
        do {
            let tracks = try await recorder.stop()
            recording = FinishedRecording(
                mode: mode, target: target, startedAt: startedAt, endedAt: endedAt, duration: duration,
                tracks: tracks, key: "recorder:\(key.uuidString)"
            )
        } catch {
            phase = .failed(error.localizedDescription)
            return
        }
        // Kept where it survives a failed save or the app quitting; if that fails, it is saved
        // from where the recorder left it.
        var kept: UnsavedRecording.ID?
        if let item = try? unsavedStore.keep(recording) {
            recording = item.recording
            kept = item.id
        }
        saving = kept
        let saved: SavedRecording
        do {
            saved = try await sink.save(recording)
            if let kept { try? unsavedStore.delete(kept) }
            saving = nil
        } catch {
            if let kept { unsavedStore.setError(error.localizedDescription, of: kept) }
            saving = nil
            refreshUnsaved()
            phase = .failed(error.localizedDescription)
            return
        }

        if atLimit {
            // Saved: the person can go on with the meeting at once, while it is transcribed in
            // the background like any other recording.
            continuation = Continuation(mode: mode, target: target)
            phase = .done(note: Self.limitReachedNote)
            Task { _ = await processor.process(saved.recording.id, in: saved.space, spaceURL: saved.spaceURL) { _ in } }
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
