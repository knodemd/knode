import AVFoundation
import CoreMedia
import Foundation
import NaturalLanguage
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
    /// The supported locales of the user's languages, one per language, in the user's order: the
    /// languages a recording without a language may be in.
    func candidateLocales() async throws -> [Locale]
    /// A short clip of the file to hear which language is spoken, removed by the caller; `nil` when
    /// none can be cut.
    func languageSample(of url: URL) async throws -> URL?
}

extension TrackTranscriber {
    func transcribe(
        _ tracks: [RecordedTrack],
        locale: Locale?,
        progress: @escaping @Sendable (Double) -> Void
    ) async throws -> Transcript {
        let resolved = if let locale { try await resolveLocale(locale) } else { try await detectLocale(tracks) }
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

    func candidateLocales() async throws -> [Locale] { [try await resolveLocale(nil)] }

    func languageSample(of url: URL) async throws -> URL? { nil }

    /// The language spoken, for a recording that has none: with one of the user's languages that
    /// one; with several, a clip of the microphone (or the first track) is transcribed in each and
    /// the one whose words read most like that language wins. A person whose Mac is in English and
    /// who speaks Spanish gets a Spanish transcript.
    func detectLocale(_ tracks: [RecordedTrack]) async throws -> Locale {
        let candidates = try await candidateLocales()
        guard candidates.count > 1 else {
            if let only = candidates.first { return only }
            return try await resolveLocale(nil)
        }
        guard let track = tracks.first(where: { $0.role == .microphone }) ?? tracks.first,
              let clip = try await languageSample(of: track.url)
        else { return candidates[0] }
        defer { try? FileManager.default.removeItem(at: clip) }
        var heard: [(Locale, String)] = []
        for candidate in candidates {
            try Task.checkCancellation()
            let segments = try await segments(of: clip, locale: candidate) { _ in }
            heard.append((candidate, segments.map(\.text).joined(separator: " ")))
        }
        return TranscriptionLocale.mostLikely(heard) ?? candidates[0]
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
            // The language's usual region (en-US for English, es-ES for Spanish), else the first.
            let usual = Locale(identifier: Locale.Language(identifier: language).maximalIdentifier).region?.identifier
            if let usual, let match = sameLanguage.first(where: { $0.region?.identifier == usual }) { return match }
            if let first = sameLanguage.sorted(by: { $0.identifier(.bcp47) < $1.identifier(.bcp47) }).first {
                return first
            }
        }
        return nil
    }

    /// Of the user's languages, one supported locale per language, in the user's order.
    static func candidates(preferred: [String], supported: [Locale]) -> [Locale] {
        var seen: Set<String> = []
        return preferred.compactMap { identifier in
            guard let match = bestMatch(preferred: [identifier], supported: supported),
                  let language = match.language.languageCode?.identifier,
                  seen.insert(language).inserted
            else { return nil }
            return match
        }
    }

    /// Of the same clip transcribed in each locale, the locale whose text reads most like its own
    /// language; `nil` when no text says anything.
    static func mostLikely(_ heard: [(Locale, String)]) -> Locale? {
        var best: (locale: Locale, score: Double)?
        for (locale, text) in heard {
            let words = text.split(whereSeparator: { $0.isWhitespace || $0.isPunctuation }).count
            guard words > 0, let language = locale.language.languageCode?.identifier else { continue }
            let recognizer = NLLanguageRecognizer()
            recognizer.processString(text)
            let probability = recognizer.languageHypotheses(withMaximum: 5)[NLLanguage(rawValue: language)] ?? 0
            // A wrong model hears few, short words; the right one more of them.
            let score = probability * log(Double(words) + 1)
            if score > (best?.score ?? 0) { best = (locale, score) }
        }
        return best?.locale
    }
}

/// Transcription with `SpeechAnalyzer` and `SpeechTranscriber` (macOS 26), which runs on the
/// device and installs the language's model through `AssetInventory` the first time.
struct SpeechAnalyzerTranscriber: Transcriber, TrackTranscriber {
    var preferredLanguages: @Sendable () -> [String] = { Locale.preferredLanguages }
    /// Where audio the speech analyzer cannot read as it is gets decoded first.
    var scratch: URL = FileManager.default.temporaryDirectory.appending(path: "Knode Decoding", directoryHint: .isDirectory)

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

    func candidateLocales() async throws -> [Locale] {
        guard SpeechTranscriber.isAvailable else { throw TranscriptionError.unavailable }
        let found = TranscriptionLocale.candidates(preferred: preferredLanguages(), supported: await SpeechTranscriber.supportedLocales)
        return found.isEmpty ? [try await resolveLocale(nil)] : Array(found.prefix(3))
    }

    /// Ninety seconds from the middle of the file, where people are most likely talking.
    func languageSample(of url: URL) async throws -> URL? {
        let opened = try await AudioReading.open(url, scratch: scratch)
        defer { opened.removeTemporary() }
        let file = opened.file
        let rate = file.processingFormat.sampleRate
        let length = AVAudioFramePosition(90 * rate)
        guard file.length > 0, rate > 0 else { return nil }
        let start = max(0, file.length / 2 - length / 2)
        let count = AVAudioFrameCount(min(length, file.length - start))
        guard let buffer = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: count) else { return nil }
        file.framePosition = start
        try file.read(into: buffer, frameCount: count)
        try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let clip = scratch.appending(path: "\(UUID().uuidString)-sample.caf")
        do {
            let output = try AVAudioFile(forWriting: clip, settings: file.processingFormat.settings, commonFormat: file.processingFormat.commonFormat, interleaved: file.processingFormat.isInterleaved)
            try output.write(from: buffer)
        } catch {
            try? FileManager.default.removeItem(at: clip)
            throw TranscriptionError.unreadableAudio(error.localizedDescription)
        }
        return clip
    }

    func segments(of url: URL, locale: Locale, progress: @escaping @Sendable (Double) -> Void) async throws -> [TranscriptSegment] {
        let transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [], attributeOptions: [.audioTimeRange])
        try await Self.installAssets(for: transcriber)

        let opened = try await AudioReading.open(url, scratch: scratch)
        defer { opened.removeTemporary() }
        let file = opened.file
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

/// Opening a recording's audio for the speech analyzer, which reads an `AVAudioFile`.
enum AudioReading {
    struct Opened {
        var file: AVAudioFile
        /// The decoded copy it reads, when the original could not be read as it is.
        var temporary: URL?

        func removeTemporary() {
            if let temporary { try? FileManager.default.removeItem(at: temporary) }
        }
    }

    /// The file as it is; or, when `AVAudioFile` cannot open it (a QuickTime `.qta` voice memo,
    /// say), a copy decoded through `AVAssetReader` into `scratch`, which the caller removes
    /// with `removeTemporary()`.
    static func open(_ url: URL, scratch: URL) async throws -> Opened {
        if let file = try? AVAudioFile(forReading: url) { return Opened(file: file) }
        let decoded = try await decode(url, into: scratch)
        do {
            return Opened(file: try AVAudioFile(forReading: decoded), temporary: decoded)
        } catch {
            try? FileManager.default.removeItem(at: decoded)
            throw TranscriptionError.unreadableAudio(error.localizedDescription)
        }
    }

    /// Decodes the first audio track of any file AVFoundation can read into a 48 kHz mono CAF in
    /// `folder`. The asset reader goes by the file's extension, so a file it does not recognise
    /// is tried again as a QuickTime movie.
    static func decode(_ url: URL, into folder: URL) async throws -> URL {
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let output = folder.appending(path: "\(UUID().uuidString).caf")
        do {
            try await read(url, to: output)
        } catch {
            try? FileManager.default.removeItem(at: output)
            let link = folder.appending(path: "\(UUID().uuidString).mov")
            defer { try? FileManager.default.removeItem(at: link) }
            do {
                try FileManager.default.createSymbolicLink(at: link, withDestinationURL: url.resolvingSymlinksInPath())
                try await read(link, to: output)
            } catch {
                try? FileManager.default.removeItem(at: output)
                throw TranscriptionError.unreadableAudio(error.localizedDescription)
            }
        }
        return output
    }

    private static func read(_ url: URL, to output: URL) async throws {
        let asset = AVURLAsset(url: url)
        guard let track = try await asset.loadTracks(withMediaType: .audio).first else {
            throw TranscriptionError.unreadableAudio("the file has no audio")
        }
        let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48_000, channels: 1, interleaved: false)!
        let reader = try AVAssetReader(asset: asset)
        let trackOutput = AVAssetReaderTrackOutput(track: track, outputSettings: [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: format.sampleRate,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 32,
            AVLinearPCMIsFloatKey: true,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsNonInterleaved: false,
        ])
        guard reader.canAdd(trackOutput) else { throw TranscriptionError.unreadableAudio("the audio cannot be decoded") }
        reader.add(trackOutput)
        guard reader.startReading() else {
            throw TranscriptionError.unreadableAudio(reader.error?.localizedDescription ?? "the audio cannot be decoded")
        }
        let file = try AVAudioFile(forWriting: output, settings: format.settings, commonFormat: .pcmFormatFloat32, interleaved: false)
        defer { file.close() }
        while let sample = trackOutput.copyNextSampleBuffer() {
            try Task.checkCancellation()
            let frames = CMSampleBufferGetNumSamples(sample)
            guard frames > 0, let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(frames)) else { continue }
            buffer.frameLength = AVAudioFrameCount(frames)
            let status = CMSampleBufferCopyPCMDataIntoAudioBufferList(sample, at: 0, frameCount: Int32(frames), into: buffer.mutableAudioBufferList)
            guard status == noErr else { throw TranscriptionError.unreadableAudio("Core Media error \(status)") }
            try file.write(from: buffer)
        }
        if reader.status == .failed {
            throw TranscriptionError.unreadableAudio(reader.error?.localizedDescription ?? "the audio cannot be decoded")
        }
    }
}
