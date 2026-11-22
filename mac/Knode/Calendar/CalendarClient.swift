import Foundation

/// What a space wants from the calendar now: `calendar.plan`.
struct CalendarPlan: Hashable, Sendable, Decodable {
    /// Whether the source is on there; when off, nothing is read for it.
    var on: Bool
    /// The days to read, in Unix milliseconds.
    var start: Int64
    var end: Int64
    /// The calendars excluded: never read.
    var exclude: [String]
    var attendeeEmails: Bool
    /// When Knode Service asks the app to run it next, at the latest; `nil` before its first run.
    var nextRun: Int64?
}

/// The calendar options of a space: `calendar.options`.
struct CalendarOptions: Hashable, Sendable, Decodable {
    static let standard = CalendarOptions(daysBack: 30, daysAhead: 14, attendeeEmails: false)

    var daysBack: Int
    var daysAhead: Int
    var attendeeEmails: Bool
}

/// An event of a space, as `calendar.current` and `calendar.link` answer it.
struct CalendarEventSummary: Hashable, Sendable, Decodable, Identifiable {
    var id: Node.ID
    var title: String
    var start: Int64
    var end: Int64
    var callLink: String?
    var project: Node.ID?
}

/// An event the space has that `calendar.sync` did not find among the events handed over: gone
/// from its calendar, or moved outside the days read. The app asks EventKit which.
struct CalendarMissing: Hashable, Sendable, Decodable {
    var key: String
    var externalId: String
    /// For an occurrence of a recurring event, when it was planned to start (Unix milliseconds).
    var occurrence: Int64?
    var calendarId: String
}

/// What `calendar.sync` answers.
struct CalendarSyncResult: Sendable, Decodable {
    struct Written: Sendable, Decodable {
        var created: Int
        var updated: Int
    }

    var written: Written
    var missing: [CalendarMissing]
    var source: SourceStatus
}

/// Linking a recording to its event, for the recorder.
protocol CalendarLinking: Sendable {
    /// The event of the space happening at `date`, that a meeting recorded then belongs to.
    func currentEvent(at date: Date, in space: URL) async throws -> CalendarEventSummary?
    /// Links the recording to the event, and the event to `project` when it has none yet.
    func link(_ recording: Node.ID, to event: Node.ID, project: Node.ID?, in space: URL) async throws
}

/// The calendar source in Knode Service (`PROTOCOL.md`, "Calendar"): the app reads EventKit and
/// hands what it read to the service, which decides what to write.
protocol CalendarClient: CalendarLinking {
    /// The spaces the calendar source is connected to: `sources.status`.
    func connectedSpaces() async throws -> [URL]
    func plan(in space: URL) async throws -> CalendarPlan
    func reportCalendars(_ calendars: [CalendarInfo]) async throws
    /// Hands over the events read from `calendars` (never the excluded ones) for the days from
    /// `start` to `end`.
    func sync(_ events: [CalendarRecord], calendars: [CalendarInfo], from start: Int64, to end: Int64, in space: URL) async throws -> CalendarSyncResult
    /// Marks the events with these keys removed: those missing that are gone from their calendar.
    func remove(_ keys: [String], in space: URL) async throws
    func fail(kind: String, message: String, in space: URL) async throws
    func options(in space: URL) async throws -> CalendarOptions
    func configure(_ options: CalendarOptions, in space: URL) async throws -> CalendarOptions
}

/// The JSON of the `calendar.*` params.
enum CalendarWire {
    static let source = "calendar"

    /// The most characters of an event's notes handed over: the core writes at most 1500, and a
    /// few more tell it they were cut.
    static let maxNotes = 1600

    static func milliseconds(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1000).rounded()) }

    /// The notes as handed over: trimmed, as the core reads them, and cut at `maxNotes`.
    static func notes(_ notes: String?) -> String? {
        guard let notes = notes?.trimmingCharacters(in: .whitespacesAndNewlines), !notes.isEmpty else { return nil }
        return notes.count > maxNotes ? String(notes.prefix(maxNotes)) : notes
    }

    static func calendars(_ calendars: [CalendarInfo]) -> JSONValue {
        .array(calendars.map { ["id": .string($0.id), "name": .string($0.name)] })
    }

    static func event(_ record: CalendarRecord) -> JSONValue {
        var event: [String: JSONValue] = [
            "external_id": .string(record.externalID),
            "calendar_id": .string(record.calendarID),
            "calendar": .string(record.calendarName),
            "title": .string(record.title),
            "start": .int(milliseconds(record.start)),
            "end": .int(milliseconds(record.end)),
            "all_day": .bool(record.isAllDay),
            "status": .string(record.status.rawValue),
        ]
        if let occurrence = record.occurrence { event["occurrence"] = .int(milliseconds(occurrence)) }
        if let location = record.location, !location.isEmpty { event["location"] = .string(location) }
        if let url = record.url, !url.isEmpty { event["url"] = .string(url) }
        if let notes = notes(record.notes) { event["notes"] = .string(notes) }
        if !record.attendees.isEmpty {
            event["attendees"] = .array(record.attendees.map { attendee in
                var json: [String: JSONValue] = ["name": .string(attendee.name)]
                if let email = attendee.email { json["email"] = .string(email) }
                return .object(json)
            })
        }
        return .object(event)
    }
}

/// The `CalendarClient` of Knode Service, over its socket as the person.
struct ServiceCalendarClient: CalendarClient {
    var socketURL: URL = KnodeHome.current.socketURL
    var person: String = CoreSpaceStore.currentUserName

    private struct SpaceSources: Decodable {
        var space: String
        var sources: [SourceStatus]
    }

    private func space(_ url: URL) -> JSONValue { .string(ServiceSpaceStore.path(of: url)) }

    func connectedSpaces() async throws -> [URL] {
        let list = try await call("sources.status", ["source": .string(CalendarWire.source)], as: [SpaceSources].self)
        return list.filter { !$0.sources.isEmpty }.map { URL(filePath: $0.space, directoryHint: .isDirectory) }
    }

    func plan(in space: URL) async throws -> CalendarPlan {
        try await call("calendar.plan", ["space": self.space(space)], as: CalendarPlan.self)
    }

    func reportCalendars(_ calendars: [CalendarInfo]) async throws {
        _ = try await call("calendar.calendars", ["calendars": CalendarWire.calendars(calendars)], as: Bool.self)
    }

    func sync(_ events: [CalendarRecord], calendars: [CalendarInfo], from start: Int64, to end: Int64, in space: URL) async throws -> CalendarSyncResult {
        try await call("calendar.sync", [
            "space": self.space(space), "calendars": CalendarWire.calendars(calendars),
            "start": .int(start), "end": .int(end), "events": .array(events.map(CalendarWire.event)),
        ], as: CalendarSyncResult.self)
    }

    func remove(_ keys: [String], in space: URL) async throws {
        struct Removed: Decodable { var removed: Int }
        _ = try await call("calendar.remove", ["space": self.space(space), "keys": .array(keys.map { .string($0) })], as: Removed.self)
    }

    func fail(kind: String, message: String, in space: URL) async throws {
        _ = try await call("calendar.fail", ["space": self.space(space), "kind": .string(kind), "message": .string(message)], as: SourceStatus.self)
    }

    func options(in space: URL) async throws -> CalendarOptions {
        try await call("calendar.options", ["space": self.space(space)], as: CalendarOptions.self)
    }

    func configure(_ options: CalendarOptions, in space: URL) async throws -> CalendarOptions {
        try await call("calendar.configure", [
            "space": self.space(space), "days_back": .int(Int64(options.daysBack)), "days_ahead": .int(Int64(options.daysAhead)),
            "attendee_emails": .bool(options.attendeeEmails), "by": ["kind": "person", "actor": .string(person)],
        ], as: CalendarOptions.self)
    }

    func currentEvent(at date: Date, in space: URL) async throws -> CalendarEventSummary? {
        try await call("calendar.current", ["space": self.space(space), "at": .int(CalendarWire.milliseconds(date))], as: CalendarEventSummary?.self)
    }

    func link(_ recording: Node.ID, to event: Node.ID, project: Node.ID?, in space: URL) async throws {
        var params: [String: JSONValue] = [
            "space": self.space(space), "id": .string(recording), "event": .string(event),
            "by": ["kind": "person", "actor": .string(person)],
        ]
        if let project { params["project"] = .string(project) }
        _ = try await call("calendar.link", params, as: CalendarEventSummary.self)
    }

    private func call<Result: Decodable & Sendable>(_ method: String, _ params: [String: JSONValue], as type: Result.Type) async throws -> Result {
        let connection = try ServiceConnection.connect(to: socketURL)
        defer { connection.close() }
        return try await connection.call(method, params, as: type)
    }
}
