import AppKit
import Observation
import SwiftUI

/// What the Record menu, the Record button and the indicators offer for a phase of the recording
/// controller, apart from any view so it can be tested.
struct RecordingCommandState: Equatable {
    var isRecording: Bool
    var isPaused: Bool
    var isProcessing: Bool

    init(phase: RecordingController.Phase, isPaused: Bool) {
        if case .recording = phase { isRecording = true } else { isRecording = false }
        switch phase {
        case .saving, .transcribing, .summarising: isProcessing = true
        default: isProcessing = false
        }
        self.isPaused = isRecording && isPaused
    }

    /// A new recording can be asked for unless one runs or is being saved; a request left open
    /// in the recording window gives way to the new one.
    var canStart: Bool { !isRecording && !isProcessing }
    var canStop: Bool { isRecording }
    var canPauseOrResume: Bool { isRecording }
    var pauseTitle: String { isPaused ? "Resume Recording" : "Pause Recording" }

    /// The first item of the Record menu, Shift-Command-R: it starts a meeting, or stops the one
    /// that runs.
    var primaryTitle: String { isRecording ? "Stop Recording" : "Record Meeting…" }

    /// The badge on the Dock icon for as long as a recording runs, paused or not.
    var dockBadge: String? {
        guard isRecording else { return nil }
        return isPaused ? "Paused" : "REC"
    }
}

extension RecordingController {
    var commandState: RecordingCommandState { RecordingCommandState(phase: phase, isPaused: isPaused) }

    /// Asks for a new recording from a menu or a button. A request left open, or the result of
    /// the last recording, gives way to it.
    func begin(_ mode: RecordingMode) {
        guard commandState.canStart else { return }
        cancelRequest()
        dismiss()
        request(mode)
    }
}

extension RecordingWindow {
    /// Brings the recording window forward: consent, where it goes, the recording, its minutes.
    @MainActor static func show(_ openWindow: OpenWindowAction) {
        openWindow(id: id)
        NSApp.activate()
    }
}

/// The badge on the app's Dock icon.
@MainActor
protocol DockBadge: AnyObject {
    var badgeLabel: String? { get set }
}

@MainActor
final class AppDockBadge: DockBadge {
    var badgeLabel: String? {
        get { NSApplication.shared.dockTile.badgeLabel }
        set { NSApplication.shared.dockTile.badgeLabel = newValue }
    }
}

/// Keeps the Dock badge in step with the recording controller, so a recording shows even when
/// the menu bar item is hidden and no Knode window is open.
@MainActor
final class RecordingDockIndicator {
    private let controller: RecordingController
    private let dock: any DockBadge
    private var shown: String?

    init(controller: RecordingController, dock: any DockBadge) {
        self.controller = controller
        self.dock = dock
        update()
    }

    private func update() {
        let label = withObservationTracking {
            controller.commandState.dockBadge
        } onChange: { [weak self] in
            Task { @MainActor in self?.update() }
        }
        guard label != shown else { return }
        shown = label
        dock.badgeLabel = label
    }
}
