import AppKit

/// How a text view attaches what is dropped on it or pasted into it: each call attaches and
/// returns the names to show the files by, `![[name]]`, or nothing when nothing was attached.
@MainActor
struct AttachmentDrop {
    /// Attaches files, such as dropped from the Finder.
    var files: ([URL]) async -> [String]
    /// Attaches an image, such as a screenshot pasted, as PNG under a name.
    var image: (Data, String) async -> String?
}

/// A plain text view that attaches files dropped on it and images pasted into it, and writes
/// `![[name]]` where they went. Without `attachments` it behaves as `NSTextView`.
final class AttachingTextView: NSTextView {
    var attachments: AttachmentDrop?

    private static let imageTypes: [NSPasteboard.PasteboardType] = [.png, .tiff]

    override var readablePasteboardTypes: [NSPasteboard.PasteboardType] {
        guard attachments != nil else { return super.readablePasteboardTypes }
        return [.fileURL] + Self.imageTypes + super.readablePasteboardTypes
    }

    override var acceptableDragTypes: [NSPasteboard.PasteboardType] {
        guard attachments != nil else { return super.acceptableDragTypes }
        return [.fileURL] + Self.imageTypes + super.acceptableDragTypes
    }

    override func preferredPasteboardType(from availableTypes: [NSPasteboard.PasteboardType], restrictedToTypesFrom allowedTypes: [NSPasteboard.PasteboardType]?) -> NSPasteboard.PasteboardType? {
        if attachments != nil {
            // Files before their names, and an image before any text that comes with it.
            for type in [NSPasteboard.PasteboardType.fileURL] + Self.imageTypes where availableTypes.contains(type) {
                if allowedTypes?.contains(type) ?? true { return type }
            }
        }
        return super.preferredPasteboardType(from: availableTypes, restrictedToTypesFrom: allowedTypes)
    }

    override func readSelection(from pboard: NSPasteboard, type: NSPasteboard.PasteboardType) -> Bool {
        guard let attachments else { return super.readSelection(from: pboard, type: type) }
        let range = selectedRange()
        if type == .fileURL {
            let urls = NoteAttachmentsModel.fileURLs(from: pboard)
            guard !urls.isEmpty else { return super.readSelection(from: pboard, type: type) }
            Task {
                let names = await attachments.files(urls)
                insertEmbeds(names, at: range)
            }
            return true
        }
        if Self.imageTypes.contains(type), let (data, name) = NoteAttachmentsModel.pastedImage(from: pboard) {
            Task {
                guard let attached = await attachments.image(data, name) else { return }
                insertEmbeds([attached], at: range)
            }
            return true
        }
        return super.readSelection(from: pboard, type: type)
    }

    /// Writes `![[name]]` for each name in place of `range`, each on a line of its own, as the
    /// person would type it, so it is saved and undone as typing is.
    func insertEmbeds(_ names: [String], at range: NSRange) {
        guard !names.isEmpty else { return }
        let text = string as NSString
        let location = min(range.location, text.length)
        let target = NSRange(location: location, length: min(range.length, text.length - location))
        let before = location > 0 ? text.substring(with: NSRange(location: location - 1, length: 1)) : "\n"
        let after = NSMaxRange(target) < text.length ? text.substring(with: NSRange(location: NSMaxRange(target), length: 1)) : "\n"
        let embeds = names.map(AttachmentInfo.embed).joined(separator: "\n")
        let insert = (before == "\n" ? "" : "\n") + embeds + (after == "\n" ? "" : "\n")
        insertText(insert, replacementRange: target)
    }
}
