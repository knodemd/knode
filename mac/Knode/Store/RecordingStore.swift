import Foundation

/// Recordings kept in a space: meetings recorded on this Mac, voice memos and audio files, with
/// their transcript and their minutes (`PROTOCOL.md`, "Recordings"). The space says what is still
/// to do; the app transcribes and summarises on the device and stores what it made.
///
/// `ServiceSpaceStore` implements it through Knode Service, `CoreSpaceStore` with the core in the
/// app, and `RoutingSpaceStore` with whichever of the two holds the space. Creating and retrying
/// are the person's; transcripts, minutes and failures are written by Knode itself, as the
/// on-device engine that made them (`RecordingEngine`).
protocol RecordingStore: Sendable {
    /// Stores a recording: its node, its audio files copied in and a link to its project. A `key`
    /// the space already has returns that recording unchanged, so it is safe to call again.
    func createRecording(_ recording: NewRecordingDraft, in space: Space.ID) async throws -> StoredRecording

    /// The recordings whose transcript is pending, or whose summary is pending once transcribed,
    /// oldest first.
    func pendingRecordings(in space: Space.ID) async throws -> [StoredRecording]

    /// The recordings whose transcript or minutes failed, which the person can retry.
    func failedRecordings(in space: Space.ID) async throws -> [StoredRecording]

    func recording(_ id: Node.ID, in space: Space.ID) async throws -> StoredRecording

    /// Writes the transcript, replacing the one written before.
    func setTranscript(_ transcript: Transcript, by engine: RecordingEngine, of id: Node.ID, in space: Space.ID) async throws -> StoredRecording

    /// Writes the minutes into the recording's body, replacing the ones written before.
    func setSummary(_ summary: MeetingSummary, by engine: RecordingEngine, of id: Node.ID, in space: Space.ID) async throws -> StoredRecording

    /// Marks a stage failed, or the summary unavailable on this Mac, with why.
    func failRecording(_ id: Node.ID, stage: StoredRecording.Stage, failure: StoredRecording.Failure, error: String, by engine: RecordingEngine, in space: Space.ID) async throws -> StoredRecording

    /// Puts a stage back to pending, as the person.
    func retryRecording(_ id: Node.ID, stage: StoredRecording.Stage, in space: Space.ID) async throws -> StoredRecording

    /// Whether the space wants minutes (`recordings.summaries`): on unless it says `off`.
    func recordingSummariesEnabled(in space: Space.ID) async throws -> Bool

    /// Turns minutes on (`on`) or off (`off`) for the recordings stored from now on, as the person.
    func setRecordingSummariesEnabled(_ enabled: Bool, in space: Space.ID) async throws
}

/// What the recordings pipeline needs from the app's store: the recordings, the space and its
/// attachments, and its changes.
protocol RecordingsBackend: RecordingStore {
    func openSpace(at url: URL) async throws -> Space
    func node(_ id: Node.ID, in space: Space.ID) async throws -> Node
    func attachmentURL(named name: String, of node: Node.ID, in space: Space.ID) async throws -> URL?
    func changes() -> AsyncStream<SpaceChange>
}

/// The space setting that turns minutes on or off.
enum RecordingSummariesSetting {
    static let key = "recordings.summaries"

    /// Absent or `on` means on; only `off` is off.
    static func isOn(_ value: String?) -> Bool { value != "off" }

    static func value(_ on: Bool) -> String { on ? "on" : "off" }
}

/// Who wrote a transcript, minutes or a failure: Knode itself, as the on-device engine. Its
/// provenance is `{ "kind": "system", "actor": "recordings", "source": id }`.
struct RecordingEngine: Hashable, Sendable {
    static let actor = "recordings"
    static let speech = RecordingEngine(name: "Apple SpeechAnalyzer", id: "apple-speech")
    static let minutes = RecordingEngine(name: "Apple Foundation Models", id: "apple-foundation-models")

    /// The name written into the transcript, such as `Apple SpeechAnalyzer`.
    var name: String
    /// The engine in the provenance's `source`, such as `apple-speech`.
    var id: String
}

/// A recording to store.
struct NewRecordingDraft: Hashable, Sendable {
    var kind: StoredRecording.Kind
    var title: String
    var startedAt: Date
    var endedAt: Date
    var project: Node.ID?
    var audio: [RecordedTrack]
    /// Finds it again: storing the same key twice keeps one recording.
    var key: String?
    /// The meeting app, such as `us.zoom.xos`.
    var app: String?
    var summaries: Bool
}

/// A recording as the space keeps it.
struct StoredRecording: Identifiable, Hashable, Sendable {
    enum Kind: String, Hashable, Sendable {
        case meeting
        case voiceMemo = "voice-memo"
        case recording
    }

    enum TranscriptState: String, Hashable, Sendable {
        case pending, done, failed
    }

    enum SummaryState: String, Hashable, Sendable {
        case pending, done, off, unavailable, failed
    }

    enum Stage: String, Hashable, Sendable {
        case transcript, summary
    }

    enum Failure: String, Hashable, Sendable {
        case failed, unavailable
    }

    /// One audio file attached to it.
    struct Track: Hashable, Sendable {
        /// The attachment's name in the recording's node.
        var name: String
        var role: RecordedTrack.Role
    }

    var id: Node.ID
    var kind: Kind
    var title: String
    var startedAt: Date
    var endedAt: Date
    var language: String?
    var transcript: TranscriptState
    var summary: SummaryState
    var error: String?
    var project: Node.ID?
    var tracks: [Track]
    var transcriptID: Node.ID?

    /// Whether a transcript or minutes are still to be made.
    var isPending: Bool {
        transcript == .pending || (transcript == .done && summary == .pending)
    }
}

/// The JSON of the `recordings.*` params (`PROTOCOL.md`, "Recordings").
enum RecordingWire {
    static func provenance(_ engine: RecordingEngine) -> JSONValue {
        ["kind": "system", "actor": .string(RecordingEngine.actor), "source": .string(engine.id)]
    }

    static func audio(_ tracks: [RecordedTrack]) -> JSONValue {
        .array(tracks.map { ["path": .string($0.url.standardizedFileURL.path(percentEncoded: false)), "track": .string($0.role.rawValue)] })
    }

    /// Every segment has a speaker on the wire: `Me`, `Others`, or `Speaker` when one microphone
    /// heard everyone.
    static func transcript(_ transcript: Transcript, engine: RecordingEngine) -> JSONValue {
        var object: [String: JSONValue] = [
            "engine": .string(engine.name),
            "segments": .array(transcript.segments.map { segment in
                [
                    "start_ms": .int(Int64(segment.startMs)), "end_ms": .int(Int64(segment.endMs)),
                    "speaker": .string(segment.speaker ?? TranscriptSegment.unnamedSpeaker), "text": .string(segment.text),
                ]
            }),
        ]
        if !transcript.locale.isEmpty { object["language"] = .string(transcript.locale) }
        return .object(object)
    }

    static func summary(_ summary: MeetingSummary) -> JSONValue {
        [
            "model": .string(summary.model),
            "overview": .string(summary.overview),
            "key_points": .array(summary.keyPoints.map(JSONValue.string)),
            "decisions": .array(summary.decisions.map(JSONValue.string)),
            "action_items": .array(summary.actionItems.map { item in
                var object: [String: JSONValue] = ["task": .string(item.task)]
                if let owner = item.owner { object["owner"] = .string(owner) }
                return .object(object)
            }),
            "open_questions": .array(summary.openQuestions.map(JSONValue.string)),
        ]
    }

    static func milliseconds(_ date: Date) -> Int64 {
        Int64((date.timeIntervalSince1970 * 1000).rounded())
    }
}

/// A recording as Knode Service answers it.
struct WireRecording: Decodable, Sendable {
    struct Track: Decodable, Sendable {
        var name: String
        var track: String
    }

    var id: String
    var kind: String
    var title: String
    var startedAt: Int64
    var endedAt: Int64
    var language: String?
    var transcript: String
    var summary: String
    var error: String?
    var project: String?
    var tracks: [Track]
    var transcriptId: String?

    func recording() throws -> StoredRecording {
        guard let kind = StoredRecording.Kind(rawValue: kind),
              let transcript = StoredRecording.TranscriptState(rawValue: transcript),
              let summary = StoredRecording.SummaryState(rawValue: summary)
        else { throw ServiceError.badResponse("a recording with an unknown kind or state") }
        return StoredRecording(
            id: id, kind: kind, title: title,
            startedAt: Date(unixMilliseconds: startedAt), endedAt: Date(unixMilliseconds: endedAt),
            language: language, transcript: transcript, summary: summary, error: error, project: project,
            tracks: tracks.compactMap { track in RecordedTrack.Role(rawValue: track.track).map { .init(name: track.name, role: $0) } },
            transcriptID: transcriptId
        )
    }
}

/// Reading a transcript back from its node, as the core writes it: a line naming the engine,
/// then one paragraph per turn, `**Speaker** (HH:MM:SS): text`.
enum TranscriptNode {
    static func transcript(fromBody body: String, language: String?) -> Transcript {
        let turn = /^\*\*(.+?)\*\* \((\d+):(\d\d):(\d\d)\): (.*)$/
        var segments: [TranscriptSegment] = []
        for line in body.split(separator: "\n", omittingEmptySubsequences: true) {
            guard let match = String(line).wholeMatch(of: turn),
                  let hours = Int(match.2), let minutes = Int(match.3), let seconds = Int(match.4)
            else { continue }
            let start = ((hours * 60 + minutes) * 60 + seconds) * 1000
            let speaker = String(match.1)
            segments.append(TranscriptSegment(
                startMs: start, endMs: start,
                speaker: speaker == TranscriptSegment.unnamedSpeaker ? nil : speaker,
                text: String(match.5)
            ))
        }
        return Transcript(locale: language ?? "", segments: segments)
    }
}

/// The title of a meeting recorded on this Mac: `Meeting, 22 November 2026 10:30` or
/// `In-person meeting, 22 November 2026 10:30`, the date and time in the person's locale.
enum RecordingTitle {
    static func of(_ mode: RecordingMode, startedAt date: Date, locale: Locale = .current, timeZone: TimeZone = .current) -> String {
        var day = Date.FormatStyle(date: .omitted, time: .omitted, locale: locale, timeZone: timeZone).day().month(.wide).year()
        day.timeZone = timeZone
        let time = Date.FormatStyle(date: .omitted, time: .shortened, locale: locale, timeZone: timeZone)
        let prefix = switch mode {
        case .online: "Meeting"
        case .inPerson: "In-person meeting"
        }
        return "\(prefix), \(date.formatted(day)) \(date.formatted(time))"
    }
}
