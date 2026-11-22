import EventKit
import Foundation

/// Whether the app may read the calendars of this Mac.
enum CalendarAccess: Equatable, Sendable {
    /// Never asked: connecting the source asks.
    case notDetermined
    /// The person said no, or allowed adding events only.
    case denied
    /// A profile or parental controls forbid it.
    case restricted
    case fullAccess
}

/// A calendar of this Mac.
struct CalendarInfo: Hashable, Sendable, Identifiable {
    var id: String
    var name: String
}

/// An event as the app read it, before it goes to the core (`calendar.sync`).
struct CalendarRecord: Hashable, Sendable {
    enum Status: String, Hashable, Sendable {
        case confirmed, tentative, cancelled
    }

    struct Attendee: Hashable, Sendable {
        var name: String
        var email: String?
    }

    /// EventKit's `calendarItemExternalIdentifier`: the same on every device.
    var externalID: String
    /// For an occurrence of a recurring event, when it was planned to start.
    var occurrence: Date?
    var calendarID: String
    var calendarName: String
    var title: String
    var start: Date
    var end: Date
    var isAllDay: Bool
    var location: String?
    var url: String?
    var notes: String?
    var attendees: [Attendee]
    var status: Status
}

/// The calendars of this Mac, behind a protocol so tests use a fake and never ask for access.
protocol CalendarEventStore: Sendable {
    var access: CalendarAccess { get }
    /// Asks the person for full access to the calendars. Only when they connect the source.
    func requestFullAccess() async throws -> Bool
    func calendars() -> [CalendarInfo]
    /// The events from `start` to `end` of the calendars with these ids.
    func events(from start: Date, to end: Date, calendars ids: Set<String>) -> [CalendarRecord]
    /// Yields when a calendar of this Mac changes.
    func changes() -> AsyncStream<Void>
}

/// The calendars of this Mac, through EventKit. The event store is made only once access is
/// granted or asked for, so merely launching the app touches nothing.
final class EventKitCalendarStore: CalendarEventStore, @unchecked Sendable {
    private let lock = NSLock()
    private var made: EKEventStore?

    private var store: EKEventStore {
        lock.withLock {
            if let made { return made }
            let store = EKEventStore()
            made = store
            return store
        }
    }

    var access: CalendarAccess {
        switch EKEventStore.authorizationStatus(for: .event) {
        case .notDetermined: .notDetermined
        case .restricted: .restricted
        case .fullAccess: .fullAccess
        case .denied, .writeOnly: .denied
        @unknown default: .denied
        }
    }

    func requestFullAccess() async throws -> Bool {
        try await store.requestFullAccessToEvents()
    }

    func calendars() -> [CalendarInfo] {
        guard access == .fullAccess else { return [] }
        return store.calendars(for: .event).map { CalendarInfo(id: $0.calendarIdentifier, name: $0.title) }
    }

    func events(from start: Date, to end: Date, calendars ids: Set<String>) -> [CalendarRecord] {
        guard access == .fullAccess, start < end else { return [] }
        let store = self.store
        let calendars = store.calendars(for: .event).filter { ids.contains($0.calendarIdentifier) }
        guard !calendars.isEmpty else { return [] }
        // EventKit answers at most four years at a time.
        var out: [CalendarRecord] = []
        var seen = Set<String>()
        var from = start
        while from < end {
            let to = min(end, from.addingTimeInterval(365 * 86_400))
            let predicate = store.predicateForEvents(withStart: from, end: to, calendars: calendars)
            for event in store.events(matching: predicate) {
                let record = Self.record(event)
                let key = record.externalID + "@" + String(record.occurrence?.timeIntervalSince1970 ?? 0)
                if seen.insert(key).inserted { out.append(record) }
            }
            from = to
        }
        return out
    }

    func changes() -> AsyncStream<Void> {
        let store = self.store
        return AsyncStream { continuation in
            let observer = NotificationCenter.default.addObserver(forName: .EKEventStoreChanged, object: store, queue: nil) { _ in
                continuation.yield()
            }
            nonisolated(unsafe) let token = observer
            continuation.onTermination = { _ in NotificationCenter.default.removeObserver(token) }
        }
    }

    static func record(_ event: EKEvent) -> CalendarRecord {
        var attendees: [CalendarRecord.Attendee] = []
        if let organizer = event.organizer { attendees.append(attendee(organizer)) }
        attendees += (event.attendees ?? []).map(attendee)
        let status: CalendarRecord.Status = switch event.status {
        case .canceled: .cancelled
        case .tentative: .tentative
        default: .confirmed
        }
        return CalendarRecord(
            externalID: event.calendarItemExternalIdentifier ?? event.calendarItemIdentifier,
            occurrence: event.hasRecurrenceRules ? event.occurrenceDate : nil,
            calendarID: event.calendar?.calendarIdentifier ?? "",
            calendarName: event.calendar?.title ?? "",
            title: event.title ?? "",
            start: event.startDate,
            end: event.endDate,
            isAllDay: event.isAllDay,
            location: event.location,
            url: event.url?.absoluteString,
            notes: event.notes,
            attendees: attendees,
            status: status
        )
    }

    private static func attendee(_ participant: EKParticipant) -> CalendarRecord.Attendee {
        let url = participant.url
        let email = url.scheme?.lowercased() == "mailto" ? String(url.absoluteString.dropFirst("mailto:".count)) : nil
        return CalendarRecord.Attendee(name: participant.name ?? "", email: email)
    }
}
