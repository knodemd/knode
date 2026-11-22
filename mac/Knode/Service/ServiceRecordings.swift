import Foundation

// Recordings through Knode Service (`PROTOCOL.md`, "Recordings"). Creating and retrying are the
// person's; transcripts, minutes and failures are written as Knode itself, with the on-device
// engine as `source`.
extension ServiceSpaceStore: RecordingStore {
    func createRecording(_ recording: NewRecordingDraft, in space: Space.ID) async throws -> StoredRecording {
        let path = try path(space)
        var params: [String: JSONValue] = [
            "space": .string(path), "kind": .string(recording.kind.rawValue), "title": .string(recording.title),
            "started_at": .int(RecordingWire.milliseconds(recording.startedAt)),
            "ended_at": .int(RecordingWire.milliseconds(recording.endedAt)),
            "audio": RecordingWire.audio(recording.audio), "summaries": .bool(recording.summaries), "by": author,
        ]
        if let project = recording.project { params["project"] = .string(project) }
        if let key = recording.key { params["key"] = .string(key) }
        if let app = recording.app { params["app"] = .string(app) }
        if let original = recording.original { params["original"] = .string(original) }
        return try await call("recordings.create", params, as: WireRecording.self).recording()
    }

    func pendingRecordings(in space: Space.ID) async throws -> [StoredRecording] {
        let path = try path(space)
        return try await call("recordings.pending", ["space": .string(path)], as: [WireRecording].self).map { try $0.recording() }
    }

    func failedRecordings(in space: Space.ID) async throws -> [StoredRecording] {
        let path = try path(space)
        return try await call("recordings.failed", ["space": .string(path)], as: [WireRecording].self).map { try $0.recording() }
    }

    func recording(_ id: Node.ID, in space: Space.ID) async throws -> StoredRecording {
        let path = try path(space)
        return try await call("recordings.get", ["space": .string(path), "id": .string(id)], as: WireRecording.self, node: id).recording()
    }

    func setTranscript(_ transcript: Transcript, by engine: RecordingEngine, of id: Node.ID, in space: Space.ID) async throws -> StoredRecording {
        let path = try path(space)
        let params: [String: JSONValue] = [
            "space": .string(path), "id": .string(id),
            "transcript": RecordingWire.transcript(transcript, engine: engine), "by": RecordingWire.provenance(engine),
        ]
        return try await call("recordings.transcript", params, as: WireRecording.self, node: id).recording()
    }

    func setSummary(_ summary: MeetingSummary, by engine: RecordingEngine, of id: Node.ID, in space: Space.ID) async throws -> StoredRecording {
        let path = try path(space)
        let params: [String: JSONValue] = [
            "space": .string(path), "id": .string(id),
            "summary": RecordingWire.summary(summary), "by": RecordingWire.provenance(engine),
        ]
        return try await call("recordings.summary", params, as: WireRecording.self, node: id).recording()
    }

    func failRecording(_ id: Node.ID, stage: StoredRecording.Stage, failure: StoredRecording.Failure, error: String, by engine: RecordingEngine, in space: Space.ID) async throws -> StoredRecording {
        let path = try path(space)
        let params: [String: JSONValue] = [
            "space": .string(path), "id": .string(id), "stage": .string(stage.rawValue),
            "state": .string(failure.rawValue), "error": .string(error), "by": RecordingWire.provenance(engine),
        ]
        return try await call("recordings.fail", params, as: WireRecording.self, node: id).recording()
    }

    func retryRecording(_ id: Node.ID, stage: StoredRecording.Stage, in space: Space.ID) async throws -> StoredRecording {
        let path = try path(space)
        let params: [String: JSONValue] = ["space": .string(path), "id": .string(id), "stage": .string(stage.rawValue), "by": author]
        return try await call("recordings.retry", params, as: WireRecording.self, node: id).recording()
    }

    func recordingSummariesEnabled(in space: Space.ID) async throws -> Bool {
        RecordingSummariesSetting.isOn(try await settings(of: space)[RecordingSummariesSetting.key])
    }

    func setRecordingSummariesEnabled(_ enabled: Bool, in space: Space.ID) async throws {
        try await setSetting(RecordingSummariesSetting.key, to: RecordingSummariesSetting.value(enabled), of: space)
    }
}
