import SwiftUI

/// A calm note at the top of the list when files were deleted from the live Markdown folder:
/// their notes are still in Knode, waiting for the person to decide.
struct DeletedNotesBanner: View {
    let deleted: DeletedNotesModel
    let review: () -> Void

    var body: some View {
        let count = deleted.waiting.count
        HStack(spacing: 8) {
            Label {
                Text(count == 1 ? "1 note was deleted from the Markdown folder" : "\(count) notes were deleted from the Markdown folder")
                    .fixedSize(horizontal: false, vertical: true)
            } icon: {
                Image(systemName: "folder.badge.minus")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Button("Review", systemImage: "list.bullet", action: review)
                .controlSize(.small)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }
}

/// Asks what to do with each note whose file was deleted from the live Markdown folder: delete it
/// in Knode too, keep it (its file is written again), or decide later.
struct DeletedNotesSheet: View {
    let deleted: DeletedNotesModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let notes = deleted.waiting
        VStack(alignment: .leading, spacing: 12) {
            Text("Deleted from the Markdown Folder")
                .font(.headline)
            Text("These files were deleted from the live Markdown folder. Their notes are still in Knode, and are not written to the folder until you decide.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if notes.isEmpty {
                ContentUnavailableView("Nothing to Decide", systemImage: "checkmark.circle", description: Text("Every note is in step with the folder."))
                    .frame(minHeight: 120)
            } else {
                List(notes) { note in
                    HStack(spacing: 8) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(note.title.isEmpty ? "Untitled" : note.title)
                            Text(note.path)
                                .font(.caption.monospaced())
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        Button("Keep", systemImage: "arrow.uturn.backward") {
                            Task { await deleted.keep([note.id]) }
                        }
                        .help("Keep the note and write its file again")
                        Button("Delete", systemImage: "trash", role: .destructive) {
                            Task { await deleted.delete([note.id]) }
                        }
                        .help("Delete the note in Knode too. You can undo it.")
                        Button("Later", systemImage: "clock") {
                            deleted.decideLater([note.id])
                        }
                        .help("Decide later")
                    }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                }
                .frame(minHeight: 160)
            }

            if !deleted.lastDeleted.isEmpty {
                HStack {
                    Text(deleted.lastDeleted.count == 1 ? "Deleted 1 note." : "Deleted \(deleted.lastDeleted.count) notes.")
                        .foregroundStyle(.secondary)
                    Button("Undo", systemImage: "arrow.uturn.backward") {
                        Task { await deleted.undoDelete() }
                    }
                }
                .font(.callout)
            }
            if let message = deleted.errorMessage {
                Text(message)
                    .font(.callout)
                    .foregroundStyle(.red)
            }

            HStack {
                Button("Decide Later", systemImage: "clock") {
                    deleted.decideLater(notes.map(\.id))
                    dismiss()
                }
                Spacer()
                Button("Delete All in Knode", systemImage: "trash", role: .destructive) {
                    Task { await deleted.delete(notes.map(\.id)) }
                }
                .disabled(notes.isEmpty)
                Button("Keep All", systemImage: "arrow.uturn.backward") {
                    Task {
                        await deleted.keep(notes.map(\.id))
                        dismiss()
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(notes.isEmpty)
            }
            .disabled(deleted.isWorking)
        }
        .padding(20)
        .frame(width: 520)
    }
}
