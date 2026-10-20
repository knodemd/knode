import AppKit
import Foundation
import Observation
import UniformTypeIdentifiers

/// The files attached to the open note: listed in the inspector, shown inline in the preview
/// where the body says `![[name]]`, and added by dragging files in or pasting an image.
@MainActor
@Observable
final class NoteAttachmentsModel {
    let spaceID: Space.ID
    let nodeID: Node.ID

    private(set) var attachments: [AttachmentInfo] = []
    /// Where each attachment's file is on this Mac, by name; missing while the file is not there.
    private(set) var files: [String: URL] = [:]
    private(set) var isLoaded = false
    private(set) var isAttaching = false
    var errorMessage: String?

    @ObservationIgnored private let store: any AttachmentStore

    init(spaceID: Space.ID, nodeID: Node.ID, store: any AttachmentStore) {
        self.spaceID = spaceID
        self.nodeID = nodeID
        self.store = store
    }

    /// Reads the attachments of the note again.
    func load() async {
        do {
            let found = try await store.attachments(of: nodeID, in: spaceID)
            var files: [String: URL] = [:]
            for attachment in found {
                files[attachment.name] = try await store.attachmentURL(named: attachment.name, of: nodeID, in: spaceID)
            }
            attachments = found
            self.files = files
            isLoaded = true
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func attachment(named name: String) -> AttachmentInfo? {
        attachments.first { $0.name == name }
    }

    func file(of name: String) -> URL? {
        files[name]
    }

    /// Attaches files, as dropped from the Finder, and returns what was attached. Folders and
    /// files larger than Knode attaches are left out, with a message.
    @discardableResult
    func attachFiles(_ urls: [URL]) async -> [AttachmentInfo] {
        let (files, refused) = Self.attachable(urls)
        isAttaching = true
        defer { isAttaching = false }
        var added: [AttachmentInfo] = []
        var failure: String?
        for url in files {
            do {
                added.append(try await store.attachFile(at: url, to: nodeID, in: spaceID))
            } catch {
                failure = "\(url.lastPathComponent) could not be attached: \(error.localizedDescription)"
            }
        }
        await load()
        errorMessage = failure ?? refused.first
        return added
    }

    /// Attaches bytes, such as a pasted image, under `name`.
    func attach(_ data: Data, named name: String) async -> AttachmentInfo? {
        isAttaching = true
        defer { isAttaching = false }
        do {
            let added = try await store.attach(data, named: name, to: nodeID, in: spaceID)
            await load()
            return added
        } catch {
            errorMessage = error.localizedDescription
            return nil
        }
    }

    /// Removes an attachment. The file stays in the space, so the change can be undone.
    func remove(_ attachment: AttachmentInfo) async {
        do {
            try await store.removeAttachment(named: attachment.name, from: nodeID, in: spaceID)
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Opens the file with the app the Mac uses for it.
    func open(_ attachment: AttachmentInfo) {
        guard let url = files[attachment.name] else {
            errorMessage = "The file of \(attachment.name) is not on this Mac yet."
            return
        }
        NSWorkspace.shared.open(url)
    }

    /// Shows the file in the Finder, inside the space.
    func reveal(_ attachment: AttachmentInfo) {
        guard let url = files[attachment.name] else {
            errorMessage = "The file of \(attachment.name) is not on this Mac yet."
            return
        }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    // MARK: Helpers

    /// The files among `urls` Knode can attach, and a message for each one it can't.
    nonisolated static func attachable(_ urls: [URL]) -> (files: [URL], refused: [String]) {
        var files: [URL] = []
        var refused: [String] = []
        for url in urls where url.isFileURL {
            let values = try? url.resourceValues(forKeys: [.isDirectoryKey, .fileSizeKey])
            if values?.isDirectory == true {
                refused.append("\(url.lastPathComponent) is a folder: attach the files in it instead.")
            } else if let size = values?.fileSize, Int64(size) > AttachmentInfo.maxSize {
                refused.append("\(url.lastPathComponent) is larger than 100 MB, the most Knode attaches.")
            } else {
                files.append(url)
            }
        }
        return (files, refused)
    }

    /// The body with `![[name]]` for each attachment added at its end, each on a line of its own.
    nonisolated static func body(_ body: String, embedding names: [String]) -> String {
        guard !names.isEmpty else { return body }
        let embeds = names.map(AttachmentInfo.embed).joined(separator: "\n")
        if body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return embeds + "\n" }
        let trimmed = body.hasSuffix("\n") ? String(body.dropLast(body.reversed().prefix(while: { $0 == "\n" }).count)) : body
        return trimmed + "\n\n" + embeds + "\n"
    }

    /// A pasted image as PNG, with a name such as `Pasted image 2026-10-20 at 09.30.00.png`, or
    /// nil when the pasteboard holds no image.
    static func pastedImage(from pasteboard: NSPasteboard, at date: Date = .now) -> (data: Data, name: String)? {
        let png = pasteboard.data(forType: .png) ?? pasteboard.data(forType: .tiff).flatMap {
            NSBitmapImageRep(data: $0)?.representation(using: .png, properties: [:])
        }
        guard let png else { return nil }
        return (png, pastedImageName(at: date))
    }

    nonisolated static func pastedImageName(at date: Date) -> String {
        let day = date.formatted(.iso8601.year().month().day())
        let time = date.formatted(.dateTime.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits).second(.twoDigits))
            .replacing(":", with: ".")
        return "Pasted image \(day) at \(time).png"
    }

    /// The file URLs on a pasteboard, such as files copied in the Finder.
    static func fileURLs(from pasteboard: NSPasteboard) -> [URL] {
        let urls = pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL]
        return urls ?? []
    }
}

/// Reads what a drag brings, from SwiftUI's `onDrop`.
@MainActor
enum DroppedItems {
    /// The file URLs of the providers that have one.
    static func fileURLs(_ providers: [NSItemProvider]) async -> [URL] {
        var urls: [URL] = []
        for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
            if let url = await fileURL(provider) { urls.append(url) }
        }
        return urls
    }

    /// The text of the providers that have some, such as note ids dragged from the list.
    static func strings(_ providers: [NSItemProvider]) async -> [String] {
        var strings: [String] = []
        for provider in providers where provider.canLoadObject(ofClass: NSString.self) {
            if let string = await string(provider) { strings.append(string) }
        }
        return strings
    }

    static func hasFiles(_ providers: [NSItemProvider]) -> Bool {
        providers.contains { $0.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) }
    }

    private static func fileURL(_ provider: NSItemProvider) async -> URL? {
        await withCheckedContinuation { continuation in
            _ = provider.loadObject(ofClass: NSURL.self) { object, _ in
                continuation.resume(returning: (object as? NSURL).flatMap { $0 as URL })
            }
        }
    }

    private static func string(_ provider: NSItemProvider) async -> String? {
        await withCheckedContinuation { continuation in
            _ = provider.loadObject(ofClass: NSString.self) { object, _ in
                continuation.resume(returning: (object as? NSString).map { $0 as String })
            }
        }
    }
}
