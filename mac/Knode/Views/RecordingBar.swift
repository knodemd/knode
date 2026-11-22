import AppKit
import SwiftUI

/// The bar across the top of every Knode window while a meeting records: the red dot, where it
/// goes, the time, Pause and Stop. It does not depend on the menu bar item, which macOS hides
/// when the menu bar is full.
struct RecordingBar: View {
    @Environment(RecordingController.self) private var controller
    @Environment(\.openWindow) private var openWindow
    @Environment(\.kTheme) private var theme

    var body: some View {
        if case .recording(_, let target) = controller.phase {
            let state = controller.commandState
            VStack(spacing: 0) {
                HStack(spacing: KSpace.x2) {
                    RecordingDot(paused: state.isPaused)
                    Text(state.isPaused ? "Recording paused" : "Recording").font(KFont.ui(13, .semibold))
                    Text(RecordingText.elapsed(controller.elapsedSeconds)).font(KFont.mono(12)).foregroundStyle(theme.muted)
                    Text([target.spaceName, target.projectTitle].compactMap { $0 }.joined(separator: " / "))
                        .font(KFont.ui(12)).foregroundStyle(theme.muted).lineLimit(1)
                    if let notice = RecordingStatusText.live(controller) {
                        Text(notice).font(KFont.ui(11)).foregroundStyle(theme.muted).lineLimit(1).kTooltip(notice)
                    }
                    Spacer(minLength: KSpace.x2)
                    KButton(state.isPaused ? "Resume" : "Pause", icon: state.isPaused ? .play : .pause, size: .small) {
                        state.isPaused ? controller.resume() : controller.pause()
                    }
                    KButton("Stop", icon: .stop, kind: .primary, size: .small) {
                        controller.stop()
                        RecordingWindow.show(openWindow)
                    }
                    .keyboardShortcut(".", modifiers: .command)
                    .kTooltip("Stop and save (Command-Period)")
                }
                .padding(.horizontal, KSpace.x4)
                .frame(height: 36)
                .background(theme.dangerFill)
                KDivider()
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Recording, \(RecordingText.elapsed(controller.elapsedSeconds))")
        }
    }
}

/// Record in the Knode window: the same two ways to start as the Record menu.
struct RecordButton: View {
    @Environment(RecordingController.self) private var controller
    @Environment(\.openWindow) private var openWindow
    @Environment(\.kTheme) private var theme

    var body: some View {
        let state = controller.commandState
        // While recording, the recording bar below takes over.
        if !state.isRecording {
            menu(state)
        }
    }

    private func menu(_ state: RecordingCommandState) -> some View {
        KMenuButton(entries: {
            [
                .action("Record Meeting…", icon: .people, shortcut: [.shift, .command, .char("R")]) { begin(.online(.allApps)) },
                .action("Record In Person…", icon: .chat, shortcut: [.option, .shift, .command, .char("R")]) { begin(.inPerson) },
            ]
        }) {
            HStack(spacing: 6) {
                KIcon(.record, size: .sm, color: state.canStart ? theme.danger : theme.muted)
                Text("Record")
            }
        }
        .disabled(!state.canStart)
        .kTooltip(state.isProcessing ? "Saving the last recording" : "Record a meeting or a conversation")
        .accessibilityLabel("Record")
    }

    private func begin(_ mode: RecordingMode) {
        controller.begin(mode)
        RecordingWindow.show(openWindow)
    }
}
