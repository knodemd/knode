import Foundation
import Synchronization
import Testing
@testable import Knode

/// A calendar store in memory: tests never touch EventKit or ask for access.
final class FakeCalendarStore: CalendarEventStore {
    struct State {
        var access = CalendarAccess.fullAccess
        var grant = true
        var requests = 0
        var calendars: [CalendarInfo] = []
        var events: [CalendarRecord] = []
        var reads: [(start: Date, end: Date, calendars: Set<String>)] = []
    }

    let state = Mutex(State())

    var access: CalendarAccess { state.withLock { $0.access } }

    func requestFullAccess() async throws -> Bool {
        state.withLock {
            $0.requests += 1
            $0.access = $0.grant ? .fullAccess : .denied
            return $0.grant
        }
    }

    func calendars() -> [CalendarInfo] { state.withLock { $0.calendars } }

    func events(from start: Date, to end: Date, calendars ids: Set<String>) -> [CalendarRecord] {
        state.withLock { s in
            s.reads.append((start, end, ids))
            return s.events.filter { ids.contains($0.calendarID) && $0.end >= start && $0.start <= end }
        }
    }

    /// Gone: no event of that id is left in the calendar, on any day.
    func gone(_ missing: [CalendarMissing], calendars ids: Set<String>) -> [String] {
        state.withLock { s in
            missing.filter { item in !s.events.contains { $0.externalID == item.externalId } }.map(\.key)
        }
    }

    func changes() -> AsyncStream<Void> { AsyncStream { $0.finish() } }
}

/// Knode Service's calendar methods in memory.
actor FakeCalendarClient: CalendarClient {
    struct Synced: Sendable {
        var space: URL
        var events: [CalendarRecord]
        var calendars: [CalendarInfo]
        var start: Int64
        var end: Int64
    }

    var spaces: [URL] = []
    var plans: [URL: CalendarPlan] = [:]
    var synced: [Synced] = []
    var removed: [[String]] = []
    /// What the next `sync` answers as missing.
    var missing: [CalendarMissing] = []
    var reported: [[CalendarInfo]] = []
    var failures: [(URL, String)] = []
    var links: [(recording: Node.ID, event: Node.ID, project: Node.ID?)] = []
    var current: CalendarEventSummary?

    func set(spaces: [URL], plan: CalendarPlan) {
        self.spaces = spaces
        for space in spaces { plans[space] = plan }
    }

    func setPlan(_ plan: CalendarPlan, for space: URL) { plans[space] = plan }
    func setCurrent(_ event: CalendarEventSummary?) { current = event }
    func setMissing(_ missing: [CalendarMissing]) { self.missing = missing }

    func connectedSpaces() async throws -> [URL] { spaces }

    func plan(in space: URL) async throws -> CalendarPlan {
        guard let plan = plans[space] else { throw ServiceError.remote(code: -32004, kind: "invalid", message: "not connected") }
        return plan
    }

    func reportCalendars(_ calendars: [CalendarInfo]) async throws { reported.append(calendars) }

    func sync(_ events: [CalendarRecord], calendars: [CalendarInfo], from start: Int64, to end: Int64, in space: URL) async throws -> CalendarSyncResult {
        synced.append(Synced(space: space, events: events, calendars: calendars, start: start, end: end))
        // As the service does: a good run is due again later.
        plans[space]?.nextRun = start + 10 * 86_400_000_000
        defer { missing = [] }
        return CalendarSyncResult(
            written: .init(created: events.count, updated: 0),
            missing: missing,
            source: SourceStatus(id: "calendar", name: "Calendar", kind: "connector", reads: "", state: .idle)
        )
    }

    func remove(_ keys: [String], in space: URL) async throws { removed.append(keys) }

    func fail(kind: String, message: String, in space: URL) async throws { failures.append((space, kind)) }

    func options(in space: URL) async throws -> CalendarOptions { .standard }

    func configure(_ options: CalendarOptions, in space: URL) async throws -> CalendarOptions { options }

    func currentEvent(at date: Date, in space: URL) async throws -> CalendarEventSummary? { current }

    func link(_ recording: Node.ID, to event: Node.ID, project: Node.ID?, in space: URL) async throws {
        links.append((recording, event, project))
    }
}

extension CalendarRecord {
    static func sample(_ id: String, calendar: String = "work", start: Date, occurrence: Date? = nil) -> CalendarRecord {
        CalendarRecord(
            externalID: id, occurrence: occurrence, calendarID: calendar, calendarName: calendar.capitalized, title: "Event \(id)",
            start: start, end: start.addingTimeInterval(3600), isAllDay: false, location: nil, url: nil, notes: nil,
            attendees: [], status: .confirmed
        )
    }
}

struct CalendarTests {
    let now = Date(timeIntervalSince1970: 1_795_510_800)
    let space = URL(filePath: "/tmp/Work.knode", directoryHint: .isDirectory)

    func plan(on: Bool = true, exclude: [String] = [], nextRun: Int64? = nil) -> CalendarPlan {
        let ms = CalendarWire.milliseconds(now)
        return CalendarPlan(on: on, start: ms - 86_400_000, end: ms + 14 * 86_400_000, exclude: exclude, attendeeEmails: false, nextRun: nextRun)
    }

    @Test func anEventGoesToTheCoreAsItsJSON() {
        var record = CalendarRecord.sample("E1", start: now, occurrence: now)
        record.url = "https://zoom.us/j/1"
        record.notes = ""
        record.attendees = [.init(name: "Grace", email: "grace@example.com")]
        record.status = .cancelled
        guard case .object(let json) = CalendarWire.event(record) else { Issue.record("not an object"); return }
        #expect(json["external_id"] == "E1")
        #expect(json["occurrence"] == .int(1_795_510_800_000))
        #expect(json["start"] == .int(1_795_510_800_000))
        #expect(json["end"] == .int(1_795_514_400_000))
        #expect(json["calendar_id"] == "work")
        #expect(json["status"] == "cancelled")
        #expect(json["url"] == "https://zoom.us/j/1")
        #expect(json["notes"] == nil)
        #expect(json["location"] == nil)
        #expect(json["attendees"] == [["name": "Grace", "email": "grace@example.com"]])
    }

    @Test func aRunReadsTheIncludedCalendarsAndHandsOverOnlyWhatChanged() async throws {
        let store = FakeCalendarStore()
        store.state.withLock {
            $0.calendars = [CalendarInfo(id: "work", name: "Work"), CalendarInfo(id: "home", name: "Home")]
            $0.events = [.sample("W1", start: now), .sample("H1", calendar: "home", start: now)]
        }
        let client = FakeCalendarClient()
        await client.set(spaces: [space], plan: plan(exclude: ["home"]))
        let clock = Mutex(now)
        let syncer = CalendarSyncer(store: store, client: client, now: { clock.withLock { $0 } })

        await syncer.runOnce()
        var synced = await client.synced
        #expect(synced.count == 1)
        #expect(synced[0].events.map(\.externalID) == ["W1"])
        // Only the calendars read go with the events; the Mac's whole list is reported apart.
        #expect(synced[0].calendars == [CalendarInfo(id: "work", name: "Work")])
        #expect(store.state.withLock { $0.reads.last?.calendars } == ["work"])
        #expect(await client.reported.last?.count == 2)

        // Nothing changed and the service does not ask yet: nothing is read or handed over.
        let reads = store.state.withLock { $0.reads.count }
        await syncer.runOnce()
        #expect(store.state.withLock { $0.reads.count } == reads)
        #expect(await client.synced.count == 1)

        // A calendar changed, but not these events: read, not handed over again.
        await syncer.calendarChanged()
        await syncer.runOnce()
        #expect(store.state.withLock { $0.reads.count } == reads + 1)
        #expect(await client.synced.count == 1)

        // An event moved: handed over.
        store.state.withLock { $0.events[0].start = now.addingTimeInterval(1800) }
        await syncer.calendarChanged()
        await syncer.runOnce()
        synced = await client.synced
        #expect(synced.count == 2)
        #expect(synced[1].events.first?.start == now.addingTimeInterval(1800))

        // The service asks again (Run now): handed over even unchanged.
        await client.setPlan(plan(exclude: ["home"], nextRun: CalendarWire.milliseconds(now) - 1), for: space)
        await syncer.runOnce()
        #expect(await client.synced.count == 3)

        // Home included again: the plan changed, so it is read and handed over.
        await client.setPlan(plan(nextRun: CalendarWire.milliseconds(now) + 3_600_000), for: space)
        await syncer.runOnce()
        synced = await client.synced
        #expect(synced.count == 4)
        #expect(Set(synced[3].events.map(\.externalID)) == ["W1", "H1"])
        #expect(await syncer.lastHanded(to: space)?.plan.exclude == [])
    }

    @Test func aDetachedOccurrenceKeepsItsOccurrence() {
        let planned = now
        let detached = EventRecurrence(isDetached: true, hasRecurrenceRules: false, occurrenceDate: planned)
        #expect(detached.occurrence == planned)
        #expect(EventRecurrence(isDetached: false, hasRecurrenceRules: true, occurrenceDate: planned).occurrence == planned)
        #expect(EventRecurrence(isDetached: false, hasRecurrenceRules: false, occurrenceDate: planned).occurrence == nil)
    }

    @Test func aMissingEventIsGoneOnlyWhenItsCalendarNoLongerHasIt() {
        let single = CalendarMissing(key: "E1", externalId: "E1", occurrence: nil, calendarId: "work")
        let series = EventRecurrence(isDetached: false, hasRecurrenceRules: true, occurrenceDate: now.addingTimeInterval(-7 * 86_400))
        #expect(EventRecurrence.isGone(single, found: []))
        // Moved outside the days read: still there.
        #expect(!EventRecurrence.isGone(single, found: [EventRecurrence(isDetached: false, hasRecurrenceRules: false, occurrenceDate: now)]))
        let ms = CalendarWire.milliseconds(now)
        let occurrence = CalendarMissing(key: "S@\(ms)", externalId: "S", occurrence: ms, calendarId: "work")
        #expect(EventRecurrence.isGone(occurrence, found: []))
        // The series is there, the occurrence was deleted.
        #expect(EventRecurrence.isGone(occurrence, found: [series]))
        // The occurrence was moved on its own, outside the days read: detached, still planned then.
        let moved = EventRecurrence(isDetached: true, hasRecurrenceRules: false, occurrenceDate: now)
        #expect(!EventRecurrence.isGone(occurrence, found: [series, moved]))
    }

    @Test func longNotesAreCutBeforeTheyGo() {
        var record = CalendarRecord.sample("E1", start: now)
        record.notes = "  " + String(repeating: "a", count: 5000)
        guard case .object(let json) = CalendarWire.event(record), case .string(let notes) = json["notes"] else {
            Issue.record("no notes"); return
        }
        #expect(notes.count == CalendarWire.maxNotes)
        #expect(notes.first == "a")
        record.notes = " \n "
        guard case .object(let blank) = CalendarWire.event(record) else { Issue.record("not an object"); return }
        #expect(blank["notes"] == nil)
    }

    @Test func aLongWindowGoesInBatchesAndOnlyGoneEventsAreRemoved() async throws {
        let day: Int64 = 86_400_000
        let batches = CalendarSyncer.batches(from: 0, to: 70 * day)
        #expect(batches.map(\.start) == [0, 31 * day, 62 * day])
        #expect(batches.map(\.end) == [31 * day, 62 * day, 70 * day])
        #expect(CalendarSyncer.batches(from: 5, to: 5).count == 1)

        let store = FakeCalendarStore()
        let ms = CalendarWire.milliseconds(now)
        store.state.withLock {
            $0.calendars = [CalendarInfo(id: "work", name: "Work")]
            $0.events = [
                .sample("EARLY", start: now.addingTimeInterval(-50 * 86_400)),
                .sample("LATE", start: now.addingTimeInterval(5 * 86_400)),
                // Moved far ahead, outside the days read.
                .sample("MOVED", start: now.addingTimeInterval(300 * 86_400)),
            ]
        }
        let client = FakeCalendarClient()
        await client.set(spaces: [space], plan: CalendarPlan(on: true, start: ms - 60 * day, end: ms + 15 * day, exclude: [], attendeeEmails: false, nextRun: nil))
        await client.setMissing([
            CalendarMissing(key: "MOVED", externalId: "MOVED", occurrence: nil, calendarId: "work"),
            CalendarMissing(key: "GONE", externalId: "GONE", occurrence: nil, calendarId: "work"),
        ])
        let syncer = CalendarSyncer(store: store, client: client, now: { [now] in now })
        await syncer.runOnce()
        let synced = await client.synced
        #expect(synced.count == 3)
        #expect(synced.map(\.start) == [ms - 60 * day, ms - 29 * day, ms + 2 * day])
        #expect(synced.last?.end == ms + 15 * day)
        #expect(synced.map { $0.events.map(\.externalID) } == [["EARLY"], [], ["LATE"]])
        #expect(await client.removed == [["GONE"]])
    }

    @Test func aPausedSpaceIsNotRead() async throws {
        let store = FakeCalendarStore()
        store.state.withLock { $0.calendars = [CalendarInfo(id: "work", name: "Work")]; $0.events = [.sample("W1", start: now)] }
        let client = FakeCalendarClient()
        await client.set(spaces: [space], plan: plan(on: false))
        let syncer = CalendarSyncer(store: store, client: client, now: { [now] in now })
        await syncer.runOnce()
        #expect(await client.synced.isEmpty)
        #expect(store.state.withLock { $0.reads.isEmpty })
    }

    @Test func withoutAccessEachSpaceIsToldOnce() async throws {
        let store = FakeCalendarStore()
        store.state.withLock { $0.access = .denied }
        let client = FakeCalendarClient()
        await client.set(spaces: [space], plan: plan())
        let syncer = CalendarSyncer(store: store, client: client, now: { [now] in now })
        await syncer.runOnce()
        await syncer.runOnce()
        let failures = await client.failures
        #expect(failures.count == 1)
        #expect(failures.first?.1 == "permission")
        #expect(await client.synced.isEmpty)
        // Access is never asked for by a run: only when the person connects.
        #expect(store.state.withLock { $0.requests } == 0)
    }

    @Test func connectingAsksForAccessAndReportsTheCalendars() async throws {
        let store = FakeCalendarStore()
        store.state.withLock { $0.access = .notDetermined; $0.calendars = [CalendarInfo(id: "work", name: "Work")] }
        let client = FakeCalendarClient()
        let signIn = CalendarAccessSignIn(store: store, client: client)
        let step = try #require(try await signIn.start(source: "calendar", space: space))
        #expect(step.fields.isEmpty)
        #expect(store.state.withLock { $0.requests } == 0)
        let next = try await signIn.answer(step, with: [:], source: "calendar", space: space)
        #expect(next == nil)
        #expect(store.state.withLock { $0.requests } == 1)
        #expect(await client.reported.last == [CalendarInfo(id: "work", name: "Work")])

        let denied = FakeCalendarStore()
        denied.state.withLock { $0.access = .notDetermined; $0.grant = false }
        let refused = CalendarAccessSignIn(store: denied, client: client)
        let first = try #require(try await refused.start(source: "calendar", space: space))
        await #expect(throws: CalendarAccessSignIn.Failure.self) {
            _ = try await refused.answer(first, with: [:], source: "calendar", space: space)
        }
        #expect(try await refused.start(source: "calendar", space: space)?.id == "calendar-denied")
    }

    @MainActor @Test func aRecordingStartedDuringAnEventTakesItsTitleAndIsLinkedToIt() async throws {
        let store = try FakeRecordingsBackend()
        let client = FakeCalendarClient()
        let event = CalendarEventSummary(id: "event-1", title: "Weekly sync", start: 0, end: 1, callLink: "https://zoom.us/j/1", project: nil)
        await client.setCurrent(event)
        // What the recorder offers in the chosen space.
        let offered = try await client.currentEvent(at: now, in: store.spaceURL)
        #expect(offered == event)

        let consent = MemoryRecordingConsentStore(hasAcknowledged: true)
        let url = store.spaceURL
        let processor = RecordingsProcessor(store: store, transcriber: RecordingTranscriber(), summarizer: RecordingSummarizer(), spaceURLs: { [url] })
        let sink = StoreRecordingSink(store: store, locale: Locale(identifier: "en_GB"), calendar: client)
        let controller = RecordingController(recorder: FakeRecorder(), sink: sink, processor: processor, consent: consent, unsaved: store.unsaved)
        controller.request(.online(.allApps))
        await controller.start(target: RecordingTarget(
            spaceURL: url, spaceName: "Work", projectID: "project-1", projectTitle: "Launch",
            eventID: offered?.id, eventTitle: offered?.title
        ))
        controller.stop()
        await controller.waitUntilProcessed()

        let draft = try #require(await store.created.first)
        #expect(draft.title == "Weekly sync")
        let stored = try #require(await store.recordings.keys.first)
        let links = await client.links
        #expect(links.count == 1)
        #expect(links.first?.recording == stored)
        #expect(links.first?.event == "event-1")
        #expect(links.first?.project == "project-1")
    }

    @Test func anUnsavedRecordingKeepsItsEvent() throws {
        let folder = URL(filePath: "/tmp/unsaved", directoryHint: .isDirectory)
        let recording = FinishedRecording(
            mode: .inPerson,
            target: RecordingTarget(spaceURL: space, spaceName: "Work", projectID: nil, projectTitle: nil, eventID: "event-1", eventTitle: "Weekly sync"),
            startedAt: now, endedAt: now.addingTimeInterval(60), duration: .seconds(60),
            tracks: [RecordedTrack(role: .microphone, url: folder.appending(path: "microphone.m4a"))], key: "k"
        )
        let manifest = UnsavedRecordingStore.Manifest(recording, error: nil)
        let data = try JSONEncoder().encode(manifest)
        let decoded = try JSONDecoder().decode(UnsavedRecordingStore.Manifest.self, from: data)
        let back = try #require(decoded.recording(in: folder))
        #expect(back.target.eventID == "event-1")
        #expect(back.target.eventTitle == "Weekly sync")
    }
}
