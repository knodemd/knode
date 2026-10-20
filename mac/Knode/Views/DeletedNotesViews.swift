import SwiftUI

/// A calm note at the top of the list when files were deleted from the live Markdown folder:
/// their notes are still in Knode, waiting for the person to decide.
struct DeletedNotesBanner: View {
    let deleted: DeletedNotesModel
    let review: () -> Void

    var body: some View {
        let count = deleted.waiting.count
        KBanner(.warning, title: count == 1 ? "1 note was deleted from the Markdown folder" : "\(count) notes were deleted from the Markdown folder", actionsBelow: true, actions: {
            KButton("Review", icon: .list, size: .small, action: review)
        })
        .padding(.horizontal, KSpace.x2)
        .padding(.vertical, KSpace.x1)
    }
}

/// Asks what to do with each note whose file was deleted from the live Markdown folder: delete it
/// in Knode too, keep it (its file is written again), or decide later.
struct DeletedNotesSheet: View {
    let deleted: DeletedNotesModel
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme

    var body: some View {
        let notes = deleted.waiting
        KSheetSurface(width: 560) {
            KSheetTitle(kicker: "Markdown folder", title: "Deleted from the Markdown folder",
                        message: "These files were deleted from the live Markdown folder. Their notes are still in Knode, and are not written to the folder until you decide.",
                        close: close)

            if notes.isEmpty {
                KCard(padding: 0) {
                    KEmptyState(icon: .done, title: "Nothing to decide", message: "Every note is in step with the folder.")
                        .frame(height: 200)
                }
            } else {
                KCard(padding: KSpace.x2) {
                    KScrollView {
                        VStack(spacing: 2) {
                            ForEach(notes) { note in
                                KRow {
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(note.title.isEmpty ? "Untitled" : note.title).font(KFont.ui(13, .semibold)).lineLimit(1)
                                        Text(note.path)
                                            .font(KFont.mono(11))
                                            .foregroundStyle(theme.muted)
                                            .lineLimit(1)
                                            .truncationMode(.middle)
                                    }
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    KIconButton(.undo, title: "Keep the note and write its file again", size: .small) {
                                        Task { await deleted.keep([note.id]) }
                                    }
                                    KIconButton(.delete, title: "Delete the note in Knode too. You can undo it.", size: .small) {
                                        Task { await deleted.delete([note.id]) }
                                    }
                                    KIconButton(.history, title: "Decide later", size: .small) {
                                        deleted.decideLater([note.id])
                                    }
                                }
                            }
                        }
                    }
                    .frame(minHeight: 160, maxHeight: 280)
                }
            }

            if !deleted.lastDeleted.isEmpty {
                HStack(spacing: KSpace.x2) {
                    Text(deleted.lastDeleted.count == 1 ? "Deleted 1 note." : "Deleted \(deleted.lastDeleted.count) notes.")
                        .font(KFont.ui(13))
                        .foregroundStyle(theme.muted)
                    KButton("Undo", icon: .undo, kind: .ghost, size: .small) {
                        Task { await deleted.undoDelete() }
                    }
                }
            }
            if let message = deleted.errorMessage {
                KErrorText(message)
            }

            HStack(spacing: KSpace.x2) {
                KButton("Decide later", icon: .history, kind: .ghost) {
                    deleted.decideLater(notes.map(\.id))
                    close()
                }
                Spacer()
                KButton("Delete all in Knode", icon: .delete, kind: .destructive) {
                    Task { await deleted.delete(notes.map(\.id)) }
                }
                .disabled(notes.isEmpty)
                Button {
                    Task {
                        await deleted.keep(notes.map(\.id))
                        close()
                    }
                } label: {
                    HStack(spacing: 8) { KIcon(.undo, size: .sm); Text("Keep all") }
                }
                .buttonStyle(KButtonStyle(kind: .primary))
                .keyboardShortcut(.defaultAction)
                .disabled(notes.isEmpty)
            }
            .disabled(deleted.isWorking)
        }
        .knodeTheme()
    }

    private func close() {
        model.isShowingDeletedNotes = false
    }
}
