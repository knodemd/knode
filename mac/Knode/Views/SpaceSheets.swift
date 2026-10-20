import AppKit
import SwiftUI

/// The buttons at the bottom of our sheets: cancel on the left of the main action.
private struct SheetActions<Extra: View>: View {
    var confirmTitle: String
    var confirmIcon: KGlyph?
    var canConfirm: Bool
    var cancel: () -> Void
    var confirm: () -> Void
    @ViewBuilder var extra: Extra

    var body: some View {
        HStack(spacing: KSpace.x2) {
            extra
            Spacer()
            KButton("Cancel", action: cancel)
            Button(action: confirm) {
                HStack(spacing: KSpace.x2) {
                    if let confirmIcon { KIcon(confirmIcon, size: .sm) }
                    Text(confirmTitle)
                }
            }
            .buttonStyle(KButtonStyle(kind: .primary))
            .keyboardShortcut(.defaultAction)
            .disabled(!canConfirm)
        }
    }
}

/// A label above a field, in Knode Line.
private struct FieldLabel: View {
    let title: String
    @Environment(\.kTheme) private var theme

    var body: some View {
        Text(title).kLineStyle(10.5).foregroundStyle(theme.muted)
    }
}

/// Creates a space: a name, where it goes (`~/Knode` by default) and whether agents and capture
/// write to it.
struct NewSpaceSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    @State private var name = ""
    @State private var folder: URL?
    @State private var makeDefault = false

    var body: some View {
        let folder = folder ?? model.settings.spacesFolder
        VStack(alignment: .leading, spacing: KSpace.x5) {
            KSheetHeader(kicker: "New space", title: "Name the space", message: "A space is a folder of notes that you and your agents read and write.")
            VStack(alignment: .leading, spacing: KSpace.x2) {
                FieldLabel(title: "Name")
                KTextField("Work", text: $name) { create(in: folder) }
            }
            VStack(alignment: .leading, spacing: KSpace.x2) {
                FieldLabel(title: "Location")
                HStack(spacing: KSpace.x2) {
                    Text(folder.path(percentEncoded: false).replacing(NSHomeDirectory(), with: "~"))
                        .font(KFont.mono(12))
                        .foregroundStyle(theme.muted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    KButton("Choose", icon: .folder, size: .small) {
                        if let url = SpacePanels.chooseFolder(startingAt: folder) { self.folder = url }
                    }
                }
            }
            VStack(alignment: .leading, spacing: KSpace.x1) {
                KToggle("Use for agents and capture", isOn: $makeDefault)
                Text("Agents connected to Knode and captured conversations write to this space.")
                    .font(KFont.ui(12))
                    .foregroundStyle(theme.muted)
            }
            SheetActions(confirmTitle: "Create", confirmIcon: .add, canConfirm: isValid, cancel: { model.sheet = nil },
                         confirm: { create(in: folder) }) { EmptyView() }
        }
        .onAppear { makeDefault = model.defaultSpaceURL == nil }
    }

    private var isValid: Bool { !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    private func create(in folder: URL) {
        guard isValid else { return }
        let (name, makeDefault) = (name, makeDefault)
        model.sheet = nil
        Task { await model.createSpace(named: name, in: folder, makeDefault: makeDefault) }
    }
}

/// Renames a space: its name in `manifest.json`. The package keeps its file name.
struct RenameSpaceSheet: View {
    @Environment(AppModel.self) private var model
    let url: URL
    @State private var name = ""

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x5) {
            KSheetHeader(kicker: "Rename space", title: "A new name for the space", message: "The package keeps its file name: \(url.lastPathComponent)")
            KTextField("Name", text: $name) { rename() }
            SheetActions(confirmTitle: "Rename", confirmIcon: .edit, canConfirm: isValid, cancel: { model.sheet = nil },
                         confirm: rename) { EmptyView() }
        }
        .onAppear { name = model.sidebarSpaces.first { $0.url == url }?.name ?? "" }
    }

    private var isValid: Bool { !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    private func rename() {
        guard isValid else { return }
        let name = name
        model.sheet = nil
        Task { await model.renameSpace(at: url, to: name) }
    }
}

/// Chooses the icon of a space: one emoji, kept in the space as `space.icon`.
struct SpaceIconSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    let url: URL
    @State private var icon = ""

    private var isValid: Bool {
        let trimmed = icon.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty || SpaceIcon.isEmoji(trimmed)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x5) {
            KSheetHeader(kicker: "Space icon", title: "Choose an icon", message: "One emoji, shown beside the name of the space.")
            HStack(spacing: KSpace.x2) {
                KTextField("One emoji", text: $icon)
                KButton("Emoji", icon: .grid, size: .regular) { NSApp.orderFrontCharacterPalette(nil) }
            }
            if !isValid {
                HStack(spacing: 6) {
                    KIcon(.warning, size: .xs, color: theme.danger)
                    Text("An icon is one emoji.").font(KFont.ui(12)).foregroundStyle(theme.danger)
                }
            }
            LazyVGrid(columns: Array(repeating: GridItem(.fixed(40), spacing: KSpace.x1), count: 8), spacing: KSpace.x1) {
                ForEach(SpaceIcon.suggestions, id: \.self) { suggestion in
                    EmojiCell(emoji: suggestion, isSelected: icon == suggestion) { icon = suggestion }
                }
            }
            SheetActions(confirmTitle: "Save", confirmIcon: .done, canConfirm: isValid, cancel: { model.sheet = nil }, confirm: save) {
                KButton("Remove icon", icon: .delete, kind: .ghost) {
                    model.sheet = nil
                    Task { await model.setIcon(nil, forSpaceAt: url) }
                }
            }
        }
        .onAppear {
            icon = model.sidebarSpaces.first { $0.url == url }?.icon ?? ""
        }
    }

    private func save() {
        guard isValid else { return }
        let icon = icon
        model.sheet = nil
        Task { await model.setIcon(icon, forSpaceAt: url) }
    }
}

private struct EmojiCell: View {
    let emoji: String
    let isSelected: Bool
    let action: () -> Void
    @Environment(\.kTheme) private var theme
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Text(emoji)
                .font(KFont.ui(22))
                .frame(width: 40, height: 40)
                .background(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous)
                    .fill(isSelected ? theme.highlight : hovering ? theme.hoverFill : .clear))
                .overlay(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous)
                    .strokeBorder(isSelected ? theme.indigo : .clear))
        }
        .buttonStyle(KPlainButtonStyle())
        .onHover { hovering = $0 }
        .accessibilityLabel("Use \(emoji)")
    }
}
