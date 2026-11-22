import Foundation
import Synchronization
import Testing
@testable import Knode

// MARK: Fakes

/// A recorder that records nothing and hands back the files it is told to.
final class FakeRecorder: MeetingRecording {
    struct State {
        var mode: RecordingMode?
        var calls: [String] = []
        var paused = false
        var folder: URL?
        var elapsed: Duration = .seconds(65)
        var inputChanges = 0
    }

    let state = Mutex(State())
    let startError: RecordingError?

    init(startError: RecordingError? = nil) {
        self.startError = startError
    }

    var calls: [String] { state.withLock { $0.calls } }
    /// The private folder its files were written to, once stopped.
    var folder: URL? { state.withLock { $0.folder } }

    func start(_ mode: RecordingMode) async throws {
        state.withLock { $0.calls.append("start") }
        if let startError { throw startError }
        state.withLock { $0.mode = mode }
    }

    func pause() { state.withLock { $0.calls.append("pause"); $0.paused = true } }
    func resume() { state.withLock { $0.calls.append("resume"); $0.paused = false } }

    func stop() async throws -> [RecordedTrack] {
        let mode = state.withLock { state in
            state.calls.append("stop")
            return state.mode
        }
        let folder = FileManager.default.temporaryDirectory.appending(path: "knode-test-recording-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        state.withLock { $0.folder = folder }
        let tracks = mode == .inPerson
            ? [RecordedTrack(role: .mixed, url: folder.appending(path: "microphone.m4a"))]
            : [
                RecordedTrack(role: .microphone, url: folder.appending(path: "microphone.m4a")),
                RecordedTrack(role: .system, url: folder.appending(path: "system.m4a")),
            ]
        for track in tracks { try Data("audio".utf8).write(to: track.url) }
        return tracks
    }

    func cancel() { state.withLock { $0.calls.append("cancel") } }
    var elapsed: Duration { state.withLock { $0.elapsed } }
    var level: Float { 0.5 }
    var isPaused: Bool { state.withLock { $0.paused } }
    var inputChanges: Int { state.withLock { $0.inputChanges } }

    func setElapsed(_ elapsed: Duration) { state.withLock { $0.elapsed = elapsed } }
    func changeInput() { state.withLock { $0.inputChanges += 1 } }
}

/// A model that answers from the chunk itself: every line becomes a key point, lines with
/// "decide" a decision, "todo" an action item and "?" an open question.
final class FakeMinutesModel: MinutesModel {
    struct State {
        var chunks: [String] = []
        var overviews: [[String]] = []
    }

    let state = Mutex(State())
    let inputBudget: Int
    let maxChunk: Int
    /// Overviews of more characters than this fail as too large for the context.
    let maxOverview: Int
    let maxChunkError: MinutesModelError
    var name: String { "fake-model" }
    let available: SummarizerAvailability

    init(inputBudget: Int, maxChunk: Int = .max, maxOverview: Int = .max, maxChunkError: MinutesModelError = .contextTooLarge, available: SummarizerAvailability = .available) {
        self.inputBudget = inputBudget
        self.maxChunk = maxChunk
        self.maxOverview = maxOverview
        self.maxChunkError = maxChunkError
        self.available = available
    }

    func availability(language: String) async -> SummarizerAvailability { available }

    func inputBudget(for text: String) async -> Int { inputBudget }

    func extract(_ chunk: String, language: String) async throws -> ChunkMinutes {
        if chunk.count > maxChunk { throw maxChunkError }
        state.withLock { $0.chunks.append(chunk) }
        var minutes = ChunkMinutes()
        for line in chunk.components(separatedBy: "\n") {
            let text = line.replacing(/^\[\d\d:\d\d\] (\w+: )?/, with: "")
            minutes.keyPoints.append(text)
            if text.contains("decide") { minutes.decisions.append(text) }
            if text.contains("todo") { minutes.actionItems.append(ActionItem(task: text, owner: nil)) }
            if text.contains("?") { minutes.openQuestions.append(text) }
        }
        return minutes
    }

    func overview(of keyPoints: [String], language: String) async throws -> String {
        if keyPoints.joined(separator: "\n").count > maxOverview { throw MinutesModelError.contextTooLarge }
        state.withLock { $0.overviews.append(keyPoints) }
        return "overview of \(keyPoints.count)"
    }
}

/// Transcribes each track into the segments it is given for that file.
struct FakeTrackTranscriber: TrackTranscriber {
    var segmentsByFile: [String: [TranscriptSegment]]

    func resolveLocale(_ preferred: Locale?) async throws -> Locale { preferred ?? Locale(identifier: "es-ES") }

    func segments(of url: URL, locale: Locale, progress: @escaping @Sendable (Double) -> Void) async throws -> [TranscriptSegment] {
        progress(0.5)
        progress(1)
        return segmentsByFile[url.lastPathComponent] ?? []
    }
}

private func segment(_ start: Int, _ speaker: String?, _ text: String) -> TranscriptSegment {
    TranscriptSegment(startMs: start, endMs: start + 500, speaker: speaker, text: text)
}

// MARK: Chunking

struct TranscriptChunkerTests {
    @Test func joinsConsecutiveSegmentsOfOneSpeakerIntoATurn() {
        let turns = TranscriptChunker.turns([
            segment(0, "Me", "Hola."),
            segment(1000, "Me", "¿Empezamos?"),
            segment(65_000, "Others", "Sí."),
            segment(70_000, nil, "   "),
        ])
        #expect(turns == ["[00:00] Me: Hola. ¿Empezamos?", "[01:05] Others: Sí."])
    }

    @Test func cutsOnlyBetweenTurnsAndStaysWithinBudget() {
        let segments = (0..<40).map { index in
            segment(index * 1000, index.isMultiple(of: 2) ? "Me" : "Others", "Turn number \(index) with some words in it.")
        }
        let budget = 200
        let chunks = TranscriptChunker.chunks(segments, budget: budget)
        let turns = TranscriptChunker.turns(segments)
        #expect(chunks.count > 1)
        #expect(chunks.allSatisfy { $0.count <= budget })
        // Every turn is whole in exactly one chunk, in order.
        let lines = chunks.flatMap { $0.components(separatedBy: "\n") }
        #expect(lines == turns)
    }

    @Test func cutsATurnLongerThanTheBudgetBetweenSentencesKeepingEveryWord() {
        let long = (1...30).map { "Sentence \($0) says the budget is \($0 * 100) euros." }.joined(separator: " ")
        let chunks = TranscriptChunker.chunks([segment(0, "Me", long)], budget: 150)
        #expect(chunks.count > 1)
        #expect(chunks.allSatisfy { $0.count <= 150 })
        let words = chunks.joined(separator: " ").split(separator: " ").map(String.init)
        let expected = ("[00:00] Me: " + long).split(separator: " ").map(String.init)
        #expect(words == expected)
    }

    @Test func halvesAChunkAtALine() {
        #expect(TranscriptChunker.halves(of: "a\nb\nc\nd") == ["a\nb", "c\nd"])
        #expect(TranscriptChunker.halves(of: "one two three four") == ["one two", "three four"])
        // One word, as text without spaces is, between characters.
        #expect(TranscriptChunker.halves(of: "single") == ["sin", "gle"])
        #expect(TranscriptChunker.halves(of: "x") == ["x"])
    }
}

// MARK: Merging minutes

struct MinutesMergerTests {
    @Test func keepsEverythingFromEveryChunkInOrder() {
        let merged = MinutesMerger.merge([
            ChunkMinutes(keyPoints: ["A"], decisions: ["Ship on Friday"], actionItems: [ActionItem(task: "Write the release notes", owner: "Ana")], openQuestions: ["Who pays?"]),
            ChunkMinutes(keyPoints: ["B"], decisions: ["Hire two people"], actionItems: [ActionItem(task: "Book the room", owner: nil)], openQuestions: ["When?"]),
        ])
        #expect(merged.keyPoints == ["A", "B"])
        #expect(merged.decisions == ["Ship on Friday", "Hire two people"])
        #expect(merged.actionItems == [ActionItem(task: "Write the release notes", owner: "Ana"), ActionItem(task: "Book the room", owner: nil)])
        #expect(merged.openQuestions == ["Who pays?", "When?"])
    }

    @Test func mergesOnlyExactOrNearExactRepeats() {
        let decisions = MinutesMerger.dedupe([
            "Lanzar la versión el viernes 14.",
            "lanzar la version el viernes 14",
            "Lanzar la versión el vierne 14.",
            "Lanzar la versión el viernes 21.",
            "Contratar a dos personas",
        ])
        #expect(decisions == ["Lanzar la versión el viernes 14.", "Lanzar la versión el viernes 21.", "Contratar a dos personas"])
    }

    @Test func neverMergesDifferentNumbers() {
        #expect(!MinutesMerger.isNearDuplicate("The budget is 100000 euros for Q3", "The budget is 200000 euros for Q3"))
        #expect(MinutesMerger.isNearDuplicate("The budget is 100000 euros for Q3", "the budget is 100000 euros for Q3!"))
    }

    @Test func mergesRepeatedActionItemsAndKeepsTheOwner() {
        let merged = MinutesMerger.merge([
            ChunkMinutes(actionItems: [ActionItem(task: "Send the contract to Acme", owner: nil)]),
            ChunkMinutes(actionItems: [ActionItem(task: "send the contract to Acme.", owner: "Luis"), ActionItem(task: "Send the contract to Acme", owner: "Marta")]),
        ])
        #expect(merged.actionItems == [ActionItem(task: "Send the contract to Acme", owner: "Luis"), ActionItem(task: "Send the contract to Acme", owner: "Marta")])
    }
}

// MARK: Map-reduce

struct MapReduceSummarizerTests {
    private func transcript(turns: Int) -> Transcript {
        Transcript(locale: "es-ES", segments: (0..<turns).map { index in
            let text: String = switch index % 4 {
            case 0: "We decide item \(index)."
            case 1: "todo task \(index)"
            case 2: "What about \(index)?"
            default: "Plain point \(index)."
            }
            return segment(index * 1000, index.isMultiple(of: 2) ? "Me" : "Others", text)
        })
    }

    @Test func extractsEveryChunkAndKeepsEverythingItFound() async throws {
        let model = FakeMinutesModel(inputBudget: 120)
        let summary = try await MapReduceSummarizer(model: model).summarize(transcript(turns: 24)) { _ in }
        let chunks = model.state.withLock { $0.chunks }
        #expect(chunks.count > 1)
        #expect(summary.keyPoints.count == 24)
        #expect(summary.decisions.count == 6)
        #expect(summary.actionItems.count == 6)
        #expect(summary.openQuestions.count == 6)
        #expect(summary.model == "fake-model")
        #expect(summary.decisions.first == "We decide item 0.")
    }

    @Test func splitsAChunkTheModelCannotFit() async throws {
        let model = FakeMinutesModel(inputBudget: 400, maxChunk: 100)
        let summary = try await MapReduceSummarizer(model: model).summarize(transcript(turns: 12)) { _ in }
        #expect(model.state.withLock { $0.chunks }.allSatisfy { $0.count <= 100 })
        #expect(summary.keyPoints.count == 12)
    }

    @Test func writesTheOverviewInRoundsWhenTheKeyPointsDoNotFit() async throws {
        let model = FakeMinutesModel(inputBudget: 60)
        let summary = try await MapReduceSummarizer(model: model).summarize(transcript(turns: 20)) { _ in }
        let overviews = model.state.withLock { $0.overviews }
        #expect(overviews.count > 1)
        // The first rounds together read every key point.
        #expect(Set(overviews.dropLast().flatMap { $0 }).isSuperset(of: summary.keyPoints))
        #expect(summary.overview.hasPrefix("overview of"))
    }

    @Test func saysWhyWhenTheModelIsUnavailable() async {
        let model = FakeMinutesModel(inputBudget: 100, available: .unavailable(.modelNotReady))
        await #expect(throws: SummarizerError.unavailable(.modelNotReady)) {
            try await MapReduceSummarizer(model: model).summarize(transcript(turns: 2)) { _ in }
        }
    }

    @Test func refusesAnEmptyTranscript() async {
        await #expect(throws: SummarizerError.emptyTranscript) {
            try await MapReduceSummarizer(model: FakeMinutesModel(inputBudget: 100)).summarize(Transcript(locale: "en-US", segments: [])) { _ in }
        }
    }
}

// MARK: Transcripts

struct TranscriptMergeTests {
    @Test func labelsTheTracksAndMergesThemInTimeOrder() {
        let microphone = RecordedTrack(role: .microphone, url: URL(filePath: "/tmp/microphone.m4a"))
        let system = RecordedTrack(role: .system, url: URL(filePath: "/tmp/system.m4a"))
        let merged = TranscriptMerger.merge([
            (microphone, [segment(0, nil, "Hola"), segment(4000, nil, "Vale")]),
            (system, [segment(2000, nil, "Buenas"), segment(4000, nil, "Perfecto")]),
        ])
        #expect(merged.map(\.text) == ["Hola", "Buenas", "Vale", "Perfecto"])
        #expect(merged.map(\.speaker) == ["Me", "Others", "Me", "Others"])
    }

    @Test func leavesTheSpeakerOutInARoom() {
        let mixed = RecordedTrack(role: .mixed, url: URL(filePath: "/tmp/microphone.m4a"))
        let merged = TranscriptMerger.merge([(mixed, [segment(0, nil, "Todos")])])
        #expect(merged == [segment(0, nil, "Todos")])
    }

    @Test func transcribesEveryTrackWithProgressUpToOne() async throws {
        let transcriber = FakeTrackTranscriber(segmentsByFile: [
            "microphone.m4a": [segment(1000, nil, "Yo")],
            "system.m4a": [segment(0, nil, "Ellos")],
        ])
        let values = Mutex<[Double]>([])
        let transcript = try await transcriber.transcribe([
            RecordedTrack(role: .microphone, url: URL(filePath: "/tmp/microphone.m4a")),
            RecordedTrack(role: .system, url: URL(filePath: "/tmp/system.m4a")),
        ], locale: nil) { value in values.withLock { $0.append(value) } }
        #expect(transcript.locale == "es-ES")
        #expect(transcript.segments.map(\.speaker) == ["Others", "Me"])
        let progress = values.withLock { $0 }
        #expect(progress == progress.sorted())
        #expect(progress.last == 1)
    }

    @Test func picksTheBestSupportedLanguage() {
        let supported = ["en-US", "en-GB", "es-ES", "es-MX"].map(Locale.init(identifier:))
        #expect(TranscriptionLocale.bestMatch(preferred: ["es-ES", "en-US"], supported: supported)?.identifier(.bcp47) == "es-ES")
        #expect(TranscriptionLocale.bestMatch(preferred: ["es"], supported: supported)?.identifier(.bcp47) == "es-ES")
        #expect(TranscriptionLocale.bestMatch(preferred: ["es-419"], supported: supported)?.identifier(.bcp47) == "es-ES")
        #expect(TranscriptionLocale.bestMatch(preferred: ["fr-FR", "en-GB"], supported: supported)?.identifier(.bcp47) == "en-GB")
        #expect(TranscriptionLocale.bestMatch(preferred: ["fr-FR"], supported: supported) == nil)
    }

    @Test func encodesSegmentsAndMinutesInSnakeCase() throws {
        let segmentJSON = String(decoding: try JSONEncoder().encode(segment(0, "Me", "Hola")), as: UTF8.self)
        #expect(segmentJSON.contains("\"start_ms\":0"))
        let summary = MeetingSummary(model: "m", overview: "o", keyPoints: [], decisions: [], actionItems: [ActionItem(task: "t", owner: nil)], openQuestions: [])
        let summaryJSON = String(decoding: try JSONEncoder().encode(summary), as: UTF8.self)
        #expect(summaryJSON.contains("\"key_points\""))
        #expect(summaryJSON.contains("\"action_items\""))
        #expect(summaryJSON.contains("\"open_questions\""))
    }
}

// MARK: Controller

@MainActor
struct RecordingControllerTests {
    private func controller(
        recorder: FakeRecorder = FakeRecorder(),
        transcriber: RecordingTranscriber = RecordingTranscriber(),
        summarizer: RecordingSummarizer = RecordingSummarizer(),
        store: FakeRecordingsBackend,
        consent: MemoryRecordingConsentStore = MemoryRecordingConsentStore(),
        limits: RecordingLimits = RecordingLimits()
    ) -> RecordingController {
        let url = store.spaceURL
        let processor = RecordingsProcessor(store: store, transcriber: transcriber, summarizer: summarizer, spaceURLs: { [url] })
        return RecordingController(recorder: recorder, sink: StoreRecordingSink(store: store), processor: processor, consent: consent, unsaved: store.unsaved, limits: limits)
    }

    private func target(_ store: FakeRecordingsBackend) -> RecordingTarget {
        RecordingTarget(spaceURL: store.spaceURL, spaceName: "Work")
    }

    @Test func asksForConsentOnlyTheFirstTime() throws {
        let consent = MemoryRecordingConsentStore()
        let controller = controller(store: try FakeRecordingsBackend(), consent: consent)
        controller.request(.inPerson)
        #expect(controller.phase == .consentNeeded(.inPerson))
        controller.acknowledgeConsent()
        #expect(consent.hasAcknowledged)
        #expect(controller.phase == .choosingTarget(.inPerson))
        controller.cancelRequest()
        #expect(controller.phase == .idle)
        controller.request(.online(.allApps))
        #expect(controller.phase == .choosingTarget(.online(.allApps)))
    }

    @Test func neverStartsWithoutTheConsentAndATarget() async throws {
        let store = try FakeRecordingsBackend()
        let recorder = FakeRecorder()
        let controller = controller(recorder: recorder, store: store)
        await controller.start(target: target(store))
        controller.request(.inPerson)
        await controller.start(target: target(store))
        #expect(recorder.calls.isEmpty)
        #expect(controller.phase == .consentNeeded(.inPerson))
    }

    @Test func stopSavesBothTracksThenTheTranscriptAndTheMinutes() async throws {
        let store = try FakeRecordingsBackend()
        let recorder = FakeRecorder()
        let controller = controller(recorder: recorder, store: store, consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.online(.allApps))
        await controller.start(target: target(store))
        #expect(controller.isRecording)
        controller.pause()
        #expect(controller.isPaused)
        controller.resume()
        controller.stop()
        await controller.waitUntilProcessed()

        #expect(recorder.calls == ["start", "pause", "resume", "stop"])
        let saved = try #require(await store.created.first)
        #expect(saved.audio.map(\.role) == [.microphone, .system])
        let id = try #require(await store.recordings.keys.first)
        #expect(await store.transcript(of: id)?.transcript.segments.map(\.speaker) == ["Me", "Others"])
        #expect(await store.summary(of: id)?.summary.overview == "Resumen")
        #expect(controller.phase == .done(note: nil))
    }

    @Test func failsWhenTheMicrophoneIsRefused() async throws {
        let store = try FakeRecordingsBackend()
        let controller = controller(recorder: FakeRecorder(startError: .microphonePermissionDenied), store: store, consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.inPerson)
        await controller.start(target: target(store))
        #expect(controller.phase == .failed(RecordingError.microphonePermissionDenied.localizedDescription))
    }

    @Test func marksTheRecordingFailedWhenTranscriptionFails() async throws {
        let store = try FakeRecordingsBackend()
        let controller = controller(transcriber: RecordingTranscriber(error: .unsupportedLocale("xx")), store: store, consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.inPerson)
        await controller.start(target: target(store))
        controller.stop()
        await controller.waitUntilProcessed()
        let reason = TranscriptionError.unsupportedLocale("xx").localizedDescription
        #expect(await store.failed.map(\.error) == [reason])
        #expect(controller.phase == .failed(reason))
    }

    @Test func keepsTheTranscriptAndSaysWhyWhenMinutesCannotBeWritten() async throws {
        let store = try FakeRecordingsBackend()
        let controller = controller(summarizer: RecordingSummarizer(available: .unavailable(.appleIntelligenceNotEnabled)), store: store, consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.online(.allApps))
        await controller.start(target: target(store))
        controller.stop()
        await controller.waitUntilProcessed()
        let id = try #require(await store.recordings.keys.first)
        #expect(await store.transcript(of: id) != nil)
        #expect(await store.summaries.isEmpty)
        #expect(controller.phase == .done(note: SummarizerUnavailableReason.appleIntelligenceNotEnabled.message))
    }

    @Test func discardingThrowsTheRecordingAway() async throws {
        let store = try FakeRecordingsBackend()
        let recorder = FakeRecorder()
        let controller = controller(recorder: recorder, store: store, consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.inPerson)
        await controller.start(target: target(store))
        controller.discard()
        #expect(recorder.calls == ["start", "cancel"])
        #expect(await store.created.isEmpty)
        #expect(controller.phase == .idle)
    }

    @Test func formatsTheElapsedTime() {
        #expect(RecordingText.elapsed(65) == "1:05")
        #expect(RecordingText.elapsed(3725) == "1:02:05")
    }
}
