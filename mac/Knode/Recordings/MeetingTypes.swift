import Foundation

/// What a recording captures: a meeting on this Mac (the system audio and the microphone, on two
/// tracks) or people in a room (the microphone only).
enum RecordingMode: Hashable, Sendable {
    case online(SystemAudioSource)
    case inPerson

    var title: String {
        switch self {
        case .online: "Meeting"
        case .inPerson: "In person"
        }
    }
}

/// Whose audio an online meeting records besides the microphone.
enum SystemAudioSource: Hashable, Sendable {
    /// Every app's audio except Knode's own.
    case allApps
    /// Only the audio of the apps with these bundle identifiers, such as a call app.
    case apps(bundleIDs: [String])
}

/// One audio file of a recording.
struct RecordedTrack: Hashable, Sendable {
    enum Role: String, Hashable, Sendable, Codable {
        /// The microphone of an online meeting: the person using the Mac.
        case microphone
        /// The system audio of an online meeting: everyone else.
        case system
        /// The microphone of a meeting in a room: everyone in it.
        case mixed
    }

    var role: Role
    var url: URL

    /// The speaker a transcript gives this track's words to.
    var speaker: String? {
        switch role {
        case .microphone: TranscriptSegment.me
        case .system: TranscriptSegment.others
        case .mixed: nil
        }
    }
}

/// Where a recording goes: a space and, optionally, one of its projects.
struct RecordingTarget: Hashable, Sendable {
    var spaceURL: URL
    var spaceName: String
    var projectID: Node.ID?
    var projectTitle: String?
}

/// A recording that has stopped, with its audio files in a private temporary folder.
struct FinishedRecording: Hashable, Sendable {
    var mode: RecordingMode
    var target: RecordingTarget
    var startedAt: Date
    var endedAt: Date
    /// How long it recorded, pauses left out.
    var duration: Duration
    var tracks: [RecordedTrack]
    /// Finds the stored recording again, so storing it twice keeps one.
    var key: String
}

/// A stretch of speech, in milliseconds from the start of the recording.
struct TranscriptSegment: Hashable, Sendable, Codable {
    static let me = "Me"
    static let others = "Others"
    /// The speaker stored for words heard by one microphone in a room, where no one is told apart.
    static let unnamedSpeaker = "Speaker"

    var startMs: Int
    var endMs: Int
    var speaker: String?
    var text: String

    enum CodingKeys: String, CodingKey {
        case startMs = "start_ms"
        case endMs = "end_ms"
        case speaker, text
    }
}

/// Every word of a recording, in time order. It is always kept, whatever the summary says.
struct Transcript: Hashable, Sendable, Codable {
    /// The BCP 47 identifier of the language it was transcribed in, such as `es-ES`.
    var locale: String
    var segments: [TranscriptSegment]

    var text: String {
        segments.map { segment in
            segment.speaker.map { "\($0): \(segment.text)" } ?? segment.text
        }
        .joined(separator: "\n")
    }
}

/// A task someone took on in a meeting.
struct ActionItem: Hashable, Sendable, Codable {
    var task: String
    var owner: String?
}

/// The minutes of a meeting, written on the device. They never replace the transcript.
struct MeetingSummary: Hashable, Sendable, Codable {
    /// The model that wrote them, for provenance.
    var model: String
    var overview: String
    var keyPoints: [String]
    var decisions: [String]
    var actionItems: [ActionItem]
    var openQuestions: [String]

    enum CodingKeys: String, CodingKey {
        case model, overview, decisions
        case keyPoints = "key_points"
        case actionItems = "action_items"
        case openQuestions = "open_questions"
    }
}
