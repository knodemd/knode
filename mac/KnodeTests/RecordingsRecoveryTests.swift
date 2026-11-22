import AVFoundation
import CoreAudio
import Foundation
import Synchronization
import Testing
@testable import Knode

// MARK: Fakes

/// Holds a save until it is opened.
actor SaveGate {
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if isOpen { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        isOpen = true
        for waiter in waiters { waiter.resume() }
        waiters = []
    }
}

/// A sink that waits at the gate, then saves through the store.
struct GatedSink: RecordingSink {
    let inner: StoreRecordingSink
    let gate: SaveGate

    func save(_ recording: FinishedRecording) async throws -> SavedRecording {
        await gate.wait()
        return try await inner.save(recording)
    }
}

private func temporaryFolder() throws -> URL {
    let folder = FileManager.default.temporaryDirectory.appending(path: "knode-recovery-\(UUID().uuidString)", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    return folder
}

private func exists(_ url: URL) -> Bool {
    FileManager.default.fileExists(atPath: url.path(percentEncoded: false))
}

private func finished(in folder: URL, target: RecordingTarget, mode: RecordingMode = .online(.allApps)) throws -> FinishedRecording {
    let tracks = [
        RecordedTrack(role: .microphone, url: folder.appending(path: "microphone.m4a")),
        RecordedTrack(role: .system, url: folder.appending(path: "system.m4a")),
    ]
    for track in tracks { try Data("audio".utf8).write(to: track.url) }
    return FinishedRecording(
        mode: mode, target: target, startedAt: Date(timeIntervalSince1970: 1_795_343_400),
        endedAt: Date(timeIntervalSince1970: 1_795_345_200), duration: .seconds(1800), tracks: tracks, key: "recorder:\(UUID().uuidString)"
    )
}

// MARK: Saving and unsaved recordings

@MainActor
struct UnsavedRecordingTests {
    let consent = MemoryRecordingConsentStore(hasAcknowledged: true)

    private func controller(
        _ store: FakeRecordingsBackend,
        recorder: FakeRecorder = FakeRecorder(),
        sink: (any RecordingSink)? = nil,
        limits: RecordingLimits = RecordingLimits()
    ) -> RecordingController {
        let url = store.spaceURL
        let processor = RecordingsProcessor(store: store, transcriber: RecordingTranscriber(), summarizer: RecordingSummarizer(), spaceURLs: { [url] })
        return RecordingController(
            recorder: recorder, sink: sink ?? StoreRecordingSink(store: store), processor: processor,
            consent: consent, unsaved: store.unsaved, limits: limits
        )
    }

    private func target(_ store: FakeRecordingsBackend) -> RecordingTarget {
        RecordingTarget(spaceURL: store.spaceURL, spaceName: "Work", projectID: "project-1", projectTitle: "Launch")
    }

    @Test func theSinkKeepsTheAudioWhenTheSpaceDidNotStoreEveryTrack() async throws {
        let store = try FakeRecordingsBackend()
        await store.setDropTracks([.system])
        let folder = try temporaryFolder()
        let recording = try finished(in: folder, target: target(store))
        await #expect(throws: RecordingError.notStored(files: ["system.m4a"], folder: folder.path(percentEncoded: false))) {
            try await StoreRecordingSink(store: store).save(recording)
        }
        #expect(recording.tracks.allSatisfy { exists($0.url) })
    }

    @Test func theSinkRemovesTheAudioOnceEveryTrackIsStored() async throws {
        let store = try FakeRecordingsBackend()
        let folder = try temporaryFolder()
        let recording = try finished(in: folder, target: target(store))
        let saved = try await StoreRecordingSink(store: store).save(recording)
        #expect(saved.recording.tracks.map(\.role) == [.microphone, .system])
        #expect(!exists(folder))
    }

    @Test func theManifestSaysWhereTheRecordingGoesAndWhatItRecorded() throws {
        let home = try temporaryFolder()
        let unsaved = UnsavedRecordingStore(home: KnodeHome(url: home))
        #expect(unsaved.root == home.appending(path: "Unsaved Recordings", directoryHint: .isDirectory))
        let folder = try temporaryFolder()
        let target = RecordingTarget(spaceURL: URL(filePath: "/tmp/Work.knode", directoryHint: .isDirectory), spaceName: "Work", projectID: "p1", projectTitle: "Launch")
        let recording = try finished(in: folder, target: target, mode: .online(.apps(bundleIDs: ["us.zoom.xos"])))

        let kept = try unsaved.keep(recording)
        #expect(kept.folder.deletingLastPathComponent() == unsaved.root)
        #expect(kept.recording.tracks.allSatisfy { exists($0.url) && $0.url.deletingLastPathComponent() == kept.folder })
        // The recorder's own folder is gone once empty.
        #expect(!exists(folder))
        let manifest = try JSONSerialization.jsonObject(with: Data(contentsOf: kept.folder.appending(path: "manifest.json"))) as? [String: Any]
        #expect(manifest?["space"] as? String == "/tmp/Work.knode/")
        #expect(manifest?["mode"] as? String == "online")
        #expect(manifest?["apps"] as? [String] == ["us.zoom.xos"])
        #expect(manifest?["duration_seconds"] as? Double == 1800)
        #expect((manifest?["tracks"] as? [[String: String]])?.map { $0["file"] } == ["microphone.m4a", "system.m4a"])

        let listed = try #require(unsaved.list().first)
        #expect(listed.recording == kept.recording)
        unsaved.setError("No space", of: kept.id)
        #expect(unsaved.list().first?.error == "No space")
        try unsaved.delete(kept.id)
        #expect(unsaved.list().isEmpty)
        #expect(!exists(kept.folder))
    }

    @Test func aFailedSaveKeepsTheRecordingToSaveAgain() async throws {
        let store = try FakeRecordingsBackend()
        await store.setDropTracks([.system])
        let recorder = FakeRecorder()
        let controller = controller(store, recorder: recorder)
        controller.request(.online(.allApps))
        await controller.start(target: target(store))
        controller.stop()
        await controller.waitUntilProcessed()

        guard case .failed(let message) = controller.phase else {
            Issue.record("expected a failure, got \(controller.phase)")
            return
        }
        #expect(message.contains("system.m4a"))
        let item = try #require(controller.unsaved.first)
        #expect(item.recording.target.projectID == "project-1")
        #expect(item.recording.tracks.allSatisfy { exists($0.url) })
        // Moved out of the recorder's temporary folder at once.
        #expect(!exists(try #require(recorder.folder)))

        await store.setDropTracks([])
        #expect(await controller.saveAgain(item) == nil)
        #expect(controller.unsaved.isEmpty)
        #expect(!exists(item.folder))
        #expect(await store.created.count == 2)
        #expect(await store.created.map(\.key) == [item.recording.key, item.recording.key])
        // Once saved, it is transcribed like any other.
        let id = try #require(await store.recordings.values.first { $0.tracks.count == 2 }?.id)
        #expect(try await eventually { await store.recordings[id]?.summary == .done })
    }

    @Test func recordingsLeftUnsavedAreSavedAtLaunch() async throws {
        let store = try FakeRecordingsBackend()
        let kept = try store.unsaved.keep(try finished(in: try temporaryFolder(), target: target(store)))
        let controller = controller(store)
        await controller.saveUnsaved()
        #expect(await store.created.map(\.key) == [kept.recording.key])
        #expect(await store.created.first?.project == "project-1")
        #expect(controller.unsaved.isEmpty)
        #expect(!exists(kept.folder))
    }

    @Test func deletingAnUnsavedRecordingRemovesItsAudio() async throws {
        let store = try FakeRecordingsBackend()
        let kept = try store.unsaved.keep(try finished(in: try temporaryFolder(), target: target(store)))
        let controller = controller(store)
        controller.refreshUnsaved()
        try controller.deleteUnsaved(try #require(controller.unsaved.first))
        #expect(controller.unsaved.isEmpty)
        #expect(!exists(kept.folder))
    }

    @Test func savingCannotBeCancelled() async throws {
        let store = try FakeRecordingsBackend()
        let gate = SaveGate()
        let controller = controller(store, sink: GatedSink(inner: StoreRecordingSink(store: store), gate: gate))
        controller.request(.inPerson)
        await controller.start(target: target(store))
        controller.stop()
        #expect(controller.isSaving)
        #expect(!controller.canCancelProcessing)
        controller.cancelProcessing()
        await gate.open()
        await controller.waitUntilProcessed()
        #expect(controller.phase == .done(note: nil))
        #expect(await store.created.count == 1)
    }

    @Test func quittingWhileRecordingStopsAndSavesFirst() async throws {
        let store = try FakeRecordingsBackend()
        let recorder = FakeRecorder()
        let controller = controller(store, recorder: recorder)
        controller.request(.online(.allApps))
        await controller.start(target: target(store))
        #expect(controller.shouldDelayTermination)
        await controller.prepareForTermination(timeout: .seconds(5))
        #expect(recorder.calls.contains("stop"))
        #expect(!controller.isRecording)
        #expect(!controller.isSaving)
        #expect(await store.created.count == 1)
        #expect(controller.unsaved.isEmpty)
    }

    @Test func quittingKeepsWhatCouldNotBeSavedInTime() async throws {
        let store = try FakeRecordingsBackend()
        let gate = SaveGate()
        let controller = controller(store, sink: GatedSink(inner: StoreRecordingSink(store: store), gate: gate))
        controller.request(.inPerson)
        await controller.start(target: target(store))
        await controller.prepareForTermination(timeout: .milliseconds(200))
        // Still saving when the time ran out: the files wait among the unsaved recordings.
        #expect(controller.isSaving)
        #expect(store.unsaved.list().count == 1)
        await gate.open()
        await controller.waitUntilProcessed()
    }

    @Test func warnsNearTheLimitThenStopsSavesAndOffersANewRecording() async throws {
        let store = try FakeRecordingsBackend()
        let recorder = FakeRecorder()
        let controller = controller(store, recorder: recorder, limits: RecordingLimits(warnAfter: .seconds(60), stopAfter: .seconds(120)))
        controller.request(.online(.allApps))
        await controller.start(target: target(store))
        #expect(try await eventually { controller.isNearLimit })
        #expect(controller.isRecording)
        #expect(controller.minutesToLimit == 1)

        recorder.setElapsed(.seconds(121))
        #expect(try await eventually { controller.phase == .done(note: RecordingController.limitReachedNote) })
        #expect(await store.created.count == 1)
        #expect(controller.continuation == .init(mode: .online(.allApps), target: target(store)))

        recorder.setElapsed(.seconds(1))
        await controller.recordAgain()
        #expect(controller.phase == .recording(.online(.allApps), target(store)))
        #expect(!controller.isNearLimit)
        #expect(controller.continuation == nil)
        controller.discard()
    }

    @Test func theDefaultLimitStopsBeforeATrackOutgrowsAnAttachment() {
        let limits = RecordingLimits()
        let bytes = Double(limits.stopAfter.components.seconds) * Double(TrackWriter.bitRate) / 8
        #expect(bytes < Double(AttachmentInfo.maxSize))
        #expect(limits.warnAfter == .seconds(3 * 3600))
        #expect(limits.warnAfter < limits.stopAfter)
    }

    @Test func saysWhenTheMicrophoneChanges() async throws {
        let store = try FakeRecordingsBackend()
        let recorder = FakeRecorder()
        let controller = controller(store, recorder: recorder)
        controller.request(.inPerson)
        await controller.start(target: target(store))
        #expect(controller.notice == nil)
        recorder.changeInput()
        #expect(try await eventually { controller.notice == RecordingController.inputChangedNotice })
        #expect(controller.isRecording)
        controller.discard()
    }
}

// MARK: Failures kept by the space

@MainActor
struct StoredFailureTests {
    @Test func failuresFromBeforeARestartCanBeRetried() async throws {
        let store = try FakeRecordingsBackend()
        let id = try await store.add(kind: .voiceMemo, tracks: [("Memo.m4a", .mixed)])
        await store.markFailed(id, stage: .transcript, error: "No speech model")
        let url = store.spaceURL
        let processor = RecordingsProcessor(store: store, transcriber: RecordingTranscriber(), summarizer: RecordingSummarizer(), spaceURLs: { [url] })
        processor.start()
        #expect(try await eventually { processor.failures.count == 1 })
        let failure = try #require(processor.failures.first)
        #expect(failure.recording == id)
        #expect(failure.stage == .transcript)
        #expect(failure.message == "No speech model")

        try await processor.retry(failure)
        #expect(try await eventually { await store.recordings[id]?.transcript == .done })
        #expect(processor.failures.isEmpty)
        await processor.stop()
    }

    @Test func failedMinutesAreRetriedAsMinutes() async throws {
        let store = try FakeRecordingsBackend()
        let id = try await store.add(kind: .meeting, tracks: [("microphone.m4a", .mixed)])
        await store.markFailed(id, stage: .summary, error: "Too long")
        let url = store.spaceURL
        let processor = RecordingsProcessor(store: store, transcriber: RecordingTranscriber(), summarizer: RecordingSummarizer(), spaceURLs: { [url] })
        processor.start()
        #expect(try await eventually { processor.failures.map(\.stage) == [.summary] })
        await processor.stop()
    }

    @Test func failuresOfThisRunStayWhenTheSpaceCannotListThem() async throws {
        let store = try FakeRecordingsBackend()
        await store.setFailedUnsupported(true)
        _ = try await store.add(kind: .voiceMemo, tracks: [("Memo.m4a", .mixed)])
        let url = store.spaceURL
        let processor = RecordingsProcessor(store: store, transcriber: RecordingTranscriber(error: .unavailable), summarizer: RecordingSummarizer(), spaceURLs: { [url] })
        processor.start()
        #expect(try await eventually { processor.failures.count == 1 })
        processor.scanAll()
        try await Task.sleep(for: .milliseconds(100))
        #expect(processor.failures.count == 1)
        await processor.stop()
    }

    @Test func unavailableMinutesAreNotAFailureToRetry() {
        let recording = StoredRecording(
            id: "r", kind: .meeting, title: "T", startedAt: .now, endedAt: .now, language: nil, transcript: .done,
            summary: .unavailable, error: "Off", project: nil, tracks: [], transcriptID: nil
        )
        #expect(RecordingsProcessor.Failure(recording, in: "s", at: URL(filePath: "/tmp/s")) == nil)
    }
}

// MARK: Minutes

struct SummarizerRobustnessTests {
    private func transcript(_ texts: [String], locale: String = "es-ES") -> Transcript {
        Transcript(locale: locale, segments: texts.enumerated().map { index, text in
            TranscriptSegment(startMs: index * 1000, endMs: index * 1000 + 500, speaker: index.isMultiple(of: 2) ? "Me" : "Others", text: text)
        })
    }

    @Test func halvesTheOverviewWhenItDoesNotFit() async throws {
        let points = (0..<16).map { "Point number \($0) of the meeting." }
        let model = FakeMinutesModel(inputBudget: 10_000, maxOverview: 120)
        let summary = try await MapReduceSummarizer(model: model).summarize(transcript(points)) { _ in }
        let overviews = model.state.withLock { $0.overviews }
        #expect(overviews.count > 2)
        #expect(overviews.allSatisfy { $0.joined(separator: "\n").count <= 120 })
        #expect(Set(overviews.flatMap { $0 }).isSuperset(of: summary.keyPoints))
        #expect(summary.overview.hasPrefix("overview of"))
    }

    @Test func splitsAChunkWhoseAnswerWasCutOff() async throws {
        let model = FakeMinutesModel(inputBudget: 400, maxChunk: 100, maxChunkError: .answerTooLong)
        let texts = (0..<12).map { "Plain point \($0)." }
        let summary = try await MapReduceSummarizer(model: model).summarize(transcript(texts)) { _ in }
        #expect(model.state.withLock { $0.chunks }.allSatisfy { $0.count <= 100 })
        #expect(summary.keyPoints.count == 12)
    }

    @Test func cutsTextWithoutSpacesBetweenCharacters() {
        let japanese = String(repeating: "会議で来週の金曜日に公開することを決めました", count: 6)
        let pieces = TranscriptChunker.split(japanese, budget: 40)
        #expect(pieces.count > 1)
        #expect(pieces.allSatisfy { $0.count <= 40 })
        #expect(pieces.joined() == japanese)
        let halves = TranscriptChunker.halves(of: japanese)
        #expect(halves.count == 2)
        #expect(halves.joined() == japanese)
    }

    @Test func countsATokenACharacterForChineseAndJapanese() {
        #expect(TokenEstimate.charactersPerToken("我们决定下周五发布新版本") == 1)
        #expect(abs(TokenEstimate.charactersPerToken("We decided to ship on Friday") - 2.5) < 0.001)
        let mixed = TokenEstimate.charactersPerToken("Decidimos 来週 lanzar")
        #expect(mixed > 1 && mixed < 2.5)
    }
}

// MARK: Audio

struct RecordingAudioFormatTests {
    private func tone(rate: Double, channels: AVAudioChannelCount, seconds: Double) -> AVAudioPCMBuffer {
        let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: channels, interleaved: false)!
        let frames = AVAudioFrameCount(rate * seconds)
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames)!
        buffer.frameLength = frames
        for channel in 0..<Int(channels) {
            for frame in 0..<Int(frames) {
                buffer.floatChannelData![channel][frame] = 0.3 * sin(Float(frame) * 2 * .pi * 440 / Float(rate))
            }
        }
        return buffer
    }

    private func write(_ buffers: [AVAudioPCMBuffer], to url: URL) throws {
        let writer = try TrackWriter(role: .microphone, url: url)
        for buffer in buffers { writer.append(buffer) }
        try writer.finish()
    }

    @Test(arguments: [8_000.0, 16_000, 44_100, 96_000, 192_000])
    func everyInputRateEndsUpAt48kHzMono(rate: Double) throws {
        let url = try temporaryFolder().appending(path: "microphone.m4a")
        try write((0..<4).map { _ in tone(rate: rate, channels: 2, seconds: 0.5) }, to: url)
        let file = try AVAudioFile(forReading: url)
        #expect(file.fileFormat.sampleRate == 48_000)
        #expect(file.fileFormat.channelCount == 1)
        let seconds = Double(file.length) / file.processingFormat.sampleRate
        #expect(abs(seconds - 2) < 0.1, "\(rate) Hz gave \(seconds) s")
    }

    @Test func aMicrophoneChangeHalfwayKeepsTheSameFile() throws {
        let url = try temporaryFolder().appending(path: "microphone.m4a")
        // The built-in microphone, then AirPods at 16 kHz, then back.
        try write([
            tone(rate: 48_000, channels: 1, seconds: 1), tone(rate: 16_000, channels: 1, seconds: 1), tone(rate: 44_100, channels: 2, seconds: 1),
        ], to: url)
        let file = try AVAudioFile(forReading: url)
        let seconds = Double(file.length) / file.processingFormat.sampleRate
        #expect(abs(seconds - 3) < 0.15, "gave \(seconds) s")
    }

    @Test func theTapIsTheLastBuffersOfTheList() {
        #expect(TapBufferSelector.tapRange(bufferCount: 2, tapBufferCount: 2) == 0..<2)
        // A headset's microphone first, then the tap's two channels.
        #expect(TapBufferSelector.tapRange(bufferCount: 3, tapBufferCount: 2) == 1..<3)
        #expect(TapBufferSelector.tapRange(bufferCount: 1, tapBufferCount: 2) == nil)
    }

    @Test func theAggregateHoldsTheTapAloneUnlessItNeedsAClock() {
        let tap = UUID()
        let alone = SystemAudioTap.composition(tap: tap, clock: nil)
        #expect(alone[kAudioAggregateDeviceSubDeviceListKey] == nil)
        #expect(alone[kAudioAggregateDeviceMainSubDeviceKey] == nil)
        #expect((alone[kAudioAggregateDeviceTapListKey] as? [[String: Any]])?.first?[kAudioSubTapUIDKey] as? String == tap.uuidString)
        let clocked = SystemAudioTap.composition(tap: tap, clock: "speakers")
        #expect(clocked[kAudioAggregateDeviceMainSubDeviceKey] as? String == "speakers")
    }

    @Test func aQuickTimeVoiceMemoIsDecodedForTheTranscriber() async throws {
        let folder = try temporaryFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let m4a = folder.appending(path: "memo.m4a")
        try write([tone(rate: 48_000, channels: 1, seconds: 1)], to: m4a)
        // The same audio in a QuickTime container, as Voice Memos keeps it.
        let mov = folder.appending(path: "memo.mov")
        let export = try #require(AVAssetExportSession(asset: AVURLAsset(url: m4a), presetName: AVAssetExportPresetPassthrough))
        try await export.export(to: mov, as: .mov)
        let qta = folder.appending(path: "Memo.qta")
        try FileManager.default.moveItem(at: mov, to: qta)

        let scratch = folder.appending(path: "scratch", directoryHint: .isDirectory)
        let decoded = try await AudioReading.decode(qta, into: scratch)
        let file = try AVAudioFile(forReading: decoded)
        #expect(abs(Double(file.length) / file.processingFormat.sampleRate - 1) < 0.1)

        let opened = try await AudioReading.open(qta, scratch: scratch)
        #expect(abs(Double(opened.file.length) / opened.file.processingFormat.sampleRate - 1) < 0.1)
        opened.removeTemporary()
        if let temporary = opened.temporary { #expect(!exists(temporary)) }
    }

    @Test func audioNoReaderCanOpenFailsClearly() async throws {
        let folder = try temporaryFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let junk = folder.appending(path: "junk.qta")
        try Data("not audio".utf8).write(to: junk)
        await #expect(throws: TranscriptionError.self) {
            _ = try await AudioReading.open(junk, scratch: folder.appending(path: "scratch"))
        }
        // Nothing decoded is left behind.
        let left = (try? FileManager.default.contentsOfDirectory(atPath: folder.appending(path: "scratch").path(percentEncoded: false))) ?? []
        #expect(left.isEmpty)
    }
}
