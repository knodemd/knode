import SwiftUI

extension EnvironmentValues {
    /// Where the calendar is read and events are linked: Knode Service.
    @Entry var calendarClient: any CalendarClient = ServiceCalendarClient()
}

/// The days the calendar source keeps and whether it writes attendees' addresses, for one space:
/// `calendar.options` and `calendar.configure`. Shown in the calendar's card in Settings > Sources.
struct CalendarOptionsRows: View {
    let space: URL
    @Environment(\.calendarClient) private var client
    @State private var options = CalendarOptions.standard
    @State private var loaded = false
    @State private var error: String?

    private static let back = [7, 14, 30, 90, 365]
    private static let ahead = [7, 14, 30, 90, 365]

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSettingRow("Days back", detail: "With From today, never before the day it was connected.") {
                KDropdown(selection: binding(\.daysBack), options: Self.choices(Self.back, current: options.daysBack))
                    .frame(width: 140)
            }
            KSettingRow("Days ahead") {
                KDropdown(selection: binding(\.daysAhead), options: Self.choices(Self.ahead, current: options.daysAhead))
                    .frame(width: 140)
            }
            KSettingToggle("Attendees' addresses",
                           detail: "Off: attendees are named, never written with their mail address.",
                           isOn: binding(\.attendeeEmails))
            if let error { KErrorText(error) }
        }
        .disabled(!loaded)
        .task(id: space) {
            loaded = false
            do {
                options = try await client.options(in: space)
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
            loaded = true
        }
    }

    private static func choices(_ days: [Int], current: Int) -> [KOption<Int>] {
        (days.contains(current) ? days : (days + [current]).sorted()).map { KOption($0, $0 == 1 ? "1 day" : "\($0) days") }
    }

    private func binding<Value: Equatable>(_ key: WritableKeyPath<CalendarOptions, Value>) -> Binding<Value> {
        Binding(get: { options[keyPath: key] }, set: { value in
            guard options[keyPath: key] != value else { return }
            let before = options
            options[keyPath: key] = value
            let chosen = options
            Task {
                do {
                    options = try await client.configure(chosen, in: space)
                    error = nil
                } catch {
                    options = before
                    self.error = error.localizedDescription
                }
            }
        })
    }
}

/// The event happening now in the chosen space, offered when a meeting starts recording: its title
/// becomes the meeting's, and the recording is linked to it (`calendar.current`, `calendar.link`).
struct RecordingEventRow: View {
    let space: URL?
    @Binding var event: CalendarEventSummary?
    @Environment(\.calendarClient) private var client
    @State private var found: CalendarEventSummary?

    var body: some View {
        Group {
            if let found {
                KSettingToggle("Link to \(found.title)",
                               detail: "Happening now in your calendar. The meeting takes its title and is linked to it.",
                               isOn: Binding(get: { event != nil }, set: { event = $0 ? found : nil }))
            }
        }
        .task(id: space) {
            found = nil
            event = nil
            guard let space else { return }
            let current = try? await client.currentEvent(at: Date(), in: space)
            found = current ?? nil
            event = found
        }
    }
}
