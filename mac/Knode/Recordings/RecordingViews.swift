import AppKit
import SwiftUI

enum RecordingText {
    static func elapsed(_ seconds: Int) -> String {
        let hours = seconds / 3600
        let minutes = seconds / 60 % 60
        let rest = seconds % 60
        return hours > 0 ? String(format: "%d:%02d:%02d", hours, minutes, rest) : String(format: "%d:%02d", minutes, rest)
    }

    static func title(of mode: RecordingMode) -> String {
        switch mode {
        case .online: "Record Meeting"
        case .inPerson: "Record In Person"
        }
    }

    static func explanation(of mode: RecordingMode) -> String {
        switch mode {
        case .online: "Records the sound of this Mac (everyone on the call) and your microphone (you), as two tracks."
        case .inPerson: "Records your microphone: everyone in the room."
        }
    }
}

/// The recording part of the menu bar panel: the two ways to start, or while recording the red
/// indicator, the time, pause and stop.
struct RecordingMenuSection: View {
    let controller: RecordingController
    let processor: RecordingsProcessor
    @Environment(\.openWindow) private var openWindow
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSectionLabel("Meetings")
            switch controller.phase {
            case .recording:
                HStack(spacing: KSpace.x2) {
                    RecordingDot(paused: controller.isPaused)
                    Text(controller.isPaused ? "Paused" : "Recording").font(KFont.ui(13, .semibold))
                    Text(RecordingText.elapsed(controller.elapsedSeconds)).font(KFont.mono(12)).foregroundStyle(theme.muted)
                    Spacer(minLength: KSpace.x2)
                    KIconButton(controller.isPaused ? .forward : .hide, title: controller.isPaused ? "Resume" : "Pause", size: .small) {
                        controller.isPaused ? controller.resume() : controller.pause()
                    }
                    KButton("Stop", icon: .done, kind: .primary, size: .small) {
                        controller.stop()
                        show()
                    }
                }
                .padding(.horizontal, KSpace.x1)
                .accessibilityElement(children: .contain)
                .accessibilityLabel("Recording, \(RecordingText.elapsed(controller.elapsedSeconds))")
                if let notice = RecordingStatusText.live(controller) {
                    Text(notice).font(KFont.ui(11)).foregroundStyle(theme.muted).lineLimit(2)
                        .padding(.horizontal, KSpace.x1)
                }
            case .saving, .transcribing, .summarising:
                KRow(action: show) {
                    KSpinner()
                    Text(RecordingStatusText.processing(controller.phase)).font(KFont.ui(13)).lineLimit(1)
                }
                .padding(.horizontal, -KSpace.x1)
            default:
                VStack(spacing: 1) {
                    KRow(action: { request(.online(.allApps)) }) {
                        KIcon(.people, size: .sm)
                        Text("Record Meeting…").font(KFont.ui(13))
                    }
                    KRow(action: { request(.inPerson) }) {
                        KIcon(.chat, size: .sm)
                        Text("Record In Person…").font(KFont.ui(13))
                    }
                }
                .padding(.horizontal, -KSpace.x1)
            }
            if !controller.isProcessing, let job = processor.current {
                // A recording the app did not just make, such as a voice memo.
                HStack(spacing: KSpace.x2) {
                    KSpinner()
                    Text(RecordingStatusText.job(job)).font(KFont.ui(12)).foregroundStyle(theme.muted).lineLimit(1)
                }
                .padding(.horizontal, KSpace.x1)
                .kTooltip(job.title)
            }
            ForEach(processor.failures) { failure in
                RecordingFailureRow(failure: failure, processor: processor)
            }
            ForEach(controller.unsaved) { item in
                UnsavedRecordingRow(item: item, controller: controller)
            }
        }
    }

    private func request(_ mode: RecordingMode) {
        controller.begin(mode)
        show()
    }

    private func show() {
        RecordingWindow.show(openWindow)
    }
}

/// A recording whose transcript or minutes failed, with Retry.
private struct RecordingFailureRow: View {
    let failure: RecordingsProcessor.Failure
    let processor: RecordingsProcessor
    @State private var retrying = false
    @State private var error: String?
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x1) {
            HStack(spacing: KSpace.x2) {
                KIcon(.warning, size: .xs, color: theme.danger)
                VStack(alignment: .leading, spacing: 2) {
                    Text(failure.title).font(KFont.ui(12, .semibold)).lineLimit(1)
                    Text(RecordingStatusText.failure(failure)).font(KFont.ui(11)).foregroundStyle(theme.muted).lineLimit(2)
                }
                Spacer(minLength: KSpace.x2)
                KButton("Retry", size: .small) { retry() }
                    .disabled(retrying)
            }
            if let error { KErrorText(error) }
        }
        .padding(.horizontal, KSpace.x1)
        .kTooltip(failure.message)
    }

    private func retry() {
        retrying = true
        Task {
            do {
                try await processor.retry(failure)
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
            retrying = false
        }
    }
}

/// A finished recording its space does not hold yet, with Save Again, Show in Finder and Delete,
/// which asks first, in the row.
private struct UnsavedRecordingRow: View {
    let item: UnsavedRecording
    let controller: RecordingController
    @State private var confirmingDelete = false
    @State private var error: String?
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x1) {
            HStack(spacing: KSpace.x2) {
                KIcon(.warning, size: .xs, color: theme.danger)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Unsaved recording").font(KFont.ui(12, .semibold)).lineLimit(1)
                    Text(RecordingStatusText.unsaved(item)).font(KFont.ui(11)).foregroundStyle(theme.muted).lineLimit(2)
                }
                Spacer(minLength: 0)
            }
            if confirmingDelete {
                HStack(spacing: KSpace.x2) {
                    Text("Delete its audio for good?").font(KFont.ui(11)).foregroundStyle(theme.danger)
                    Spacer(minLength: KSpace.x2)
                    KButton("Cancel", size: .small) { confirmingDelete = false }
                    KButton("Delete", kind: .destructive, size: .small) { delete() }
                }
            } else {
                HStack(spacing: KSpace.x2) {
                    Spacer(minLength: 0)
                    KButton("Save Again", size: .small) { saveAgain() }
                        .disabled(controller.resaving.contains(item.id))
                    KButton("Show in Finder", size: .small) {
                        NSWorkspace.shared.activateFileViewerSelecting([item.folder])
                    }
                    KButton("Delete", kind: .destructive, size: .small) { confirmingDelete = true }
                        .disabled(controller.resaving.contains(item.id))
                }
            }
            if let error { KErrorText(error) }
        }
        .padding(.horizontal, KSpace.x1)
        .kTooltip(item.error)
    }

    private func saveAgain() {
        error = nil
        Task { error = await controller.saveAgain(item) }
    }

    private func delete() {
        do {
            try controller.deleteUnsaved(item)
        } catch {
            self.error = error.localizedDescription
        }
        confirmingDelete = false
    }
}

/// The red dot shown for as long as a recording runs.
struct RecordingDot: View {
    var paused = false
    @Environment(\.kTheme) private var theme

    var body: some View {
        Circle()
            .fill(paused ? theme.muted : theme.danger)
            .frame(width: 8, height: 8)
            .accessibilityHidden(true)
    }
}

enum RecordingStatusText {
    static func processing(_ phase: RecordingController.Phase) -> String {
        switch phase {
        case .saving: "Saving the recording"
        case .transcribing(let progress): "Transcribing on this Mac, \(Int(progress * 100))%"
        case .summarising(let progress): "Writing the minutes on this Mac, \(Int(progress * 100))%"
        default: ""
        }
    }

    /// A line about the recording running now: the microphone changed, or how soon it stops by
    /// itself at the longest a space keeps.
    @MainActor static func live(_ controller: RecordingController) -> String? {
        if let notice = controller.notice { return notice }
        if controller.isNearLimit {
            return "This recording stops by itself in about \(controller.minutesToLimit) min, at the longest a space keeps in one file. It is saved, and you can start a new one."
        }
        return nil
    }

    static func unsaved(_ item: UnsavedRecording) -> String {
        let recording = item.recording
        let seconds = Int(recording.duration.components.seconds)
        let parts = [
            [recording.target.spaceName, recording.target.projectTitle].compactMap { $0 }.joined(separator: " / "),
            recording.startedAt.formatted(date: .abbreviated, time: .shortened),
            RecordingText.elapsed(seconds),
        ]
        let summary = parts.joined(separator: ", ")
        return item.error.map { "\(summary). \($0)" } ?? summary
    }

    static func job(_ job: RecordingsProcessor.Job) -> String {
        switch job.step {
        case .transcribing: "Transcribing \(job.title), \(Int(job.progress * 100))%"
        case .summarising: "Writing minutes for \(job.title), \(Int(job.progress * 100))%"
        }
    }

    static func failure(_ failure: RecordingsProcessor.Failure) -> String {
        switch failure.stage {
        case .transcript: "Transcript failed: \(failure.message)"
        case .summary: "Minutes failed: \(failure.message)"
        }
    }
}

/// The window of a recording: the consent explanation the first time, where it goes, the
/// recording itself, then its transcription and minutes.
struct RecordingWindow: View {
    static let id = "recording"
    @Environment(RecordingController.self) private var controller
    @Environment(\.dismissWindow) private var dismissWindow

    var body: some View {
        KSheetSurface(width: 440) {
            switch controller.phase {
            case .idle:
                KSheetTitle(kicker: "Meetings", title: "Nothing is being recorded", message: "Start one from the Record menu, or with Record in the Knode window.", close: close)
            case .consentNeeded(let mode):
                RecordingConsentView(mode: mode)
            case .choosingTarget(let mode):
                RecordingTargetView(mode: mode)
            case .recording(let mode, let target):
                RecordingLiveView(mode: mode, target: target)
            case .saving, .transcribing, .summarising:
                RecordingProcessingView()
            case .done(let note):
                if controller.continuation != nil {
                    KSheetTitle(kicker: "Meetings", title: "Recording saved", message: note, close: close)
                    HStack {
                        Spacer()
                        KButton("Done") { close() }
                        KButton("Record Again", icon: .add, kind: .primary) { Task { await controller.recordAgain() } }
                    }
                } else {
                    KSheetTitle(kicker: "Meetings", title: "Recording saved", message: "The full transcript is kept, and the minutes sit next to it.", close: close)
                    if let note { KBanner(.warning, title: "No minutes", message: note) }
                    HStack { Spacer(); KButton("Done", kind: .primary) { close() } }
                }
            case .failed(let message):
                KSheetTitle(kicker: "Meetings", title: "The recording failed", close: close)
                KErrorText(message)
                HStack { Spacer(); KButton("Close", kind: .primary) { close() } }
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private func close() {
        controller.cancelRequest()
        controller.dismiss()
        dismissWindow(id: Self.id)
    }
}

/// The first-run explanation: recording people may need their consent.
private struct RecordingConsentView: View {
    let mode: RecordingMode
    @Environment(RecordingController.self) private var controller

    var body: some View {
        KSheetTitle(kicker: "Before you record", title: "Ask everyone first", message: nil, close: controller.cancelRequest)
        VStack(alignment: .leading, spacing: KSpace.x3) {
            KExplanation("Knode records meetings on this Mac, transcribes them and writes their minutes on this Mac. Nothing is sent anywhere.", size: 13)
            KExplanation("In some places the law requires every participant to agree before a conversation is recorded. You are responsible for asking them.", size: 13)
            KExplanation("Nothing is ever recorded unless you start it for that meeting, and a red dot shows in the menu bar for as long as it records.", size: 13)
        }
        HStack {
            Spacer()
            KButton("Cancel") { controller.cancelRequest() }
            KButton("I Understand", kind: .primary) { controller.acknowledgeConsent() }
        }
    }
}

/// Where the recording goes: a space and, optionally, a project in it.
private struct RecordingTargetView: View {
    let mode: RecordingMode
    @Environment(RecordingController.self) private var controller
    @Environment(AppModel.self) private var model
    @State private var space: URL?
    @State private var project: Node.ID?
    @State private var projects: [ProjectSummary] = []
    @State private var starting = false
    @State private var minutes = true
    @State private var minutesLoaded = false
    @State private var minutesError: String?

    private var spaces: [SpaceItem] {
        model.sidebarSpaces.filter { !$0.isExample && !$0.isMissing }
    }

    var body: some View {
        KSheetTitle(kicker: "Meetings", title: RecordingText.title(of: mode), message: RecordingText.explanation(of: mode), close: controller.cancelRequest)
        if spaces.isEmpty {
            KExplanation("Create or open a space first: the recording goes into one.")
        } else {
            VStack(alignment: .leading, spacing: KSpace.x3) {
                KSettingRow("Space") {
                    KDropdown(selection: $space, options: spaces.map { item in KOption(Optional(item.url), item.icon.map { "\($0) \(item.name)" } ?? item.name) })
                        .frame(width: 220)
                }
                KSettingRow("Project", detail: "Optional") {
                    KDropdown(selection: $project, options: [KOption(nil, "None")] + projects.map { KOption(Optional($0.id), $0.title) })
                        .frame(width: 220)
                }
                VStack(alignment: .leading, spacing: KSpace.x1) {
                    KToggle("Write minutes", isOn: Binding(get: { minutes }, set: { setMinutes($0) }))
                        .disabled(!minutesLoaded)
                    KExplanation("For every recording in this space. The full transcript is always kept.", size: 12)
                    if let minutesError { KErrorText(minutesError) }
                }
            }
            RecordingNoticeView()
        }
        HStack {
            Spacer()
            KButton("Cancel") { controller.cancelRequest() }
            KButton("Start Recording", icon: .add, kind: .primary) { start() }
                .disabled(space == nil || starting)
        }
        .task {
            if space == nil {
                space = model.selectedSpaceURL.flatMap { url in spaces.first { $0.url == url }?.url } ?? spaces.first?.url
            }
        }
        .task(id: space) {
            project = nil
            projects = []
            minutesLoaded = false
            minutesError = nil
            guard let space else { return }
            minutes = await model.recordingSummaries(inSpaceAt: space)
            minutesLoaded = true
            projects = await model.projects(inSpaceAt: space)
        }
    }

    /// Writes `recordings.summaries` of the chosen space at once, as the capture switch does.
    private func setMinutes(_ on: Bool) {
        guard let space else { return }
        let before = minutes
        minutes = on
        minutesError = nil
        Task {
            do {
                try await model.setRecordingSummaries(on, inSpaceAt: space)
            } catch {
                minutes = before
                minutesError = error.localizedDescription
            }
        }
    }

    private func start() {
        guard let space, let item = spaces.first(where: { $0.url == space }) else { return }
        let chosen = projects.first { $0.id == project }
        starting = true
        Task {
            await controller.start(target: RecordingTarget(spaceURL: space, spaceName: item.name, projectID: chosen?.id, projectTitle: chosen?.title))
            starting = false
        }
    }
}

/// The notice to post in the meeting's chat, with a button that copies it.
private struct RecordingNoticeView: View {
    @State private var copied = false
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSectionLabel("Notice for the meeting chat")
            HStack(alignment: .top, spacing: KSpace.x2) {
                Text(RecordingController.notice)
                    .font(KFont.ui(12))
                    .foregroundStyle(theme.fg)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: KSpace.x2)
                KIconButton(copied ? .done : .copy, title: copied ? "Copied" : "Copy Notice", size: .small) {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(RecordingController.notice, forType: .string)
                    copied = true
                }
            }
            .padding(KSpace.x3)
            .background(RoundedRectangle(cornerRadius: KRadius.control, style: .continuous).fill(theme.hoverFill))
        }
    }
}

/// The recording as it runs: the red dot, the time, the input level, pause and stop.
private struct RecordingLiveView: View {
    let mode: RecordingMode
    let target: RecordingTarget
    @Environment(RecordingController.self) private var controller
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(spacing: KSpace.x3) {
            RecordingDot(paused: controller.isPaused)
            VStack(alignment: .leading, spacing: 2) {
                Text(controller.isPaused ? "Paused" : "Recording").font(KFont.ui(17, .semibold))
                Text([target.spaceName, target.projectTitle].compactMap { $0 }.joined(separator: " / ")).font(KFont.ui(12)).foregroundStyle(theme.muted)
            }
            Spacer()
            Text(RecordingText.elapsed(controller.elapsedSeconds)).font(KFont.mono(20)).monospacedDigit()
        }
        TimelineView(.periodic(from: .now, by: 0.1)) { _ in
            KProgress(value: Double(controller.level))
        }
        .accessibilityLabel("Input level")
        if let notice = controller.notice {
            KBanner(.info, title: notice)
        } else if controller.isNearLimit, let line = RecordingStatusText.live(controller) {
            KBanner(.warning, title: "Close to the limit", message: line)
        }
        RecordingNoticeView()
        HStack {
            KButton("Discard", kind: .destructive) { controller.discard() }
            Spacer()
            KButton(controller.isPaused ? "Resume" : "Pause") {
                controller.isPaused ? controller.resume() : controller.pause()
            }
            KButton("Stop", icon: .done, kind: .primary) { controller.stop() }
        }
    }
}

private struct RecordingProcessingView: View {
    @Environment(RecordingController.self) private var controller

    var body: some View {
        KSheetHeader(kicker: "Meetings", title: "Saving the meeting", message: "Everything runs on this Mac. You can close this window; it goes on in the background.")
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KExplanation(RecordingStatusText.processing(controller.phase), size: 13)
            KProgress(value: progress)
        }
        HStack {
            Spacer()
            KButton("Cancel") { controller.cancelProcessing() }
                .disabled(!controller.canCancelProcessing)
                .kTooltip(controller.isSaving ? "Saving cannot be interrupted" : nil)
        }
    }

    private var progress: Double? {
        switch controller.phase {
        case .transcribing(let value), .summarising(let value): value
        default: nil
        }
    }
}
