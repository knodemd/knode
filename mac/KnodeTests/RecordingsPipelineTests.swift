import Foundation
import Synchronization
import Testing
@testable import Knode

// MARK: Fakes

/// A space's recordings in memory, as the core keeps them.
actor FakeRecordingsBackend: RecordingsBackend {
    struct Failed: Equatable {
        var id: Node.ID
        var stage: StoredRecording.Stage
        var failure: StoredRecording.Failure
        var error: String
        var engine: RecordingEngine
    }

    let space: Space
    let audio: URL
    var recordings: [Node.ID: StoredRecording] = [:]
    var bodies: [Node.ID: String] = [:]
    var created: [NewRecordingDraft] = []
    var transcripts: [Node.ID: (transcript: Transcript, engine: RecordingEngine)] = [:]
    var summaries: [Node.ID: (summary: MeetingSummary, engine: RecordingEngine)] = [:]
    var failed: [Failed] = []
    var retried: [Node.ID] = []
    var summariesOn = true
    private var counter = 0
    private nonisolated let broadcast = Broadcast<SpaceChange>()

    init() throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "knode-rec-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        audio = folder
        space = Space(id: "space-1", name: "Work", url: folder.appending(path: "Work.knode", directoryHint: .isDirectory))
    }

    nonisolated var spaceURL: URL { space.url! }
    /// Unsaved recordings kept beside the space, never in the real Knode home.
    nonisolated var unsaved: UnsavedRecordingStore { UnsavedRecordingStore(root: audio.appending(path: "Unsaved Recordings", directoryHint: .isDirectory)) }
    /// Tracks a create leaves out, as a space that failed to copy them would.
    var dropTracks: Set<RecordedTrack.Role> = []
    /// Set to make `failedRecordings` fail, as a core without `recordings.failed` does.
    var failedUnsupported = false
    var summariesSet: [Bool] = []

    func setDropTracks(_ roles: Set<RecordedTrack.Role>) { dropTracks = roles }
    func setFailedUnsupported(_ on: Bool) { failedUnsupported = on }

    func setSummaries(_ on: Bool) { summariesOn = on }
    func transcript(of id: Node.ID) -> (transcript: Transcript, engine: RecordingEngine)? { transcripts[id] }
    func summary(of id: Node.ID) -> (summary: MeetingSummary, engine: RecordingEngine)? { summaries[id] }

    /// A recording brought in by someone else, such as a voice memo from a connector.
    @discardableResult
    func add(kind: StoredRecording.Kind, tracks: [(String, RecordedTrack.Role)], summary: StoredRecording.SummaryState = .pending, announce: Bool = false) throws -> Node.ID {
        counter += 1
        let id = "rec-\(counter)"
        for (name, _) in tracks { try Data("audio".utf8).write(to: audio.appending(path: "\(id)-\(name)")) }
        recordings[id] = StoredRecording(
            id: id, kind: kind, title: "Recording \(counter)", startedAt: Date(timeIntervalSince1970: TimeInterval(counter)),
            endedAt: Date(timeIntervalSince1970: TimeInterval(counter + 60)), language: nil, transcript: .pending, summary: summary,
            error: nil, project: nil, tracks: tracks.map { .init(name: $0.0, role: $0.1) }, transcriptID: nil
        )
        if announce { broadcast.yield(SpaceChange(space: space.id, nodeID: id, kind: .create)) }
        return id
    }

    func openSpace(at url: URL) async throws -> Space { space }

    func node(_ id: Node.ID, in space: Space.ID) async throws -> Node {
        guard let body = bodies[id] else { throw SpaceStoreError.nodeNotFound(id) }
        return Node(id: id, title: "Transcript", body: body, createdAt: .now, updatedAt: .now, kind: "transcript")
    }

    func attachmentURL(named name: String, of node: Node.ID, in space: Space.ID) async throws -> URL? {
        let url = audio.appending(path: "\(node)-\(name)")
        return FileManager.default.fileExists(atPath: url.path(percentEncoded: false)) ? url : nil
    }

    nonisolated func changes() -> AsyncStream<SpaceChange> { broadcast.stream() }

    func createRecording(_ recording: NewRecordingDraft, in space: Space.ID) async throws -> StoredRecording {
        created.append(recording)
        counter += 1
        let id = "rec-\(counter)"
        for track in recording.audio {
            try FileManager.default.copyItem(at: track.url, to: audio.appending(path: "\(id)-\(track.url.lastPathComponent)"))
        }
        let stored = StoredRecording(
            id: id, kind: recording.kind, title: recording.title, startedAt: recording.startedAt, endedAt: recording.endedAt,
            language: nil, transcript: .pending, summary: recording.summaries ? .pending : .off, error: nil,
            project: recording.project,
            tracks: recording.audio.filter { !dropTracks.contains($0.role) }.map { .init(name: $0.url.lastPathComponent, role: $0.role) },
            transcriptID: nil
        )
        recordings[id] = stored
        return stored
    }

    func pendingRecordings(in space: Space.ID) async throws -> [StoredRecording] {
        recordings.values.filter(\.isPending).sorted { $0.startedAt < $1.startedAt }
    }

    func failedRecordings(in space: Space.ID) async throws -> [StoredRecording] {
        if failedUnsupported { throw SpaceStoreError.unsupported("no recordings.failed") }
        return recordings.values.filter { $0.transcript == .failed || $0.summary == .failed || $0.summary == .unavailable }.sorted { $0.startedAt < $1.startedAt }
    }

    /// A recording that failed before the app started.
    func markFailed(_ id: Node.ID, stage: StoredRecording.Stage, error: String) {
        switch stage {
        case .transcript: recordings[id]?.transcript = .failed
        case .summary:
            recordings[id]?.transcript = .done
            recordings[id]?.summary = .failed
        }
        recordings[id]?.error = error
    }

    func recording(_ id: Node.ID, in space: Space.ID) async throws -> StoredRecording {
        guard let recording = recordings[id] else { throw SpaceStoreError.nodeNotFound(id) }
        return recording
    }

    func setTranscript(_ transcript: Transcript, by engine: RecordingEngine, of id: Node.ID, in space: Space.ID) async throws -> StoredRecording {
        transcripts[id] = (transcript, engine)
        let lines = transcript.segments.map { segment in
            let seconds = segment.startMs / 1000
            let time = String(format: "%02d:%02d:%02d", seconds / 3600, seconds / 60 % 60, seconds % 60)
            return "**\(segment.speaker ?? TranscriptSegment.unnamedSpeaker)** (\(time)): \(segment.text)"
        }
        bodies["t-\(id)"] = (["Transcribed on this Mac by \(engine.name)."] + lines).joined(separator: "\n\n") + "\n"
        recordings[id]?.transcript = .done
        recordings[id]?.language = transcript.locale
        recordings[id]?.transcriptID = "t-\(id)"
        return recordings[id]!
    }

    func setSummary(_ summary: MeetingSummary, by engine: RecordingEngine, of id: Node.ID, in space: Space.ID) async throws -> StoredRecording {
        summaries[id] = (summary, engine)
        recordings[id]?.summary = .done
        return recordings[id]!
    }

    func failRecording(_ id: Node.ID, stage: StoredRecording.Stage, failure: StoredRecording.Failure, error: String, by engine: RecordingEngine, in space: Space.ID) async throws -> StoredRecording {
        failed.append(Failed(id: id, stage: stage, failure: failure, error: error, engine: engine))
        switch stage {
        case .transcript: recordings[id]?.transcript = .failed
        case .summary: recordings[id]?.summary = failure == .unavailable ? .unavailable : .failed
        }
        recordings[id]?.error = error
        return recordings[id]!
    }

    func retryRecording(_ id: Node.ID, stage: StoredRecording.Stage, in space: Space.ID) async throws -> StoredRecording {
        retried.append(id)
        switch stage {
        case .transcript:
            // As the core does: the language is listened for again, and done minutes made again.
            recordings[id]?.transcript = .pending
            recordings[id]?.language = nil
            if recordings[id]?.summary == .done { recordings[id]?.summary = .pending }
        case .summary: recordings[id]?.summary = .pending
        }
        recordings[id]?.error = nil
        return recordings[id]!
    }

    func recordingSummariesEnabled(in space: Space.ID) async throws -> Bool { summariesOn }

    func setRecordingSummariesEnabled(_ enabled: Bool, in space: Space.ID) async throws {
        summariesSet.append(enabled)
        summariesOn = enabled
    }
}

/// A transcriber that remembers the tracks it was given and can fail until told otherwise.
final class RecordingTranscriber: Transcriber {
    struct State {
        var calls: [[RecordedTrack.Role]] = []
        var readable: [Bool] = []
        var error: TranscriptionError?
    }

    let state: Mutex<State>
    /// Runs while a transcription is under way, after its first progress, before it ends.
    let during = Mutex<(@Sendable () async -> Void)?>(nil)

    init(error: TranscriptionError? = nil) {
        state = Mutex(State(error: error))
    }

    func transcribe(_ tracks: [RecordedTrack], locale: Locale?, progress: @escaping @Sendable (Double) -> Void) async throws -> Transcript {
        let error = state.withLock { state in
            state.calls.append(tracks.map(\.role))
            state.readable.append(contentsOf: tracks.map { FileManager.default.isReadableFile(atPath: $0.url.path(percentEncoded: false)) })
            return state.error
        }
        if let error { throw error }
        progress(0.5)
        if let during = during.withLock({ $0 }) { await during() }
        return Transcript(locale: "es-ES", segments: tracks.enumerated().map { index, track in
            TranscriptSegment(startMs: index * 1000, endMs: index * 1000 + 900, speaker: track.speaker, text: "Hola \(index)")
        })
    }
}

/// A summarizer that remembers the transcripts it read, and can wait until cancelled.
final class RecordingSummarizer: Summarizer {
    let read = Mutex<[Transcript]>([])
    let availableNow: Mutex<SummarizerAvailability>
    let blocks: Bool

    init(available: SummarizerAvailability = .available, blocks: Bool = false) {
        self.availableNow = Mutex(available)
        self.blocks = blocks
    }

    func availability(for transcript: Transcript) async -> SummarizerAvailability { availableNow.withLock { $0 } }

    func summarize(_ transcript: Transcript, progress: @escaping @Sendable (Double) -> Void) async throws -> MeetingSummary {
        read.withLock { $0.append(transcript) }
        if blocks {
            while true { try await Task.sleep(for: .milliseconds(10)) }
        }
        return MeetingSummary(model: "fake", overview: "Resumen", keyPoints: [], decisions: ["Ship"], actionItems: [], openQuestions: [])
    }
}

// MARK: Processor

@MainActor
struct RecordingsProcessorTests {
    private func processor(_ store: FakeRecordingsBackend, transcriber: RecordingTranscriber = RecordingTranscriber(), summarizer: RecordingSummarizer = RecordingSummarizer()) -> RecordingsProcessor {
        let url = store.spaceURL
        return RecordingsProcessor(store: store, transcriber: transcriber, summarizer: summarizer, spaceURLs: { [url] })
    }

    @Test func transcribesThenSummarisesAPendingMeeting() async throws {
        let store = try FakeRecordingsBackend()
        let id = try await store.add(kind: .meeting, tracks: [("microphone.m4a", .microphone), ("system.m4a", .system)])
        let transcriber = RecordingTranscriber()
        let processor = processor(store, transcriber: transcriber)
        processor.start()
        #expect(try await eventually { await store.recordings[id]?.summary == .done })

        let transcript = try #require(await store.transcript(of: id))
        #expect(transcript.engine == .speech)
        #expect(transcript.transcript.segments.map(\.speaker) == ["Me", "Others"])
        #expect(transcriber.state.withLock { $0.calls } == [[.microphone, .system]])
        #expect(transcriber.state.withLock { $0.readable } == [true, true])
        #expect(await store.summary(of: id)?.engine == .minutes)
        #expect(await store.summary(of: id)?.summary.decisions == ["Ship"])
        #expect(await store.failed.isEmpty)
        #expect(processor.current == nil)
        await processor.stop()
    }

    @Test func transcribesAVoiceMemoItDidNotMakeWhenItIsAnnounced() async throws {
        let store = try FakeRecordingsBackend()
        let transcriber = RecordingTranscriber()
        let processor = processor(store, transcriber: transcriber)
        processor.start()
        // Nothing to do yet; then a connector brings a voice memo in.
        #expect(try await eventually { await processor.current == nil })
        let id = try await store.add(kind: .voiceMemo, tracks: [("Memo.m4a", .mixed)], summary: .off, announce: true)
        #expect(try await eventually { await store.recordings[id]?.transcript == .done })

        #expect(transcriber.state.withLock { $0.calls } == [[.mixed]])
        let transcript = try #require(await store.transcript(of: id)?.transcript)
        #expect(transcript.segments.map(\.speaker) == [nil])
        // One microphone heard everyone: stored as `Speaker`.
        let wire = RecordingWire.transcript(transcript, engine: .speech)
        guard case .object(let object) = wire, case .array(let segments) = object["segments"], case .object(let first) = segments.first else {
            Issue.record("no segments")
            return
        }
        #expect(first["speaker"] == .string("Speaker"))
        #expect(await store.summary(of: id) == nil)
        await processor.stop()
    }

    @Test func aTranscriptionErrorMarksTheTranscriptFailedAndCanBeRetried() async throws {
        let store = try FakeRecordingsBackend()
        let id = try await store.add(kind: .voiceMemo, tracks: [("Memo.m4a", .mixed)])
        let transcriber = RecordingTranscriber(error: .unsupportedLocale("xx"))
        let processor = processor(store, transcriber: transcriber)
        processor.start()
        #expect(try await eventually { await processor.failures.count == 1 })

        let reason = TranscriptionError.unsupportedLocale("xx").localizedDescription
        #expect(await store.failed == [.init(id: id, stage: .transcript, failure: .failed, error: reason, engine: .speech)])
        #expect(processor.failures.first?.message == reason)

        transcriber.state.withLock { $0.error = nil }
        try await processor.retry(try #require(processor.failures.first))
        #expect(await store.retried == [id])
        #expect(processor.failures.isEmpty)
        #expect(try await eventually { await store.recordings[id]?.summary == .done })
        await processor.stop()
    }

    @Test func aRecordingCanBeTranscribedAgain() async throws {
        let store = try FakeRecordingsBackend()
        let id = try await store.add(kind: .meeting, tracks: [("microphone.m4a", .microphone), ("system.m4a", .system)])
        let transcriber = RecordingTranscriber()
        let processor = processor(store, transcriber: transcriber)
        processor.start()
        #expect(try await eventually { await store.recordings[id]?.summary == .done })
        #expect(transcriber.state.withLock { $0.calls.count } == 1)

        try await processor.transcribeAgain(id, in: store.space.id)
        #expect(await store.retried == [id])
        #expect(try await eventually { transcriber.state.withLock { $0.calls.count } == 2 })
        #expect(try await eventually { await store.recordings[id]?.summary == .done })
        #expect(RecordingCommands.isRecording("meeting"))
        #expect(RecordingCommands.isRecording("voice-memo"))
        #expect(!RecordingCommands.isRecording("note"))
        #expect(!RecordingCommands.isRecording(nil))
        await processor.stop()
    }

    @Test func aMissingAudioFileFailsTheTranscript() async throws {
        let store = try FakeRecordingsBackend()
        let id = try await store.add(kind: .voiceMemo, tracks: [("Memo.m4a", .mixed)])
        try FileManager.default.removeItem(at: store.audio.appending(path: "\(id)-Memo.m4a"))
        let processor = processor(store)
        processor.start()
        #expect(try await eventually { await store.failed.count == 1 })
        #expect(await store.failed.first?.error == RecordingsProcessorError.missingAudio("Memo.m4a").localizedDescription)
        await processor.stop()
    }

    @Test func minutesUnavailableOnThisMacMarkTheSummaryUnavailable() async throws {
        let store = try FakeRecordingsBackend()
        let id = try await store.add(kind: .meeting, tracks: [("microphone.m4a", .mixed)])
        let processor = processor(store, summarizer: RecordingSummarizer(available: .unavailable(.appleIntelligenceNotEnabled)))
        processor.start()
        #expect(try await eventually { await store.recordings[id]?.summary == .unavailable })

        let reason = SummarizerUnavailableReason.appleIntelligenceNotEnabled.message
        #expect(await store.failed == [.init(id: id, stage: .summary, failure: .unavailable, error: reason, engine: .minutes)])
        #expect(await store.recordings[id]?.transcript == .done)
        // Unavailable is not a failure to retry by hand.
        #expect(processor.failures.isEmpty)
        await processor.stop()
    }

    @Test func minutesAreWrittenOnTheirOwnOnceTheModelIsReady() async throws {
        let store = try FakeRecordingsBackend()
        let id = try await store.add(kind: .meeting, tracks: [("microphone.m4a", .mixed)])
        let summarizer = RecordingSummarizer(available: .unavailable(.modelNotReady))
        let url = store.spaceURL
        let processor = RecordingsProcessor(
            store: store, transcriber: RecordingTranscriber(), summarizer: summarizer, spaceURLs: { [url] },
            rescanInterval: .milliseconds(50)
        )
        processor.start()
        #expect(try await eventually { await store.recordings[id]?.summary == .unavailable })
        #expect(await store.retried.isEmpty)

        // Apple's model finished downloading: the next look at the space writes the minutes.
        summarizer.availableNow.withLock { $0 = .available }
        #expect(try await eventually { await store.recordings[id]?.summary == .done })
        #expect(await store.retried == [id])
        await processor.stop()
    }

    @Test func aRestartPicksUpWorkLeftPending() async throws {
        let store = try FakeRecordingsBackend()
        let id = try await store.add(kind: .meeting, tracks: [("microphone.m4a", .microphone), ("system.m4a", .system)])

        // The app quits while the minutes are being written: nothing is marked failed.
        let first = processor(store, summarizer: RecordingSummarizer(blocks: true))
        first.start()
        #expect(try await eventually { await first.current?.step == .summarising })
        await first.stop()
        #expect(await store.recordings[id]?.transcript == .done)
        #expect(await store.recordings[id]?.summary == .pending)
        #expect(await store.failed.isEmpty)

        // Next launch: the minutes are written from the stored transcript.
        let summarizer = RecordingSummarizer()
        let transcriber = RecordingTranscriber()
        let second = processor(store, transcriber: transcriber, summarizer: summarizer)
        second.start()
        #expect(try await eventually { await store.recordings[id]?.summary == .done })
        #expect(transcriber.state.withLock { $0.calls }.isEmpty)
        let read = try #require(summarizer.read.withLock { $0.first })
        #expect(read.segments.map(\.text) == ["Hola 0", "Hola 1"])
        #expect(read.segments.map(\.speaker) == ["Me", "Others"])
        #expect(read.locale == "es-ES")
        await second.stop()
    }

    @Test func cancellingMarksTheStageFailedWithCancelled() async throws {
        let store = try FakeRecordingsBackend()
        let id = try await store.add(kind: .meeting, tracks: [("microphone.m4a", .mixed)])
        let processor = processor(store, summarizer: RecordingSummarizer(blocks: true))
        processor.start()
        #expect(try await eventually { await processor.current?.step == .summarising })
        processor.cancel(id)
        #expect(try await eventually { await store.recordings[id]?.summary == .failed })
        #expect(await store.failed.first?.error == RecordingsProcessor.cancelled)
        #expect(processor.failures.map(\.stage) == [.summary])
        await processor.stop()
    }
}

// MARK: Controller hand-off

@MainActor
struct RecordingHandOffTests {
    let consent = MemoryRecordingConsentStore(hasAcknowledged: true)

    @Test func theControllerStoresTheMeetingAndHandsItToTheProcessor() async throws {
        let store = try FakeRecordingsBackend()
        let recorder = FakeRecorder()
        let transcriber = RecordingTranscriber()
        let url = store.spaceURL
        let processor = RecordingsProcessor(store: store, transcriber: transcriber, summarizer: RecordingSummarizer(), spaceURLs: { [url] })
        let sink = StoreRecordingSink(store: store, locale: Locale(identifier: "en_GB"))
        let controller = RecordingController(recorder: recorder, sink: sink, processor: processor, consent: consent, unsaved: store.unsaved)
        // While the transcriber works, the controller shows it: waited for, so it is never missed.
        let sawTranscribing = Mutex(false)
        transcriber.during.withLock {
            $0 = { @Sendable in
                let saw = (try? await eventually { @MainActor in
                    if case .transcribing = controller.phase { true } else { false }
                }) ?? false
                sawTranscribing.withLock { $0 = saw }
            }
        }

        controller.request(.online(.allApps))
        await controller.start(target: RecordingTarget(spaceURL: url, spaceName: "Work", projectID: "project-1", projectTitle: "Launch"))
        controller.stop()
        await controller.waitUntilProcessed()

        let draft = try #require(await store.created.first)
        #expect(draft.kind == .meeting)
        #expect(draft.title.hasPrefix("Meeting, "))
        #expect(draft.project == "project-1")
        #expect(draft.summaries)
        #expect(draft.audio.map(\.role) == [.microphone, .system])
        #expect(draft.key?.hasPrefix("recorder:") == true)
        // The temporary audio is gone once the space holds it.
        let folder = try #require(recorder.folder)
        #expect(!FileManager.default.fileExists(atPath: folder.path(percentEncoded: false)))

        let id = try #require(await store.recordings.keys.first)
        #expect(await store.recordings[id]?.summary == .done)
        #expect(transcriber.state.withLock { $0.calls } == [[.microphone, .system]])
        #expect(controller.phase == .done(note: nil))
        #expect(sawTranscribing.withLock { $0 })
    }

    @Test func noMinutesWhenTheSpaceTurnedThemOff() async throws {
        let store = try FakeRecordingsBackend()
        await store.setSummaries(false)
        let url = store.spaceURL
        let summarizer = RecordingSummarizer()
        let processor = RecordingsProcessor(store: store, transcriber: RecordingTranscriber(), summarizer: summarizer, spaceURLs: { [url] })
        let controller = RecordingController(recorder: FakeRecorder(), sink: StoreRecordingSink(store: store), processor: processor, consent: consent, unsaved: store.unsaved)
        controller.request(.inPerson)
        await controller.start(target: RecordingTarget(spaceURL: url, spaceName: "Work"))
        controller.stop()
        await controller.waitUntilProcessed()

        let draft = try #require(await store.created.first)
        #expect(!draft.summaries)
        #expect(draft.title.hasPrefix("In-person meeting, "))
        #expect(draft.audio.map(\.role) == [.mixed])
        #expect(summarizer.read.withLock { $0 }.isEmpty)
        #expect(controller.phase == .done(note: nil))
    }

    @Test func aFileTooLargeForTheSpaceFailsBeforeAnythingIsStored() async throws {
        let store = try FakeRecordingsBackend()
        let url = store.spaceURL
        let processor = RecordingsProcessor(store: store, transcriber: RecordingTranscriber(), summarizer: RecordingSummarizer(), spaceURLs: { [url] })
        let recorder = FakeRecorder()
        let controller = RecordingController(recorder: recorder, sink: StoreRecordingSink(store: store, limit: 2), processor: processor, consent: consent, unsaved: store.unsaved)
        controller.request(.inPerson)
        await controller.start(target: RecordingTarget(spaceURL: url, spaceName: "Work"))
        controller.stop()
        await controller.waitUntilProcessed()

        #expect(await store.created.isEmpty)
        guard case .failed(let message) = controller.phase else {
            Issue.record("expected a failure, got \(controller.phase)")
            return
        }
        #expect(message.contains("too long to keep"))
        // The audio is kept among the unsaved recordings, so nothing is lost.
        let unsaved = try #require(controller.unsaved.first)
        #expect(controller.unsaved.count == 1)
        #expect(unsaved.error?.contains("too long to keep") == true)
        #expect(unsaved.recording.tracks.allSatisfy { FileManager.default.fileExists(atPath: $0.url.path(percentEncoded: false)) })
    }
}

// MARK: Titles, transcripts and sizes

struct RecordingFormatTests {
    @Test func titlesSayWhatAndWhenInTheLocale() {
        let date = Date(timeIntervalSince1970: 1_795_343_400) // 22 November 2026, 10:30 UTC
        let utc = TimeZone(identifier: "UTC")!
        let british = Locale(identifier: "en_GB")
        #expect(RecordingTitle.of(.online(.allApps), startedAt: date, locale: british, timeZone: utc) == "Meeting, 22 November 2026 10:30")
        #expect(RecordingTitle.of(.inPerson, startedAt: date, locale: british, timeZone: utc) == "In-person meeting, 22 November 2026 10:30")
    }

    @Test func readsATranscriptBackFromItsNode() {
        let body = """
        Transcribed on this Mac by Apple SpeechAnalyzer (es-ES).

        **Me** (00:00:05): Hola. Qué tal.

        **Others** (01:02:03): Bien.

        **Speaker** (00:10:00): Todos.

        """
        let transcript = TranscriptNode.transcript(fromBody: body, language: "es-ES")
        #expect(transcript.locale == "es-ES")
        #expect(transcript.segments == [
            TranscriptSegment(startMs: 5000, endMs: 5000, speaker: "Me", text: "Hola. Qué tal."),
            TranscriptSegment(startMs: 3_723_000, endMs: 3_723_000, speaker: "Others", text: "Bien."),
            TranscriptSegment(startMs: 600_000, endMs: 600_000, speaker: nil, text: "Todos."),
        ])
    }

    @Test func summariesAreOnUnlessTheSpaceSaysOff() {
        #expect(RecordingSummariesSetting.isOn(nil))
        #expect(RecordingSummariesSetting.isOn("on"))
        #expect(!RecordingSummariesSetting.isOn("off"))
    }

    @Test func anHourOfAudioFitsOneAttachment() {
        let bytesPerHour = Int64(TrackWriter.bitRate / 8 * 3600)
        #expect(bytesPerHour < 30_000_000)
        #expect(bytesPerHour * 3 < AttachmentInfo.maxSize)
    }
}

// MARK: Wire

/// The `recordings.*` calls against a fake Knode Service, checked against the shapes of
/// `PROTOCOL.md`, "Recordings".
@Suite(.serialized) final class RecordingWireTests {
    let service: FakeKnodeService
    let path = "/tmp/Recordings.knode"

    init() throws {
        service = try FakeKnodeService()
        for method in ["recordings.create", "recordings.get", "recordings.transcript", "recordings.summary", "recordings.fail", "recordings.retry"] {
            service.answer(method, with: Self.recording)
        }
        service.answer("recordings.pending", with: [Self.recording])
        service.answer("recordings.failed", with: [Self.recording])
    }

    deinit {
        service.stop()
    }

    static var recording: [String: Any] { json("""
    {
      "id": "01JREC", "kind": "meeting", "title": "Weekly sync",
      "started_at": 1795343400000, "ended_at": 1795345200000, "duration_ms": 1800000,
      "source": "recorder", "key": null, "language": "es-ES",
      "transcript": "done", "summary": "pending", "error": null, "app": "zoom.us",
      "project": "01JPRJ", "transcript_id": "01JTRN",
      "tracks": [{ "name": "microphone.m4a", "track": "microphone" }, { "name": "system.m4a", "track": "system" }]
    }
    """) }

    static func json(_ text: String) -> [String: Any] {
        (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any] ?? [:]
    }

    func store() async throws -> (ServiceSpaceStore, Space.ID) {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let space = try await store.openSpace(at: URL(filePath: path, directoryHint: .isDirectory))
        return (store, space.id)
    }

    func expectParams(_ method: String, _ fixture: String) {
        let sent = service.params(of: method) ?? [:]
        #expect(NSDictionary(dictionary: sent) == NSDictionary(dictionary: Self.json(fixture)), "\(method): \(sent)")
    }

    @Test func createSendsThePersonAndEveryFile() async throws {
        let (store, space) = try await store()
        let draft = NewRecordingDraft(
            kind: .meeting, title: "Meeting, 22 November 2026 07:50",
            startedAt: Date(timeIntervalSince1970: 1_795_343_400), endedAt: Date(timeIntervalSince1970: 1_795_345_200),
            project: "01JPRJ",
            audio: [
                RecordedTrack(role: .microphone, url: URL(filePath: "/tmp/r/microphone.m4a")),
                RecordedTrack(role: .system, url: URL(filePath: "/tmp/r/system.m4a")),
            ],
            key: "recorder:1", app: "us.zoom.xos", summaries: true
        )
        let recording = try await store.createRecording(draft, in: space)
        expectParams("recordings.create", """
        { "space": "\(path)", "kind": "meeting", "title": "Meeting, 22 November 2026 07:50",
          "started_at": 1795343400000, "ended_at": 1795345200000, "project": "01JPRJ",
          "audio": [{ "path": "/tmp/r/microphone.m4a", "track": "microphone" }, { "path": "/tmp/r/system.m4a", "track": "system" }],
          "key": "recorder:1", "app": "us.zoom.xos", "summaries": true,
          "by": { "kind": "person", "actor": "Ada" } }
        """)
        #expect(recording.id == "01JREC")
        #expect(recording.kind == .meeting)
        #expect(recording.startedAt == Date(timeIntervalSince1970: 1_795_343_400))
        #expect(recording.transcript == .done)
        #expect(recording.summary == .pending)
        #expect(recording.project == "01JPRJ")
        #expect(recording.transcriptID == "01JTRN")
        #expect(recording.tracks == [.init(name: "microphone.m4a", role: .microphone), .init(name: "system.m4a", role: .system)])
    }

    @Test func theTranscriptIsWrittenByTheSpeechEngine() async throws {
        let (store, space) = try await store()
        let transcript = Transcript(locale: "es-ES", segments: [
            TranscriptSegment(startMs: 61_000, endMs: 62_000, speaker: "Me", text: "Hola"),
            TranscriptSegment(startMs: 63_000, endMs: 64_000, speaker: nil, text: "Todos"),
        ])
        _ = try await store.setTranscript(transcript, by: .speech, of: "01JREC", in: space)
        expectParams("recordings.transcript", """
        { "space": "\(path)", "id": "01JREC",
          "transcript": { "language": "es-ES", "engine": "Apple SpeechAnalyzer", "segments": [
            { "start_ms": 61000, "end_ms": 62000, "speaker": "Me", "text": "Hola" },
            { "start_ms": 63000, "end_ms": 64000, "speaker": "Speaker", "text": "Todos" } ] },
          "by": { "kind": "system", "actor": "recordings", "source": "apple-speech" } }
        """)
    }

    @Test func theMinutesAreWrittenByTheModel() async throws {
        let (store, space) = try await store()
        let summary = MeetingSummary(
            model: "Apple Foundation Models (on-device)", overview: "Short.", keyPoints: ["Budget"], decisions: ["Ship"],
            actionItems: [ActionItem(task: "Send the deck", owner: "Ada"), ActionItem(task: "Book a room", owner: nil)],
            openQuestions: ["When?"]
        )
        _ = try await store.setSummary(summary, by: .minutes, of: "01JREC", in: space)
        expectParams("recordings.summary", """
        { "space": "\(path)", "id": "01JREC",
          "summary": { "model": "Apple Foundation Models (on-device)", "overview": "Short.", "key_points": ["Budget"],
            "decisions": ["Ship"], "action_items": [{ "task": "Send the deck", "owner": "Ada" }, { "task": "Book a room" }],
            "open_questions": ["When?"] },
          "by": { "kind": "system", "actor": "recordings", "source": "apple-foundation-models" } }
        """)
    }

    @Test func failRetryPendingAndGet() async throws {
        let (store, space) = try await store()
        _ = try await store.failRecording("01JREC", stage: .summary, failure: .unavailable, error: "Off", by: .minutes, in: space)
        expectParams("recordings.fail", """
        { "space": "\(path)", "id": "01JREC", "stage": "summary", "state": "unavailable", "error": "Off",
          "by": { "kind": "system", "actor": "recordings", "source": "apple-foundation-models" } }
        """)
        _ = try await store.retryRecording("01JREC", stage: .transcript, in: space)
        expectParams("recordings.retry", """
        { "space": "\(path)", "id": "01JREC", "stage": "transcript", "by": { "kind": "person", "actor": "Ada" } }
        """)
        let pending = try await store.pendingRecordings(in: space)
        expectParams("recordings.pending", #"{ "space": "\#(path)" }"#)
        #expect(pending.map(\.id) == ["01JREC"])
        let failed = try await store.failedRecordings(in: space)
        expectParams("recordings.failed", #"{ "space": "\#(path)" }"#)
        #expect(failed.map(\.id) == ["01JREC"])
        _ = try await store.recording("01JREC", in: space)
        expectParams("recordings.get", #"{ "space": "\#(path)", "id": "01JREC" }"#)
    }

    @Test func minutesFollowTheSpaceSetting() async throws {
        let (store, space) = try await store()
        #expect(try await store.recordingSummariesEnabled(in: space))
        _ = try await ServiceConnection.connect(to: service.socketURL).call(
            "settings.set", ["space": .string(path), "key": "recordings.summaries", "value": "off", "by": ["kind": "person", "actor": "Ada"]],
            as: [String: String].self
        )
        #expect(try await store.recordingSummariesEnabled(in: space) == false)
        try await store.setRecordingSummariesEnabled(true, in: space)
        #expect(try await store.recordingSummariesEnabled(in: space))
    }
}

// MARK: Core in the app

/// The recordings of a real space, through the core in the app.
final class CoreRecordingsTests {
    let folder: URL

    init() throws {
        folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    }

    deinit {
        try? FileManager.default.removeItem(at: folder)
    }

    @Test func storesTranscribesAndSummarisesARecording() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: folder.appending(path: "Test.knode", directoryHint: .isDirectory), name: "Test")
        #expect(try await store.recordingSummariesEnabled(in: space.id))
        let audio = folder.appending(path: "Memo.m4a")
        try Data("audio".utf8).write(to: audio)
        let draft = NewRecordingDraft(
            kind: .voiceMemo, title: "Memo", startedAt: Date(timeIntervalSince1970: 1_795_343_400),
            endedAt: Date(timeIntervalSince1970: 1_795_343_460), project: nil,
            audio: [RecordedTrack(role: .mixed, url: audio)], key: "memo-1", app: nil, summaries: true
        )
        let created = try await store.createRecording(draft, in: space.id)
        #expect(created.kind == .voiceMemo)
        #expect(created.tracks == [.init(name: "Memo.m4a", role: .mixed)])
        #expect(try await store.createRecording(draft, in: space.id).id == created.id)
        #expect(try await store.pendingRecordings(in: space.id).map(\.id) == [created.id])
        let blob = try #require(try await store.attachmentURL(named: "Memo.m4a", of: created.id, in: space.id))
        #expect(try Data(contentsOf: blob) == Data("audio".utf8))

        let transcript = Transcript(locale: "es-ES", segments: [TranscriptSegment(startMs: 61_000, endMs: 62_000, speaker: nil, text: "Hola")])
        let transcribed = try await store.setTranscript(transcript, by: .speech, of: created.id, in: space.id)
        #expect(transcribed.transcript == .done)
        let node = try await store.node(try #require(transcribed.transcriptID), in: space.id)
        #expect(node.body.contains("**Speaker** (00:01:01): Hola"))
        #expect(TranscriptNode.transcript(fromBody: node.body, language: "es-ES").segments.map(\.text) == ["Hola"])
        let history = try await store.history(of: node.id, in: space.id)
        #expect(history.first?.author == Author(kind: .system, name: "recordings", source: "apple-speech"))

        let unavailable = try await store.failRecording(created.id, stage: .summary, failure: .unavailable, error: "Off", by: .minutes, in: space.id)
        #expect(unavailable.summary == .unavailable)
        #expect(unavailable.error == "Off")
        #expect(try await store.failedRecordings(in: space.id).map(\.id) == [created.id])
        #expect(try await store.retryRecording(created.id, stage: .summary, in: space.id).summary == .pending)
        let summary = MeetingSummary(model: "Apple Foundation Models", overview: "Short.", keyPoints: [], decisions: ["Ship"], actionItems: [ActionItem(task: "Call", owner: nil)], openQuestions: [])
        #expect(try await store.setSummary(summary, by: .minutes, of: created.id, in: space.id).summary == .done)
        #expect(try await store.pendingRecordings(in: space.id).isEmpty)
        #expect(try await store.failedRecordings(in: space.id).isEmpty)
    }
}
