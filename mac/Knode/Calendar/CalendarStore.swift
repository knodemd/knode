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

/// How an EventKit event repeats: what tells an occurrence of a recurring event, a detached one
/// (moved or changed on its own) included, from an event that does not repeat.
struct EventRecurrence: Hashable, Sendable {
    var isDetached: Bool
    var hasRecurrenceRules: Bool
    var occurrenceDate: Date?

    init(isDetached: Bool, hasRecurrenceRules: Bool, occurrenceDate: Date?) {
        self.isDetached = isDetached
        self.hasRecurrenceRules = hasRecurrenceRules
        self.occurrenceDate = occurrenceDate
    }

    init(_ event: EKEvent) {
        self.init(isDetached: event.isDetached, hasRecurrenceRules: event.hasRecurrenceRules, occurrenceDate: event.occurrenceDate)
    }

    /// When this occurrence was planned to start, or `nil` for an event that does not repeat. A
    /// detached occurrence has no recurrence rules of its own but is still an occurrence.
    var occurrence: Date? { isDetached || hasRecurrenceRules ? occurrenceDate : nil }

    /// Whether an event the core found missing (`calendar.sync`) is gone from its calendar, given
    /// the events of the calendars read with its external id, and those of its series planned
    /// near its occurrence. One that does not repeat is gone when there is no such event; an
    /// occurrence when none of them is the occurrence planned at that time (moved or not).
    static func isGone(_ missing: CalendarMissing, found: [EventRecurrence]) -> Bool {
        guard !found.isEmpty else { return true }
        guard let occurrence = missing.occurrence else { return false }
        let planned = Date(timeIntervalSince1970: TimeInterval(occurrence) / 1000)
        return !found.contains { item in
            guard let date = item.occurrenceDate else { return false }
            return abs(date.timeIntervalSince(planned)) < 1
        }
    }
}

/// The calendars of this Mac, behind a protocol so tests use a fake and never ask for access.
protocol CalendarEventStore: Sendable {
    var access: CalendarAccess { get }
    /// Asks the person for full access to the calendars. Only when they connect the source.
    func requestFullAccess() async throws -> Bool
    func calendars() -> [CalendarInfo]
    /// The events from `start` to `end` of the calendars with these ids.
    func events(from start: Date, to end: Date, calendars ids: Set<String>) -> [CalendarRecord]
    /// The keys of the missing events that are gone from the calendars with these ids, rather
    /// than moved outside the days read.
    func gone(_ missing: [CalendarMissing], calendars ids: Set<String>) -> [String]
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

    func gone(_ missing: [CalendarMissing], calendars ids: Set<String>) -> [String] {
        guard access == .fullAccess, !missing.isEmpty else { return [] }
        let store = self.store
        let calendars = store.calendars(for: .event).filter { ids.contains($0.calendarIdentifier) }
        func read(_ event: EKEvent) -> Bool { event.calendar.map { ids.contains($0.calendarIdentifier) } ?? false }
        return missing.filter { item in
            // A calendar not read now: nothing can be told about its events.
            guard ids.contains(item.calendarId) else { return false }
            var found = store.calendarItems(withExternalIdentifier: item.externalId)
                .compactMap { $0 as? EKEvent }
                .filter(read)
            if let occurrence = item.occurrence, !found.isEmpty {
                // The occurrence at its planned time, when EventKit lists only the series.
                let planned = Date(timeIntervalSince1970: TimeInterval(occurrence) / 1000)
                let predicate = store.predicateForEvents(
                    withStart: planned.addingTimeInterval(-86_400), end: planned.addingTimeInterval(86_400),
                    calendars: calendars.filter { $0.calendarIdentifier == item.calendarId }
                )
                found += store.events(matching: predicate).filter { $0.calendarItemExternalIdentifier == item.externalId }
            }
            return EventRecurrence.isGone(item, found: found.map(EventRecurrence.init))
        }.map(\.key)
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
            occurrence: EventRecurrence(event).occurrence,
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
