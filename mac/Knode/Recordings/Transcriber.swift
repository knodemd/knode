import AVFAudio
import CoreMedia
import Foundation
import Speech

/// Why a recording could not be transcribed.
enum TranscriptionError: Error, LocalizedError, Equatable {
    case unavailable
    case unsupportedLocale(String)
    case assetsUnavailable(String)
    case unreadableAudio(String)

    var errorDescription: String? {
        switch self {
        case .unavailable:
            "Speech recognition on the device is not available on this Mac."
        case .unsupportedLocale(let locale):
            "Transcription on the device does not support \(locale)."
        case .assetsUnavailable(let reason):
            "The speech model for this language could not be installed: \(reason)."
        case .unreadableAudio(let reason):
            "The audio could not be read: \(reason)."
        }
    }
}

/// Turns the tracks of a recording into one transcript, on the device.
protocol Transcriber: Sendable {
    /// Transcribes every track and merges them in time order. `progress` goes from 0 to 1 over all
    /// tracks. Cancelling the task stops it with `CancellationError`.
    func transcribe(
        _ tracks: [RecordedTrack],
        locale: Locale?,
        progress: @escaping @Sendable (Double) -> Void
    ) async throws -> Transcript
}

/// Transcribes each track on its own and merges the segments, labelled by track.
protocol TrackTranscriber: Sendable {
    /// The locale to use: `preferred` when supported, else the best match of the user's languages.
    func resolveLocale(_ preferred: Locale?) async throws -> Locale
    /// The segments of one file, without speakers, in time order.
    func segments(of url: URL, locale: Locale, progress: @escaping @Sendable (Double) -> Void) async throws -> [TranscriptSegment]
}

extension TrackTranscriber {
    func transcribe(
        _ tracks: [RecordedTrack],
        locale: Locale?,
        progress: @escaping @Sendable (Double) -> Void
    ) async throws -> Transcript {
        let resolved = try await resolveLocale(locale)
        var perTrack: [(RecordedTrack, [TranscriptSegment])] = []
        for (index, track) in tracks.enumerated() {
            try Task.checkCancellation()
            let share = 1 / Double(max(tracks.count, 1))
            let segments = try await segments(of: track.url, locale: resolved) { value in
                progress((Double(index) + min(max(value, 0), 1)) * share)
            }
            perTrack.append((track, segments))
        }
        progress(1)
        return Transcript(locale: resolved.identifier(.bcp47), segments: TranscriptMerger.merge(perTrack))
    }
}

/// Merging the segments of several tracks into one transcript.
enum TranscriptMerger {
    /// Gives each track's segments its speaker (`Me`, `Others`, or none in a room) and sorts them all
    /// by start time; on a tie the microphone comes first. Nothing is dropped.
    static func merge(_ tracks: [(RecordedTrack, [TranscriptSegment])]) -> [TranscriptSegment] {
        let order: [RecordedTrack.Role: Int] = [.microphone: 0, .mixed: 0, .system: 1]
        var labelled: [(segment: TranscriptSegment, order: Int, index: Int)] = []
        for (track, segments) in tracks {
            for (index, segment) in segments.enumerated() {
                var segment = segment
                segment.speaker = track.speaker ?? segment.speaker
                labelled.append((segment, order[track.role] ?? 0, index))
            }
        }
        return labelled.sorted { lhs, rhs in
            if lhs.segment.startMs != rhs.segment.startMs { return lhs.segment.startMs < rhs.segment.startMs }
            if lhs.order != rhs.order { return lhs.order < rhs.order }
            return lhs.index < rhs.index
        }
        .map(\.segment)
    }
}

/// Choosing the transcription language.
enum TranscriptionLocale {
    /// The first of the user's preferred languages that is supported, matched by language and
    /// region, then by language alone; `nil` when none is.
    static func bestMatch(preferred: [String], supported: [Locale]) -> Locale? {
        for identifier in preferred {
            let wanted = Locale(identifier: identifier)
            if let exact = supported.first(where: { $0.identifier(.bcp47) == wanted.identifier(.bcp47) }) { return exact }
            guard let language = wanted.language.languageCode?.identifier else { continue }
            let sameLanguage = supported.filter { $0.language.languageCode?.identifier == language }
            if let region = wanted.region?.identifier,
               let sameRegion = sameLanguage.first(where: { $0.region?.identifier == region }) {
                return sameRegion
            }
            if let first = sameLanguage.sorted(by: { $0.identifier(.bcp47) < $1.identifier(.bcp47) }).first {
                return first
            }
        }
        return nil
    }
}

/// Transcription with `SpeechAnalyzer` and `SpeechTranscriber` (macOS 26), which runs on the
/// device and installs the language's model through `AssetInventory` the first time.
struct SpeechAnalyzerTranscriber: Transcriber, TrackTranscriber {
    var preferredLanguages: @Sendable () -> [String] = { Locale.preferredLanguages }

    func resolveLocale(_ preferred: Locale?) async throws -> Locale {
        guard SpeechTranscriber.isAvailable else { throw TranscriptionError.unavailable }
        if let preferred {
            guard let supported = await SpeechTranscriber.supportedLocale(equivalentTo: preferred) else {
                throw TranscriptionError.unsupportedLocale(preferred.identifier(.bcp47))
            }
            return supported
        }
        let supported = await SpeechTranscriber.supportedLocales
        if let match = TranscriptionLocale.bestMatch(preferred: preferredLanguages(), supported: supported) {
            return match
        }
        if let english = supported.first(where: { $0.identifier(.bcp47) == "en-US" }) { return english }
        throw TranscriptionError.unsupportedLocale(preferredLanguages().first ?? Locale.current.identifier(.bcp47))
    }

    func segments(of url: URL, locale: Locale, progress: @escaping @Sendable (Double) -> Void) async throws -> [TranscriptSegment] {
        let transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [], attributeOptions: [.audioTimeRange])
        try await Self.installAssets(for: transcriber)

        let file: AVAudioFile
        do {
            file = try AVAudioFile(forReading: url)
        } catch {
            throw TranscriptionError.unreadableAudio(error.localizedDescription)
        }
        let total = Double(file.length) / file.processingFormat.sampleRate
        guard total > 0 else { return [] }

        let analyzer = SpeechAnalyzer(modules: [transcriber])
        let collector = Task {
            var segments: [TranscriptSegment] = []
            for try await result in transcriber.results {
                let sentences = Self.sentences(of: result)
                segments += sentences
                if let end = sentences.last?.endMs { progress(min(Double(end) / 1000 / total, 0.99)) }
            }
            return segments
        }
        do {
            try await withTaskCancellationHandler {
                try await analyzer.start(inputAudioFile: file, finishAfterFile: true)
                _ = try await collector.value
            } onCancel: {
                collector.cancel()
                Task { await analyzer.cancelAndFinishNow() }
            }
        } catch {
            collector.cancel()
            await analyzer.cancelAndFinishNow()
            throw Task.isCancelled ? CancellationError() : error
        }
        try Task.checkCancellation()
        progress(1)
        return try await collector.value.sorted { $0.startMs < $1.startMs }
    }

    /// A result cut into sentences, each timed by its words' audio time ranges; a sentence whose
    /// words carry none takes the result's own range.
    private static func sentences(of result: SpeechTranscriber.Result) -> [TranscriptSegment] {
        var segments: [TranscriptSegment] = []
        var text = ""
        var start: CMTime?
        var end: CMTime?
        func flush() {
            let sentence = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if !sentence.isEmpty {
                let from = milliseconds(start ?? result.range.start)
                let to = milliseconds(end ?? result.range.end)
                segments.append(TranscriptSegment(startMs: from, endMs: max(to, from), speaker: nil, text: sentence))
            }
            text = ""
            start = nil
            end = nil
        }
        for run in result.text.runs {
            let piece = String(result.text[run.range].characters)
            text += piece
            if let range = run.audioTimeRange {
                if start.map({ range.start < $0 }) ?? true { start = range.start }
                if end.map({ range.end > $0 }) ?? true { end = range.end }
            }
            if let last = piece.trimmingCharacters(in: .whitespacesAndNewlines).last, ".?!…".contains(last) {
                flush()
            }
        }
        flush()
        return segments
    }

    private static func milliseconds(_ time: CMTime) -> Int {
        guard time.isNumeric else { return 0 }
        return Int((time.seconds * 1000).rounded())
    }

    private static func installAssets(for transcriber: SpeechTranscriber) async throws {
        do {
            if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                try await request.downloadAndInstall()
            }
        } catch {
            throw TranscriptionError.assetsUnavailable(error.localizedDescription)
        }
    }
}
