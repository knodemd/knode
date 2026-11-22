import Foundation

/// A finished recording that is not in its space yet: saving failed, was interrupted by the app
/// quitting, or has not been tried again. Its audio and a manifest stay in its own folder until it
/// is saved or the person deletes it.
struct UnsavedRecording: Identifiable, Hashable, Sendable {
    /// The folder's name.
    var id: String
    var folder: URL
    var recording: FinishedRecording
    /// Why the last save failed.
    var error: String?
}

/// Where finished recordings wait to be saved: `<Knode home>/Unsaved Recordings/<id>/`, the audio
/// files beside a `manifest.json` that says where the recording goes (space and project), what
/// it recorded (mode, start, duration, tracks) and the key that keeps a second save from storing
/// it twice. Each recording moves here as soon as it stops, so nothing is lost if saving fails or
/// the app quits, and leaves once its space holds it.
struct UnsavedRecordingStore: Sendable {
    static let folderName = "Unsaved Recordings"
    static let manifestName = "manifest.json"

    let root: URL

    init(root: URL) {
        self.root = root
    }

    init(home: KnodeHome) {
        self.init(root: home.url.appending(path: Self.folderName, directoryHint: .isDirectory))
    }

    /// Moves the recording's audio files into a folder of their own here and writes its
    /// manifest; returns the recording with its files at their new place.
    func keep(_ recording: FinishedRecording) throws -> UnsavedRecording {
        let id = UUID().uuidString
        let folder = root.appending(path: id, directoryHint: .isDirectory)
        let files = FileManager.default
        try files.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        var moved = recording
        do {
            moved.tracks = try recording.tracks.map { track in
                let destination = folder.appending(path: track.url.lastPathComponent)
                try files.moveItem(at: track.url, to: destination)
                return RecordedTrack(role: track.role, url: destination)
            }
            try write(Manifest(moved, error: nil), to: folder)
        } catch {
            // Put back what moved, so the files stay where the recorder left them.
            for (old, new) in zip(recording.tracks, moved.tracks) where old.url != new.url {
                try? files.moveItem(at: new.url, to: old.url)
            }
            try? files.removeItem(at: folder)
            throw error
        }
        for old in Set(recording.tracks.map { $0.url.deletingLastPathComponent() }) {
            if (try? files.contentsOfDirectory(atPath: old.path(percentEncoded: false)))?.isEmpty == true {
                try? files.removeItem(at: old)
            }
        }
        return UnsavedRecording(id: id, folder: folder, recording: moved, error: nil)
    }

    /// Every recording waiting here whose manifest can be read, oldest first.
    func list() -> [UnsavedRecording] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: root.path(percentEncoded: false))) ?? []
        return names.filter { !$0.hasPrefix(".") }
            .compactMap { read(root.appending(path: $0, directoryHint: .isDirectory)) }
            .sorted { $0.recording.startedAt < $1.recording.startedAt }
    }

    /// Keeps why the last save failed, for the menu.
    func setError(_ error: String?, of id: String) {
        let folder = root.appending(path: id, directoryHint: .isDirectory)
        guard let unsaved = read(folder) else { return }
        try? write(Manifest(unsaved.recording, error: error), to: folder)
    }

    /// Removes the recording and its audio for good.
    func delete(_ id: String) throws {
        let folder = root.appending(path: id, directoryHint: .isDirectory)
        guard FileManager.default.fileExists(atPath: folder.path(percentEncoded: false)) else { return }
        try FileManager.default.removeItem(at: folder)
    }

    private func read(_ folder: URL) -> UnsavedRecording? {
        guard let data = try? Data(contentsOf: folder.appending(path: Self.manifestName)),
              let manifest = try? Self.decoder.decode(Manifest.self, from: data),
              let recording = manifest.recording(in: folder)
        else { return nil }
        return UnsavedRecording(id: folder.lastPathComponent, folder: folder, recording: recording, error: manifest.error)
    }

    private func write(_ manifest: Manifest, to folder: URL) throws {
        try Self.encoder.encode(manifest).write(to: folder.appending(path: Self.manifestName), options: .atomic)
    }

    private static var encoder: JSONEncoder {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return encoder
    }

    private static var decoder: JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return decoder
    }

    /// `manifest.json`. Track files are named relative to the folder, so it can be moved.
    struct Manifest: Codable, Equatable {
        struct Track: Codable, Equatable {
            var role: RecordedTrack.Role
            var file: String
        }

        var version = 1
        var space: String
        var spaceName: String
        var project: String?
        var projectTitle: String?
        /// The calendar event it is linked to, and its title.
        var event: String?
        var eventTitle: String?
        /// `online` or `in-person`.
        var mode: String
        /// The apps an online meeting recorded, when not every app.
        var apps: [String]?
        var startedAt: Date
        var endedAt: Date
        var durationSeconds: Double
        var tracks: [Track]
        var key: String
        var error: String?

        enum CodingKeys: String, CodingKey {
            case version, space, project, event, mode, apps, tracks, key, error
            case spaceName = "space_name"
            case eventTitle = "event_title"
            case projectTitle = "project_title"
            case startedAt = "started_at"
            case endedAt = "ended_at"
            case durationSeconds = "duration_seconds"
        }

        init(_ recording: FinishedRecording, error: String?) {
            space = recording.target.spaceURL.standardizedFileURL.path(percentEncoded: false)
            spaceName = recording.target.spaceName
            project = recording.target.projectID
            projectTitle = recording.target.projectTitle
            event = recording.target.eventID
            eventTitle = recording.target.eventTitle
            switch recording.mode {
            case .inPerson:
                mode = "in-person"
            case .online(.allApps):
                mode = "online"
            case .online(.apps(let ids)):
                mode = "online"
                apps = ids
            }
            startedAt = recording.startedAt
            endedAt = recording.endedAt
            let parts = recording.duration.components
            durationSeconds = Double(parts.seconds) + Double(parts.attoseconds) / 1e18
            tracks = recording.tracks.map { Track(role: $0.role, file: $0.url.lastPathComponent) }
            key = recording.key
            self.error = error
        }

        func recording(in folder: URL) -> FinishedRecording? {
            let mode: RecordingMode
            switch self.mode {
            case "in-person": mode = .inPerson
            case "online": mode = .online(apps.map { .apps(bundleIDs: $0) } ?? .allApps)
            default: return nil
            }
            return FinishedRecording(
                mode: mode,
                target: RecordingTarget(
                    spaceURL: URL(filePath: space, directoryHint: .isDirectory), spaceName: spaceName,
                    projectID: project, projectTitle: projectTitle, eventID: event, eventTitle: eventTitle
                ),
                startedAt: startedAt, endedAt: endedAt, duration: .seconds(durationSeconds),
                tracks: tracks.map { RecordedTrack(role: $0.role, url: folder.appending(path: $0.file)) },
                key: key
            )
        }
    }
}

/// When a recording stops by itself: before a track would grow past what a space keeps in one
/// file. At 64 kbps a track grows by 8 kB a second, so 100 MB is about 3 h 38 min; it stops at
/// 95 % of that, and warns from three hours on.
struct RecordingLimits: Hashable, Sendable {
    var warnAfter: Duration
    var stopAfter: Duration

    init(warnAfter: Duration, stopAfter: Duration) {
        self.warnAfter = warnAfter
        self.stopAfter = stopAfter
    }

    init(fileLimit: Int64 = AttachmentInfo.maxSize, bitRate: Int = TrackWriter.bitRate) {
        let seconds = Double(fileLimit) * 0.95 / (Double(bitRate) / 8)
        stopAfter = .seconds(seconds)
        warnAfter = min(.seconds(3 * 3600), .seconds(max(seconds - 600, 0)))
    }
}
