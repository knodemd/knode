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
    }

    let state = Mutex(State())
    let startError: RecordingError?

    init(startError: RecordingError? = nil) {
        self.startError = startError
    }

    var calls: [String] { state.withLock { $0.calls } }

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
        let folder = URL(filePath: "/tmp/knode-test-recording", directoryHint: .isDirectory)
        if mode == .inPerson { return [RecordedTrack(role: .mixed, url: folder.appending(path: "microphone.m4a"))] }
        return [
            RecordedTrack(role: .microphone, url: folder.appending(path: "microphone.m4a")),
            RecordedTrack(role: .system, url: folder.appending(path: "system.m4a")),
        ]
    }

    func cancel() { state.withLock { $0.calls.append("cancel") } }
    var elapsed: Duration { .seconds(65) }
    var level: Float { 0.5 }
    var isPaused: Bool { state.withLock { $0.paused } }
}

struct FakeTranscriber: Transcriber {
    var error: TranscriptionError?

    func transcribe(_ tracks: [RecordedTrack], locale: Locale?, progress: @escaping @Sendable (Double) -> Void) async throws -> Transcript {
        if let error { throw error }
        progress(1)
        return Transcript(locale: "es-ES", segments: tracks.enumerated().map { index, track in
            TranscriptSegment(startMs: index * 1000, endMs: index * 1000 + 900, speaker: track.speaker, text: "Hola \(index)")
        })
    }
}

struct FakeSummarizer: Summarizer {
    var available: SummarizerAvailability = .available

    func availability(for transcript: Transcript) async -> SummarizerAvailability { available }

    func summarize(_ transcript: Transcript, progress: @escaping @Sendable (Double) -> Void) async throws -> MeetingSummary {
        MeetingSummary(model: "fake", overview: "Resumen", keyPoints: ["Punto"], decisions: [], actionItems: [], openQuestions: [])
    }
}

/// A sink that keeps every call in memory.
final class FakeSink: RecordingSink {
    struct State {
        var saved: [FinishedRecording] = []
        var transcripts: [String: Transcript] = [:]
        var summaries: [String: MeetingSummary] = [:]
        var failures: [String: String] = [:]
    }

    let state = Mutex(State())

    func save(_ recording: FinishedRecording) async throws -> String {
        state.withLock { $0.saved.append(recording) }
        return "rec-1"
    }

    func saveTranscript(_ transcript: Transcript, for recording: String) async throws {
        state.withLock { $0.transcripts[recording] = transcript }
    }

    func saveSummary(_ summary: MeetingSummary, for recording: String) async throws {
        state.withLock { $0.summaries[recording] = summary }
    }

    func fail(_ recording: String, reason: String) async {
        state.withLock { $0.failures[recording] = reason }
    }
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
    var name: String { "fake-model" }
    let available: SummarizerAvailability

    init(inputBudget: Int, maxChunk: Int = .max, available: SummarizerAvailability = .available) {
        self.inputBudget = inputBudget
        self.maxChunk = maxChunk
        self.available = available
    }

    func availability(language: String) async -> SummarizerAvailability { available }

    func extract(_ chunk: String, language: String) async throws -> ChunkMinutes {
        if chunk.count > maxChunk { throw MinutesModelError.contextTooLarge }
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
        #expect(TranscriptChunker.halves(of: "single") == ["single"])
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
    let target = RecordingTarget(spaceURL: URL(filePath: "/tmp/Work.knode", directoryHint: .isDirectory), spaceName: "Work")

    private func controller(
        recorder: FakeRecorder = FakeRecorder(),
        transcriber: FakeTranscriber = FakeTranscriber(),
        summarizer: FakeSummarizer = FakeSummarizer(),
        sink: FakeSink = FakeSink(),
        consent: MemoryRecordingConsentStore = MemoryRecordingConsentStore()
    ) -> RecordingController {
        RecordingController(recorder: recorder, transcriber: transcriber, summarizer: summarizer, sink: sink, consent: consent)
    }

    @Test func asksForConsentOnlyTheFirstTime() {
        let consent = MemoryRecordingConsentStore()
        let controller = controller(consent: consent)
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

    @Test func neverStartsWithoutTheConsentAndATarget() async {
        let recorder = FakeRecorder()
        let controller = controller(recorder: recorder)
        await controller.start(target: target)
        controller.request(.inPerson)
        await controller.start(target: target)
        #expect(recorder.calls.isEmpty)
        #expect(controller.phase == .consentNeeded(.inPerson))
    }

    @Test func stopSavesBothTracksThenTheTranscriptAndTheMinutes() async throws {
        let recorder = FakeRecorder()
        let sink = FakeSink()
        let controller = controller(recorder: recorder, sink: sink, consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.online(.allApps))
        await controller.start(target: target)
        #expect(controller.isRecording)
        controller.pause()
        #expect(controller.isPaused)
        controller.resume()
        controller.stop()
        await controller.waitUntilProcessed()

        #expect(recorder.calls == ["start", "pause", "resume", "stop"])
        let state = sink.state.withLock { $0 }
        let saved = try #require(state.saved.first)
        #expect(saved.tracks.map(\.role) == [.microphone, .system])
        #expect(saved.target == target)
        #expect(saved.duration == .seconds(65))
        #expect(state.transcripts["rec-1"]?.segments.map(\.speaker) == ["Me", "Others"])
        #expect(state.summaries["rec-1"]?.overview == "Resumen")
        #expect(controller.phase == .done(note: nil))
    }

    @Test func recordsOnlyTheMicrophoneInPerson() async throws {
        let sink = FakeSink()
        let controller = controller(sink: sink, consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.inPerson)
        await controller.start(target: target)
        controller.stop()
        await controller.waitUntilProcessed()
        #expect(sink.state.withLock { $0.saved.first?.tracks.map(\.role) } == [.mixed])
    }

    @Test func failsWhenTheMicrophoneIsRefused() async {
        let controller = controller(recorder: FakeRecorder(startError: .microphonePermissionDenied), consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.inPerson)
        await controller.start(target: target)
        #expect(controller.phase == .failed(RecordingError.microphonePermissionDenied.localizedDescription))
    }

    @Test func marksTheRecordingFailedWhenTranscriptionFails() async {
        let sink = FakeSink()
        let controller = controller(transcriber: FakeTranscriber(error: .unsupportedLocale("xx")), sink: sink, consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.inPerson)
        await controller.start(target: target)
        controller.stop()
        await controller.waitUntilProcessed()
        let reason = TranscriptionError.unsupportedLocale("xx").localizedDescription
        #expect(sink.state.withLock { $0.failures["rec-1"] } == reason)
        #expect(controller.phase == .failed(reason))
    }

    @Test func keepsTheTranscriptAndSaysWhyWhenMinutesCannotBeWritten() async {
        let sink = FakeSink()
        let controller = controller(summarizer: FakeSummarizer(available: .unavailable(.appleIntelligenceNotEnabled)), sink: sink, consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.online(.allApps))
        await controller.start(target: target)
        controller.stop()
        await controller.waitUntilProcessed()
        let state = sink.state.withLock { $0 }
        #expect(state.transcripts["rec-1"] != nil)
        #expect(state.summaries.isEmpty)
        #expect(controller.phase == .done(note: SummarizerUnavailableReason.appleIntelligenceNotEnabled.message))
    }

    @Test func discardingThrowsTheRecordingAway() async {
        let recorder = FakeRecorder()
        let sink = FakeSink()
        let controller = controller(recorder: recorder, sink: sink, consent: MemoryRecordingConsentStore(hasAcknowledged: true))
        controller.request(.inPerson)
        await controller.start(target: target)
        controller.discard()
        #expect(recorder.calls == ["start", "cancel"])
        #expect(sink.state.withLock { $0.saved.isEmpty })
        #expect(controller.phase == .idle)
    }

    @Test func formatsTheElapsedTime() {
        #expect(RecordingText.elapsed(65) == "1:05")
        #expect(RecordingText.elapsed(3725) == "1:02:05")
    }
}
