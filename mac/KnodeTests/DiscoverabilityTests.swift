import AppKit
import Foundation
import Testing
@testable import Knode

/// The Dock badge, in memory.
@MainActor
final class FakeDockBadge: DockBadge {
    var badgeLabel: String?
}

@MainActor
struct RecordingCommandsTests {
    private func controller(_ store: FakeRecordingsBackend, recorder: FakeRecorder = FakeRecorder()) -> RecordingController {
        let url = store.spaceURL
        let processor = RecordingsProcessor(store: store, transcriber: RecordingTranscriber(), summarizer: RecordingSummarizer(), spaceURLs: { [url] })
        return RecordingController(recorder: recorder, sink: StoreRecordingSink(store: store), processor: processor,
                                   consent: MemoryRecordingConsentStore(hasAcknowledged: true), unsaved: store.unsaved)
    }

    private func target(_ store: FakeRecordingsBackend) -> RecordingTarget {
        RecordingTarget(spaceURL: store.spaceURL, spaceName: "Work")
    }

    @Test func startsWhenNothingRecordsAndStopsOrPausesOnlyWhileRecording() {
        let target = RecordingTarget(spaceURL: URL(filePath: "/tmp/Work.knode"), spaceName: "Work")
        let idle = RecordingCommandState(phase: .idle, isPaused: false)
        #expect(idle.canStart && !idle.canStop && !idle.canPauseOrResume)
        #expect(idle.primaryTitle == "Record Meeting…")
        #expect(idle.dockBadge == nil)

        for open in [RecordingController.Phase.consentNeeded(.inPerson), .choosingTarget(.inPerson), .done(note: nil), .failed("x")] {
            #expect(RecordingCommandState(phase: open, isPaused: false).canStart)
        }

        let recording = RecordingCommandState(phase: .recording(.inPerson, target), isPaused: false)
        #expect(!recording.canStart && recording.canStop && recording.canPauseOrResume)
        #expect(recording.primaryTitle == "Stop Recording")
        #expect(recording.pauseTitle == "Pause Recording")
        #expect(recording.dockBadge == "REC")

        let paused = RecordingCommandState(phase: .recording(.inPerson, target), isPaused: true)
        #expect(paused.pauseTitle == "Resume Recording")
        #expect(paused.dockBadge == "Paused")

        for busy in [RecordingController.Phase.saving, .transcribing(progress: 0.5), .summarising(progress: 0.1)] {
            let state = RecordingCommandState(phase: busy, isPaused: true)
            #expect(!state.canStart && !state.canStop && !state.isPaused && state.dockBadge == nil)
        }
    }

    @Test func beginReplacesARequestLeftOpen() throws {
        let store = try FakeRecordingsBackend()
        let controller = controller(store)
        controller.begin(.inPerson)
        #expect(controller.phase == .choosingTarget(.inPerson))
        controller.begin(.online(.allApps))
        #expect(controller.phase == .choosingTarget(.online(.allApps)))
    }

    @Test func beginNeverInterruptsARecording() async throws {
        let store = try FakeRecordingsBackend()
        let controller = controller(store)
        controller.begin(.inPerson)
        await controller.start(target: target(store))
        controller.begin(.online(.allApps))
        #expect(controller.isRecording)
        controller.discard()
    }

    @Test func dockBadgeFollowsTheRecording() async throws {
        let store = try FakeRecordingsBackend()
        let controller = controller(store)
        let dock = FakeDockBadge()
        let indicator = RecordingDockIndicator(controller: controller, dock: dock)
        #expect(dock.badgeLabel == nil)

        controller.begin(.inPerson)
        await controller.start(target: target(store))
        try await waitFor { dock.badgeLabel == "REC" }
        controller.pause()
        try await waitFor { dock.badgeLabel == "Paused" }
        controller.resume()
        try await waitFor { dock.badgeLabel == "REC" }
        controller.stop()
        try await waitFor { dock.badgeLabel == nil }
        await controller.waitUntilProcessed()
        #expect(dock.badgeLabel == nil)
        _ = indicator
    }

    private func waitFor(_ condition: () -> Bool) async throws {
        for _ in 0..<200 where !condition() {
            try await Task.sleep(for: .milliseconds(5))
        }
        #expect(condition())
    }
}

struct SourceAppsTests {
    @Test func mapsSourcesToTheBundleIDsOfTheirApps() {
        #expect(SourceApps.bundleIDs(for: "apple-mail") == ["com.apple.mail"])
        #expect(SourceApps.bundleIDs(for: "messages") == ["com.apple.MobileSMS"])
        #expect(SourceApps.bundleIDs(for: "voice-memos") == ["com.apple.VoiceMemos"])
        #expect(SourceApps.bundleIDs(for: "calendar") == ["com.apple.iCal"])
        #expect(SourceApps.bundleIDs(for: "slack") == ["com.tinyspeck.slackmacgap"])
        #expect(SourceApps.bundleIDs(for: "apple-notes") == ["com.apple.Notes"])
        #expect(SourceApps.bundleIDs(for: "bear") == ["net.shinyfrog.bear"])
    }

    @Test func variantsTakeTheAppOfTheirPrefix() {
        #expect(SourceApps.bundleIDs(for: "whatsapp-mac") == ["net.whatsapp.WhatsApp", "desktop.WhatsApp"])
        #expect(SourceApps.bundleIDs(for: "telegram-chat") == ["ru.keepcoder.Telegram", "org.telegram.desktop"])
        #expect(SourceApps.bundleIDs(for: "photos-library") == ["com.apple.Photos"])
        #expect(SourceApps.bundleIDs(for: "Slack") == ["com.tinyspeck.slackmacgap"])
    }

    @Test func sourcesWithoutAnAppFallBackToAGlyph() {
        #expect(SourceApps.bundleIDs(for: "gmail").isEmpty)
        #expect(SourceApps.bundleIDs(for: "imap").isEmpty)
        #expect(SourceApps.bundleIDs(for: "slackish").isEmpty)
        #expect(SourceApps.glyph(for: "gmail") == .mail)
        #expect(SourceApps.glyph(for: "imap") == .mail)
        #expect(SourceApps.glyph(for: "calendar") == .calendar)
        #expect(SourceApps.glyph(for: "telegram") == .chat)
        #expect(SourceApps.glyph(for: "something-else") == .import)
    }

    @Test func findsTheFirstInstalledApp() {
        let installed = ["desktop.WhatsApp": URL(filePath: "/Applications/WhatsApp.app")]
        #expect(SourceApps.appURL(for: "whatsapp") { installed[$0] } == URL(filePath: "/Applications/WhatsApp.app"))
        #expect(SourceApps.appURL(for: "telegram") { installed[$0] } == nil)
        #expect(SourceApps.appURL(for: "gmail") { _ in URL(filePath: "/x") } == nil)
    }
}

struct SettingsTabTests {
    @Test func selectsASectionAndRemembersIt() throws {
        let suite = "knode-settings-tab-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }

        #expect(SettingsTab.selected(in: defaults) == .sources)
        SettingsTab.select(.agents, in: defaults)
        #expect(SettingsTab.selected(in: defaults) == .agents)
        #expect(defaults.string(forKey: SettingsTab.storageKey) == "agents")
        defaults.set("accounts", forKey: SettingsTab.storageKey)
        #expect(SettingsTab.selected(in: defaults) == .sources)
    }

    @Test func listsSourcesFirstAndEverySectionOnce() {
        #expect(SettingsTab.allCases.first == .sources)
        #expect(Set(SettingsTab.allCases.map(\.title)).count == SettingsTab.allCases.count)
        #expect(SettingsTab.allCases.allSatisfy { !$0.subtitle.isEmpty })
    }
}
