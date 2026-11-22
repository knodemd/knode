import Foundation

/// Runs the calendar source for every space it is connected to: Knode Service lists it and keeps
/// its settings and state, but only the app can read EventKit (`PROTOCOL.md`, "Calendar").
///
/// A run asks each space for its plan (`calendar.plan`: on, the days, the calendars excluded),
/// reads those calendars and days, and hands the events to `calendar.sync`, which writes them, in
/// batches of at most `batchDays` days so a message stays small. The events the core finds
/// missing are looked up in EventKit, and only those gone from their calendar (not those moved
/// outside the days read) are marked, with `calendar.remove`.
/// The calendars of a space are read again when the service asks (its `next_run` came, or the
/// person chose Run now or changed the days), when a calendar of the Mac changed, or when the plan
/// changed (a calendar excluded or included); what was read is handed over only when it differs
/// from what was handed over last, or when the service asked. Without calendar access, each space
/// is told why once (`calendar.fail`), so Settings > Sources says what to do.
actor CalendarSyncer {
    /// What was last handed to a space: the cursor that keeps a run from writing the same again.
    struct Handed: Equatable, Sendable {
        var plan: CalendarPlan
        var fingerprint: Int
        var at: Date
    }

    /// The most days handed over in one `calendar.sync`.
    static let batchDays: Int64 = 31

    private let store: any CalendarEventStore
    private let client: any CalendarClient
    private let now: @Sendable () -> Date
    private var handed: [URL: Handed] = [:]
    /// A calendar changed since the spaces were last read.
    private var calendarsChanged = true
    private var deniedReported: Set<URL> = []
    private var loop: Task<Void, Never>?
    private var observer: Task<Void, Never>?

    init(store: any CalendarEventStore, client: any CalendarClient, now: @escaping @Sendable () -> Date = { Date() }) {
        self.store = store
        self.client = client
        self.now = now
    }

    /// What was handed to `space` last, for tests.
    func lastHanded(to space: URL) -> Handed? { handed[space] }

    /// Says a calendar of the Mac changed: the next run reads every space again.
    func calendarChanged() { calendarsChanged = true }

    /// Runs every `interval`, and soon after a calendar changes, until `stop()`.
    func start(every interval: Duration = .seconds(30)) {
        guard loop == nil else { return }
        loop = Task { [weak self] in
            while !Task.isCancelled {
                await self?.observeChanges()
                await self?.runOnce()
                try? await Task.sleep(for: interval)
            }
        }
    }

    func stop() {
        loop?.cancel()
        loop = nil
        observer?.cancel()
        observer = nil
    }

    /// Follows the changes of the calendars once access is allowed, which may come later.
    private func observeChanges() {
        guard observer == nil, store.access == .fullAccess else { return }
        let changes = store.changes()
        observer = Task { [weak self] in
            for await _ in changes { await self?.calendarChanged() }
        }
    }

    /// One run over every space the calendar is connected to. Errors of one space never stop
    /// the others; a service that is not running ends the run quietly.
    func runOnce() async {
        let spaces: [URL]
        do {
            spaces = try await client.connectedSpaces()
        } catch {
            return
        }
        handed = handed.filter { spaces.contains($0.key) }
        guard !spaces.isEmpty else { return }
        guard store.access == .fullAccess else {
            for space in spaces where !deniedReported.contains(space) {
                let message = store.access == .restricted
                    ? "Calendar access is restricted on this Mac"
                    : "Knode may not read your calendars: allow it in System Settings > Privacy & Security > Calendars"
                if (try? await client.fail(kind: "permission", message: message, in: space)) != nil {
                    deniedReported.insert(space)
                }
            }
            return
        }
        deniedReported = []
        let calendars = store.calendars()
        try? await client.reportCalendars(calendars)
        let changed = calendarsChanged
        calendarsChanged = false
        var failed = false
        for space in spaces {
            do {
                try await run(space, calendars: calendars, calendarsChanged: changed)
            } catch {
                failed = true
            }
        }
        if failed { calendarsChanged = true }
    }

    private func run(_ space: URL, calendars: [CalendarInfo], calendarsChanged: Bool) async throws {
        let plan = try await client.plan(in: space)
        guard plan.on else { return }
        let date = now()
        let asked = plan.nextRun.map { $0 <= CalendarWire.milliseconds(date) } ?? true
        let last = handed[space]
        let planChanged = last.map { $0.plan.start != plan.start || $0.plan.end != plan.end || $0.plan.exclude != plan.exclude || $0.plan.attendeeEmails != plan.attendeeEmails } ?? true
        guard asked || calendarsChanged || planChanged else { return }
        let excluded = Set(plan.exclude)
        let read = calendars.filter { !excluded.contains($0.id) }
        let ids = Set(read.map(\.id))
        var hasher = Hasher()
        hasher.combine(read)
        let batches = Self.batches(from: plan.start, to: plan.end).map { batch in
            let events = store.events(from: Self.date(batch.start), to: Self.date(batch.end), calendars: ids)
                .sorted { ($0.start, $0.externalID) < ($1.start, $1.externalID) }
            hasher.combine(events)
            return (batch, events)
        }
        let fingerprint = hasher.finalize()
        if !asked, !planChanged, last?.fingerprint == fingerprint { return }
        for (batch, events) in batches {
            let result = try await client.sync(events, calendars: read, from: batch.start, to: batch.end, in: space)
            let gone = store.gone(result.missing, calendars: ids)
            if !gone.isEmpty { try await client.remove(gone, in: space) }
        }
        handed[space] = Handed(plan: plan, fingerprint: fingerprint, at: date)
    }

    /// The days from `start` to `end` (Unix milliseconds) in runs of at most `batchDays`; one,
    /// even empty, when `end` is not after `start`.
    static func batches(from start: Int64, to end: Int64) -> [(start: Int64, end: Int64)] {
        let step = batchDays * 86_400_000
        guard end > start else { return [(start, end)] }
        var out: [(start: Int64, end: Int64)] = []
        var from = start
        while from < end {
            let to = min(end, from + step)
            out.append((from, to))
            from = to
        }
        return out
    }

    private static func date(_ milliseconds: Int64) -> Date { Date(timeIntervalSince1970: TimeInterval(milliseconds) / 1000) }
}

/// Connecting the calendar asks for calendar access, as the sign-in step of the Connect sheet:
/// never before. Once allowed, the calendars of the Mac are reported to Knode Service, so the
/// sheet can list them to leave some out.
struct CalendarAccessSignIn: SourceSignIn {
    enum Failure: LocalizedError {
        case denied

        var errorDescription: String? {
            "Knode may not read your calendars. Allow it in System Settings > Privacy & Security > Calendars, then check again."
        }
    }

    let store: any CalendarEventStore
    let client: any CalendarClient
    var openURL: @Sendable (URL) -> Void = { _ in }

    /// Privacy & Security > Calendars in System Settings.
    static let settingsURL = URL(string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Calendars")!

    func start(source: String, space: URL) async throws -> SourceSignInStep? {
        switch store.access {
        case .fullAccess:
            try await report()
            return nil
        case .notDetermined:
            return SourceSignInStep(
                id: "calendar-access", title: "Allow calendar access",
                message: "Knode reads the events of the calendars you keep, on this Mac, and writes them into this space. It never changes your calendars and sends nothing anywhere.",
                continueTitle: "Allow Access"
            )
        case .denied, .restricted:
            return deniedStep
        }
    }

    func answer(_ step: SourceSignInStep, with values: [String: String], source: String, space: URL) async throws -> SourceSignInStep? {
        if store.access == .notDetermined {
            _ = try await store.requestFullAccess()
        }
        guard store.access == .fullAccess else {
            if step.id == "calendar-denied" { openURL(Self.settingsURL) }
            throw Failure.denied
        }
        try await report()
        return nil
    }

    private var deniedStep: SourceSignInStep {
        SourceSignInStep(
            id: "calendar-denied", title: "Calendar access is off",
            message: "Allow Knode in System Settings > Privacy & Security > Calendars, then check again.",
            continueTitle: "Check Again"
        )
    }

    private func report() async throws {
        try await client.reportCalendars(store.calendars())
    }
}
