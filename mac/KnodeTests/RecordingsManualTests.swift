import Foundation
import FoundationModels
import Testing
@testable import Knode

/// Checks against the real on-device models, run by hand only: they download speech assets and
/// depend on Apple Intelligence. Make a Spanish file (the voice is Mónica, with the accent: `say`
/// falls back to an English voice for `Monica`) and point the test at it:
///
///     say -v Mónica -o /tmp/x.aiff "Hola a todos. Hemos decidido lanzar la versión el viernes catorce."
///     TEST_RUNNER_KNODE_MANUAL_AUDIO=/tmp/x.aiff xcodebuild … test -only-testing:KnodeTests/RecordingsManualTests
struct RecordingsManualTests {
    static let audio = ProcessInfo.processInfo.environment["KNODE_MANUAL_AUDIO"]

    @Test(.enabled(if: audio != nil, "Set KNODE_MANUAL_AUDIO to a Spanish audio file"))
    func transcribesSpanishOnTheDevice() async throws {
        let url = URL(filePath: try #require(Self.audio))
        let transcript = try await SpeechAnalyzerTranscriber().transcribe(
            [RecordedTrack(role: .mixed, url: url)],
            locale: Locale(identifier: "es-ES")
        ) { _ in }
        print("MANUAL transcript (\(transcript.locale)):", transcript.segments.map { "[\($0.startMs)-\($0.endMs)] \($0.text)" })
        #expect(transcript.locale.hasPrefix("es"))
        #expect(!transcript.segments.isEmpty)
        #expect(transcript.text.localizedCaseInsensitiveContains("viernes"))

        let summarizer = FoundationModelsSummarizer()
        let availability = await summarizer.availability(for: transcript)
        print("MANUAL Foundation Models availability:", availability, "context size:", SystemLanguageModel.default.contextSize)
        if availability == .available {
            let summary = try await summarizer.summarize(transcript) { _ in }
            print("MANUAL minutes:", summary)
        }
    }
}
