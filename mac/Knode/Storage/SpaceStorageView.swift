import SwiftUI

/// Storage, in Settings > Spaces: what the space takes, by kind and by source, its largest files
/// with Show and Remove, and its limits.
struct SpaceStorageSection: View {
    @Bindable var model: SpaceStorageModel
    @Environment(\.kOverlays) private var overlays
    @Environment(\.kTheme) private var theme
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        KSettingsSection("Storage") {
            if let summary = model.summary {
                KSettingRow("Total", detail: "\(summary.nodes) nodes, \(summary.files) files. Sizes are computed on this Mac.") {
                    Text(StorageText.size(summary.total)).font(KFont.ui(13, .semibold)).monospacedDigit()
                }
                if summary.over {
                    KErrorText("This space is past its size limit: imports ask first, and the Photos library adds nothing new until there is room.")
                }
                ForEach(model.kinds, id: \.name) { kind in
                    KHairline()
                    KSettingRow(kind.name) {
                        Text(StorageText.size(kind.size)).font(KFont.ui(12)).foregroundStyle(theme.muted).monospacedDigit()
                    }
                }
                if !summary.sources.isEmpty {
                    KHairline()
                    KSettingRow("Added by", detail: summary.sources.prefix(4).map { "\(StorageText.actor($0.actor)): \(StorageText.size($0.size))" }.joined(separator: " · ")) { EmptyView() }
                }
                KHairline()
                KSettingRow("Warn past", detail: "Imports ask first and the Photos library holds back once the space is larger.") {
                    KDropdown(selection: Binding(get: { summary.limits.warnAt }, set: { value in Task { await model.setWarnAt(value) } }),
                              options: Self.warnOptions(current: summary.limits.warnAt))
                        .frame(width: 140)
                }
                KHairline()
                KSettingToggle("Keep large video originals",
                               detail: "Off: a video larger than \(StorageText.size(summary.limits.maxFile)) keeps only its audio, and the original stays where it was. On: the original is kept in the space too, up to \(StorageText.size(summary.limits.maxOriginal)).",
                               isOn: Binding(get: { summary.limits.keepLargeOriginals }, set: { on in Task { await model.setKeepLargeOriginals(on) } }))
                if !summary.largest.isEmpty {
                    KHairline()
                    KSectionLabel("Largest files")
                    ForEach(summary.largest) { file in
                        StorageFileRow(file: file, canRemove: model.canRemove,
                                       show: { Task { await model.show(file); openWindow(id: "main") } },
                                       remove: { confirmRemove(file) })
                    }
                }
            } else if model.isBusy {
                HStack { Spacer(); KSpinner(); Spacer() }.padding(KSpace.x2)
            }
            if let error = model.errorMessage { KErrorText(error) }
        }
        .disabled(model.isBusy && model.summary != nil)
        .task(id: model.spaceID) { await model.load() }
    }

    static func warnOptions(current: Int64) -> [KOption<Int64>] {
        let choices = SpaceStorageModel.warnChoices.contains(current) ? SpaceStorageModel.warnChoices : (SpaceStorageModel.warnChoices + [current]).sorted()
        return choices.map { KOption($0, $0 == 0 ? "Never" : StorageText.size($0)) }
    }

    private func confirmRemove(_ file: StorageFile) {
        overlays?.confirmation = KConfirmation(
            title: "Remove \(file.name)?",
            message: "\(file.title) keeps its text, such as what was read or transcribed. The file leaves the space at the next purge, and Undo in the history brings it back until then.",
            confirmTitle: "Remove file",
            isDestructive: true
        ) { [model] in Task { await model.remove(file) } }
    }
}

private struct StorageFileRow: View {
    let file: StorageFile
    let canRemove: Bool
    let show: () -> Void
    let remove: () -> Void
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(spacing: KSpace.x2) {
            VStack(alignment: .leading, spacing: 2) {
                Text(file.name).font(KFont.ui(12, .semibold)).lineLimit(1)
                Text("\(file.title) · \(StorageText.size(file.size)) · \(StorageText.actor(file.addedBy))")
                    .font(KFont.ui(11)).foregroundStyle(theme.muted).lineLimit(1)
            }
            Spacer(minLength: KSpace.x2)
            KButton("Show", icon: .show, size: .small, action: show)
            if canRemove {
                KButton("Remove", icon: .delete, kind: .destructive, size: .small, action: remove)
            }
        }
        .padding(.horizontal, KSpace.x1)
        .padding(.vertical, 4)
    }
}
