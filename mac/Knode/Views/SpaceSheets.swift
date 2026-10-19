import AppKit
import SwiftUI

/// Creates a space: a name, where it goes (`~/Knode` by default) and whether agents and capture
/// write to it.
struct NewSpaceSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var folder: URL?
    @State private var makeDefault = false

    var body: some View {
        let folder = folder ?? model.settings.spacesFolder
        Form {
            TextField("Name", text: $name, prompt: Text("Work"))
            LabeledContent("Location") {
                HStack {
                    Text(folder.path(percentEncoded: false).replacing(NSHomeDirectory(), with: "~"))
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .foregroundStyle(.secondary)
                    Button("Choose…", systemImage: "folder") {
                        if let url = SpacePanels.chooseFolder(startingAt: folder) { self.folder = url }
                    }
                }
            }
            Toggle("Use for agents and capture", isOn: $makeDefault)
            Text("Agents connected to Knode and captured conversations write to this space.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .formStyle(.grouped)
        .frame(width: 440)
        .navigationTitle("New Space")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel", systemImage: "xmark") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Create", systemImage: "plus") {
                    let (name, makeDefault) = (name, makeDefault)
                    dismiss()
                    Task { await model.createSpace(named: name, in: folder, makeDefault: makeDefault) }
                }
                .disabled(name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .onAppear { makeDefault = model.defaultSpaceURL == nil }
    }
}

/// Renames a space: its name in `manifest.json`. The package keeps its file name.
struct RenameSpaceSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let url: URL
    @State private var name = ""

    var body: some View {
        Form {
            TextField("Name", text: $name)
            Text("The package keeps its file name: \(url.lastPathComponent)")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .formStyle(.grouped)
        .frame(width: 400)
        .navigationTitle("Rename Space")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel", systemImage: "xmark") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Rename", systemImage: "pencil") {
                    let name = name
                    dismiss()
                    Task { await model.renameSpace(at: url, to: name) }
                }
                .disabled(name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .onAppear { name = model.sidebarSpaces.first { $0.url == url }?.name ?? "" }
    }
}

/// Chooses the icon of a space: one emoji, kept in the space as `space.icon`.
struct SpaceIconSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let url: URL
    @State private var icon = ""
    @FocusState private var focused: Bool

    private var isValid: Bool {
        let trimmed = icon.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty || SpaceIcon.isEmoji(trimmed)
    }

    var body: some View {
        Form {
            HStack {
                TextField("Icon", text: $icon, prompt: Text("One emoji"))
                    .focused($focused)
                Button("Emoji & Symbols", systemImage: "face.smiling") {
                    focused = true
                    NSApp.orderFrontCharacterPalette(nil)
                }
            }
            if !isValid {
                Label("An icon is one emoji.", systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            LazyVGrid(columns: Array(repeating: GridItem(.fixed(34)), count: 6), spacing: 6) {
                ForEach(SpaceIcon.suggestions, id: \.self) { suggestion in
                    Button(suggestion) { icon = suggestion }
                        .buttonStyle(.borderless)
                        .font(.system(size: 22))
                        .accessibilityLabel("Use \(suggestion)")
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 360)
        .navigationTitle("Choose Icon")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel", systemImage: "xmark") { dismiss() }
            }
            ToolbarItem(placement: .destructiveAction) {
                Button("Remove Icon", systemImage: "trash") {
                    dismiss()
                    Task { await model.setIcon(nil, forSpaceAt: url) }
                }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Save", systemImage: "checkmark") {
                    let icon = icon
                    dismiss()
                    Task { await model.setIcon(icon, forSpaceAt: url) }
                }
                .disabled(!isValid)
            }
        }
        .onAppear {
            icon = model.sidebarSpaces.first { $0.url == url }?.icon ?? ""
            focused = true
        }
    }
}
